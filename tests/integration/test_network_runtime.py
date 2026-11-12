"""Network settings over the management session (plan 006 slice 4): the runtime state machine."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
import sys
import time
import unittest
from unittest.mock import MagicMock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_live as base  # noqa: E402  (reuses the stubbed runtime module and helpers)

RuntimeError = base.RuntimeError
runtime_module = base.runtime_module
# The runtime was loaded under stubbed modules; its own references are the ones whose classes
# and sentinels match, a fresh import would give different objects.
NetworkSettings = runtime_module.NetworkSettings
network = base.importlib.import_module(f"{base.PACKAGE_NAME}.network")
FIXTURE = json.loads((base.ROOT / "protocol/fixtures/network-messages.json").read_text())

CAPABILITIES = {"orientations": ["portrait"], "brightness": True, "network": ["wifi"]}
WIFI = {"ssid": "Home", "password": "correct horse"}
STATIC = {
    "mode": "static",
    "address": "192.168.1.50",
    "prefix": 24,
    "gateway": "192.168.1.1",
    "dns": ["192.168.1.1"],
}
STATE = FIXTURE["results"]["state"]


class _SavingStore:
    def __init__(self):
        self.saved = []

    async def async_save(self, data):
        self.saved.append(data)


def _runtime(capabilities=CAPABILITIES):
    runtime = base._runtime(capabilities)
    runtime.store = _SavingStore()
    runtime.network_settings = NetworkSettings()
    runtime._raise_network_issue = MagicMock()
    runtime._clear_network_issue = MagicMock()
    return runtime


def result(revision, outcome="applied", detail=None, operation_id="op"):
    payload = {
        "type": "network_result",
        "operation_id": operation_id,
        "revision": revision,
        "result": outcome,
        "state": STATE,
    }
    if outcome == "applied":
        payload["proof"] = detail or "session"
    else:
        payload["reason"] = detail or "no_home_assistant"
    return payload


def network_messages(noise):
    return [message for message in noise.sent if message.get("type") == "network"]


class CapabilityTests(unittest.TestCase):
    def test_the_network_capability_lists_interfaces(self):
        plain = {"orientations": ["portrait"], "brightness": True}
        for interfaces in (["wifi"], ["wifi", "ethernet"], ["ethernet"]):
            capabilities = {**plain, "network": interfaces}
            self.assertEqual(base.model.validate_capabilities(capabilities), capabilities)
        for interfaces in ([], ["bluetooth"], ["wifi", "wifi"], "wifi", [1]):
            with self.assertRaises(base.model.ConfigurationError, msg=interfaces):
                base.model.validate_capabilities({**plain, "network": interfaces})


class SendTests(unittest.IsolatedAsyncioTestCase):
    async def test_a_profile_change_is_sent_to_a_connected_tablet(self):
        runtime = _runtime()
        session, connection, noise = await base._establish(runtime)
        self.assertTrue(session.network)
        await runtime.async_set_profile_network("home", WIFI)
        message = network_messages(noise)[0]
        self.assertEqual(message["revision"], 1)
        self.assertEqual(message["wifi"], WIFI)
        self.assertEqual(network.check_network_message(
            message, now=int(time.time()), last_revision=0, interfaces=["wifi"]), {"wifi": WIFI})
        self.assertEqual(len(connection.events), 1)
        self.assertEqual(runtime.network_status("t1")["pending"], True)

    async def test_the_static_setup_of_a_tablet_goes_with_the_profile_credentials(self):
        runtime = _runtime()
        session, _, noise = await base._establish(runtime)
        await runtime.async_set_profile_network("home", WIFI)
        await base._receive(runtime, session, _, noise, result(1))
        await runtime.async_set_tablet_network("t1", ip={"wifi": STATIC})
        message = network_messages(noise)[-1]
        self.assertEqual(message["revision"], 2)
        self.assertEqual(message["wifi"], {**WIFI, "ip": STATIC})

    async def test_a_static_address_alone_sends_nothing(self):
        runtime = _runtime()
        _, _, noise = await base._establish(runtime)
        await runtime.async_set_tablet_network("t1", ip={"wifi": STATIC})
        self.assertEqual(network_messages(noise), [])

    async def test_a_tablet_own_network_replaces_the_profile_credentials(self):
        runtime = _runtime()
        _, _, noise = await base._establish(runtime)
        await runtime.async_set_profile_network("home", WIFI)
        # the first message is unanswered, so the change waits; the answer sends it
        await runtime.async_set_tablet_network("t1", wifi={"ssid": "Kitchen"})
        self.assertEqual(len(network_messages(noise)), 1)

    async def test_a_tablet_without_the_capability_is_sent_nothing(self):
        runtime = _runtime({"orientations": ["portrait"], "brightness": True})
        session, _, noise = await base._establish(runtime)
        self.assertFalse(session.network)
        await runtime.async_set_profile_network("home", WIFI)
        self.assertEqual(network_messages(noise), [])

    async def test_a_disconnected_tablet_gets_it_when_the_session_opens(self):
        runtime = _runtime()
        await runtime.async_set_profile_network("home", WIFI)
        session, _, noise = await base._establish(runtime)
        self.assertEqual(network_messages(noise), [])
        await runtime.async_send_initial_configuration(session)
        self.assertEqual(network_messages(noise)[0]["wifi"], WIFI)

    async def test_nothing_is_sent_while_a_message_is_unanswered_and_the_newest_follows(self):
        runtime = _runtime()
        session, connection, noise = await base._establish(runtime)
        await runtime.async_set_profile_network("home", WIFI)
        await runtime.async_set_profile_network("home", {"ssid": "Home", "password": "another one"})
        self.assertEqual([m["revision"] for m in network_messages(noise)], [1])
        await base._receive(runtime, session, connection, noise, result(1))
        sent = network_messages(noise)
        self.assertEqual([m["revision"] for m in sent], [1, 2])
        self.assertEqual(sent[1]["wifi"]["password"], "another one")

    async def test_an_unanswered_message_is_not_waited_for_forever(self):
        runtime = _runtime()
        session, _, noise = await base._establish(runtime)
        await runtime.async_set_profile_network("home", WIFI)
        operation, revision, _ = runtime._network_flights["t1"]
        runtime._network_flights["t1"] = (operation, revision, time.monotonic() - 1000)
        await runtime.async_send_initial_configuration(session)
        self.assertEqual([m["revision"] for m in network_messages(noise)], [1, 1])


class ResultTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.runtime = _runtime()
        self.session, self.connection, self.noise = await base._establish(self.runtime)
        await self.runtime.async_set_profile_network("home", WIFI)

    async def receive(self, payload, session=None):
        await base._receive(
            self.runtime, session or self.session, self.connection, self.noise, payload)

    async def test_applied_is_stored_and_not_sent_again(self):
        await self.receive(result(1, detail="session"))
        stored = self.runtime.network_settings.result("t1")
        self.assertEqual((stored["result"], stored["proof"]), ("applied", "session"))
        self.assertEqual(stored["state"], STATE)
        self.runtime._clear_network_issue.assert_called_with("t1")
        self.assertFalse(self.runtime.network_status("t1")["pending"])
        await self.runtime.async_send_initial_configuration(self.session)
        self.assertEqual(len(network_messages(self.noise)), 1)

    async def test_reverted_is_remembered_and_the_revision_is_not_sent_again(self):
        await self.receive(result(1, "reverted", "no_home_assistant"))
        stored = self.runtime.network_settings.result("t1")
        self.assertEqual((stored["result"], stored["reason"]), ("reverted", "no_home_assistant"))
        self.runtime._raise_network_issue.assert_called_once_with("t1", "no_home_assistant")
        await self.runtime.async_send_initial_configuration(self.session)
        await self.runtime.async_send_initial_configuration(self.session)
        self.assertEqual(len(network_messages(self.noise)), 1)

    async def test_changing_the_values_after_a_revert_sends_a_new_revision(self):
        await self.receive(result(1, "reverted", "no_association"))
        await self.runtime.async_set_profile_network(
            "home", {"ssid": "Home", "password": "the right one"})
        sent = network_messages(self.noise)
        self.assertEqual([m["revision"] for m in sent], [1, 2])
        self.runtime._clear_network_issue.assert_called_with("t1")

    async def test_the_result_may_come_on_a_new_session_after_the_trial(self):
        # a Wi-Fi trial breaks the session; the tablet answers on the next one
        second, connection, noise = await base._establish(self.runtime, subscription_id=6)
        await base._receive(self.runtime, second, connection, noise, result(1))
        self.assertEqual(self.runtime.network_settings.result("t1")["result"], "applied")
        self.assertFalse(self.runtime.network_status("t1")["pending"])

    async def test_a_result_for_a_revision_ha_did_not_issue_ends_the_session(self):
        with self.assertRaises(RuntimeError):
            await self.receive(result(7))
        self.assertNotIn(self.session.session_id, self.runtime.sessions)
        self.assertIsNone(self.runtime.network_settings.result("t1"))

    async def test_a_result_for_an_older_revision_changes_nothing(self):
        await self.receive(result(1))
        await self.runtime.async_set_profile_network(
            "home", {"ssid": "Home", "password": "newer password"})
        before = self.runtime.network_settings.dump()
        await self.receive(result(1, "reverted", "no_address", operation_id="late"))
        self.assertEqual(self.runtime.network_settings.dump(), before)

    async def test_an_invalid_result_ends_the_session(self):
        bad = result(1)
        bad["extra"] = 1
        with self.assertRaises(RuntimeError):
            await self.receive(bad)
        self.assertNotIn(self.session.session_id, self.runtime.sessions)

    async def test_a_result_without_the_negotiated_capability_ends_the_session(self):
        self.session.network = False
        with self.assertRaises(RuntimeError):
            await self.receive(result(1))
        self.assertNotIn(self.session.session_id, self.runtime.sessions)


class SettingsTests(unittest.IsolatedAsyncioTestCase):
    async def test_a_profile_cannot_carry_an_ip_setup(self):
        runtime = _runtime()
        with self.assertRaises(RuntimeError):
            await runtime.async_set_profile_network("home", {**WIFI, "ip": STATIC})
        self.assertEqual(runtime.network_settings.profiles, {})

    async def test_invalid_values_are_refused_and_nothing_changes(self):
        runtime = _runtime()
        for wifi in ({"ssid": ""}, {"ssid": "Home", "password": "short"}, {"password": "x"}):
            with self.assertRaises(RuntimeError, msg=wifi):
                await runtime.async_set_profile_network("home", wifi)
        with self.assertRaises(RuntimeError):
            await runtime.async_set_tablet_network(
                "t1", ip={"wifi": {**STATIC, "gateway": "10.0.0.1"}})
        with self.assertRaises(RuntimeError):
            await runtime.async_set_tablet_network("t1", ip={"ethernet": {"mode": "dhcp"}})
        self.assertTrue(runtime.network_settings.is_empty())
        with self.assertRaises(RuntimeError):
            await runtime.async_set_profile_network("nobody", WIFI)
        with self.assertRaises(RuntimeError):
            await runtime.async_set_tablet_network("nobody", wifi=WIFI)

    async def test_an_error_message_never_names_the_password(self):
        runtime = _runtime()
        with self.assertRaises(RuntimeError) as caught:
            await runtime.async_set_profile_network("home", {"ssid": "Home", "password": "short"})
        self.assertNotIn("short", str(caught.exception))

    async def test_the_settings_are_stored_with_the_state_and_restored(self):
        runtime = _runtime()
        await runtime.async_set_profile_network("home", WIFI)
        await runtime.async_set_tablet_network("t1", ip={"wifi": STATIC})
        data = json.loads(json.dumps(runtime._stored_data()))
        self.assertEqual(data["network"]["profiles"]["home"]["wifi"], WIFI)
        data["bindings"] = {}
        again = _runtime()
        again._restore(data)
        self.assertEqual(again.network_settings.dump(), runtime.network_settings.dump())

    async def test_storage_without_network_settings_keeps_its_old_shape(self):
        runtime = _runtime()
        self.assertNotIn("network", runtime._stored_data())
        data = json.loads(json.dumps(runtime._stored_data()))
        data["bindings"] = {}
        again = _runtime()
        again._restore(data)
        self.assertTrue(again.network_settings.is_empty())

    async def test_corrupt_stored_network_settings_are_refused(self):
        runtime = _runtime()
        await runtime.async_set_profile_network("home", WIFI)
        data = json.loads(json.dumps(runtime._stored_data()))
        data["bindings"] = {}
        data["network"]["profiles"]["home"]["wifi"] = {"ssid": ""}
        with self.assertRaises(RuntimeError):
            _runtime()._restore(data)
        data["network"] = {"profiles": {}}
        with self.assertRaises(RuntimeError):
            _runtime()._restore(data)

    async def test_removing_a_tablet_forgets_its_network_state(self):
        runtime = _runtime()
        await runtime.async_set_profile_network("home", WIFI)
        await runtime.async_set_tablet_network("t1", ip={"wifi": STATIC})
        self.assertIn("t1", runtime.network_settings.tablets)
        runtime._close_tablet_sessions = MagicMock()
        runtime._async_remove_access = MagicMock(side_effect=lambda _users: asyncio.sleep(0))
        await runtime.async_remove_tablet("t1")
        self.assertNotIn("t1", runtime.network_settings.tablets)
        self.assertNotIn("t1", runtime.network_settings.state)


if __name__ == "__main__":
    unittest.main()
