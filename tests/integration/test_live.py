"""Live commands and telemetry of decision D6: message checks and runtime behaviour."""

from __future__ import annotations

import asyncio
import base64
from copy import deepcopy
import importlib.util
import json
from pathlib import Path
import sys
import types
import unittest
from unittest.mock import patch

# Imported before the stubbed load, see test_session_takeover.
from cryptography.hazmat.primitives import serialization  # noqa: F401
from cryptography.hazmat.primitives.asymmetric import x25519  # noqa: F401
import noise.connection  # noqa: F401

ROOT = Path(__file__).resolve().parents[2]
INTEGRATION = ROOT / "custom_components" / "njorun"
FIXTURE = json.loads((ROOT / "protocol/fixtures/live-messages.json").read_text())
PACKAGE_NAME = "njorun_live_test"
package = types.ModuleType(PACKAGE_NAME)
package.__path__ = [str(INTEGRATION)]
sys.modules[PACKAGE_NAME] = package


class _Store:
    def __class_getitem__(cls, _item):
        return cls


ha = types.ModuleType("homeassistant")
ha.__path__ = []
config_entries = types.ModuleType("homeassistant.config_entries")
config_entries.ConfigEntry = type("ConfigEntry", (), {})
core = types.ModuleType("homeassistant.core")
core.HomeAssistant = type("HomeAssistant", (), {})
storage = types.ModuleType("homeassistant.helpers.storage")
storage.Store = _Store
helpers = types.ModuleType("homeassistant.helpers")
helpers.__path__ = []
stub_modules = {
    "homeassistant": ha,
    "homeassistant.config_entries": config_entries,
    "homeassistant.core": core,
    "homeassistant.helpers": helpers,
    "homeassistant.helpers.storage": storage,
}
spec = importlib.util.spec_from_file_location(f"{PACKAGE_NAME}.runtime", INTEGRATION / "runtime.py")
runtime_module = importlib.util.module_from_spec(spec)
with patch.dict(sys.modules, stub_modules):
    sys.modules[spec.name] = runtime_module
    spec.loader.exec_module(runtime_module)
live = sys.modules.get(f"{PACKAGE_NAME}.live") or importlib.import_module(f"{PACKAGE_NAME}.live")
model = sys.modules.get(f"{PACKAGE_NAME}.model") or importlib.import_module(f"{PACKAGE_NAME}.model")

Configuration = runtime_module.Configuration
NjorunRuntime = runtime_module.NjorunRuntime
RuntimeError = runtime_module.RuntimeError

LIVE_CAPABILITIES = {
    "orientations": ["portrait"],
    "brightness": True,
    "live": ["wake", "backlight", "telemetry"],
}


class _Noise:
    """Stub cipher: decrypt returns the next queued payload, encrypt records the plaintext."""

    def __init__(self):
        self.incoming = None
        self.sent = []

    def decrypt(self, _frame):
        return json.dumps(self.incoming).encode()

    def encrypt(self, plaintext):
        self.sent.append(json.loads(plaintext))
        return b"sealed"


class _Connection:
    def __init__(self):
        self.user = types.SimpleNamespace(id="user")
        self.subscriptions = {}
        self.events = []

    def send_event(self, subscription_id, event):
        self.events.append((subscription_id, event))


class _MemoryStore:
    async def async_save(self, _data):
        pass


def _runtime(capabilities):
    config = Configuration()
    config.add_profile("home", "Home", {}, 0)
    config.add_tablet("t1", "Kitchen", "home", capabilities, config.revision)
    runtime = NjorunRuntime.__new__(NjorunRuntime)
    runtime.configuration = config
    runtime.bindings = {"b1": {"tablet_id": "t1", "status": "active"}}
    runtime.applied = {}
    runtime.sessions = {}
    runtime.session_by_tablet = {}
    runtime.pending_operations = {}
    runtime.installs = {}
    runtime.completed_operations = set()
    runtime.announcements = {}
    runtime.entity_adders = {}
    runtime.entities = {}
    runtime._registered_platforms = set()
    runtime._entities_created = set()
    runtime._lock = asyncio.Lock()
    runtime._closed = False
    runtime.store = _MemoryStore()
    runtime.hass = types.SimpleNamespace(loop=asyncio.get_running_loop())
    return runtime


async def _establish(runtime, subscription_id=5):
    connection = _Connection()
    started = runtime.async_start_session(connection, "b1")
    session = runtime.sessions[started["session_id"]]
    session.phase = "await_confirm"
    noise = _Noise()
    noise.incoming = {
        "type": "confirm",
        "protocol": 1,
        "binding_id": session.binding_id,
        "session_id": session.session_id,
    }
    session.noise = noise
    frame = base64.b64encode(b"frame").decode()
    await runtime.async_confirm_and_subscribe(
        connection, session.session_id, frame, subscription_id
    )
    return session, connection, noise


async def _receive(runtime, session, connection, noise, payload):
    noise.incoming = payload
    await runtime.async_receive_ack(
        connection, session.session_id, base64.b64encode(b"frame").decode()
    )


TELEMETRY = FIXTURE["telemetry"]["valid"][0]


class MessageTests(unittest.TestCase):
    def test_fixture_command_results(self):
        for message in FIXTURE["command_result"]["valid"]:
            self.assertIsNotNone(live.parse_command_result(message), message)
        for message in FIXTURE["command_result"]["invalid"]:
            self.assertIsNone(live.parse_command_result(message), message)

    def test_fixture_telemetry(self):
        for message in FIXTURE["telemetry"]["valid"]:
            parsed = live.parse_telemetry(message)
            self.assertIsNotNone(parsed, message)
            self.assertEqual({"type": "telemetry", **parsed}, message)
        for message in FIXTURE["telemetry"]["invalid"]:
            self.assertIsNone(live.parse_telemetry(message), message)

    def test_backlight_command_values_follow_the_fixture(self):
        for message in FIXTURE["command"]["valid"]:
            if message["name"] == "backlight":
                self.assertTrue(live.valid_backlight(message["value"], partial=True), message)
        for message in FIXTURE["command"]["invalid"]:
            if message["name"] == "backlight" and message["command_id"] != "c6":
                self.assertFalse(live.valid_backlight(message["value"], partial=True), message)
        self.assertFalse(live.valid_backlight(None, partial=True))
        self.assertFalse(live.valid_backlight({"on": True}))

    def test_constants_match_the_fixture(self):
        self.assertEqual(list(live.COMMAND_REASONS), FIXTURE["command_reasons"])
        self.assertEqual(list(live.RESET_REASONS), FIXTURE["reset_reasons"])
        self.assertEqual(live.COMMAND_TTL, FIXTURE["limits"]["command_ttl_seconds"])
        self.assertEqual(
            live.MAX_FIRMWARE_VERSION_CHARS, FIXTURE["limits"]["firmware_version_chars"]
        )

    def test_capabilities_carry_an_optional_live_list(self):
        base = {"orientations": ["portrait"], "brightness": True}
        self.assertEqual(model.validate_capabilities(base), base)
        camera = {**base, "camera_motion": True}
        self.assertEqual(model.validate_capabilities(camera), camera)
        self.assertEqual(model.validate_capabilities(LIVE_CAPABILITIES), LIVE_CAPABILITIES)
        for live_list in (["reboot"], ["wake", "wake"], "wake", [1]):
            with self.assertRaises(model.ConfigurationError):
                model.validate_capabilities({**base, "live": live_list})
        with self.assertRaises(model.ConfigurationError):
            model.validate_capabilities({**base, "extra": 1})
        for invalid in (None, 0, "true"):
            with self.assertRaises(model.ConfigurationError):
                model.validate_capabilities({**base, "camera_motion": invalid})


class LiveRuntimeTests(unittest.IsolatedAsyncioTestCase):
    async def test_offered_parts_come_from_the_reported_capabilities(self):
        session, *_ = await _establish(_runtime(LIVE_CAPABILITIES))
        self.assertEqual(session.offered, ("wake", "backlight", "telemetry"))
        older, *_ = await _establish(_runtime({"orientations": ["portrait"], "brightness": True}))
        self.assertEqual(older.offered, ())

    async def test_a_command_is_sent_and_resolved_by_its_result(self):
        runtime = _runtime(LIVE_CAPABILITIES)
        session, connection, noise = await _establish(runtime)
        send = asyncio.create_task(runtime.async_send_command("t1", "wake"))
        await asyncio.sleep(0)
        command = noise.sent[-1]
        self.assertEqual(connection.events[-1][0], 5)
        self.assertEqual(set(command), {"type", "command_id", "name", "value", "expires_at"})
        self.assertEqual((command["type"], command["name"], command["value"]), ("command", "wake", None))
        self.assertLessEqual(command["expires_at"] - int(__import__("time").time()), 10)
        await _receive(runtime, session, connection, noise, {
            "type": "command_result", "command_id": command["command_id"], "ok": True, "reason": None,
        })
        await send
        self.assertEqual(session.pending_commands, {})

    async def test_a_refusal_by_the_tablet_is_an_error(self):
        runtime = _runtime(LIVE_CAPABILITIES)
        session, connection, noise = await _establish(runtime)
        send = asyncio.create_task(runtime.async_send_command("t1", "wake"))
        await asyncio.sleep(0)
        await _receive(runtime, session, connection, noise, {
            "type": "command_result", "command_id": noise.sent[-1]["command_id"],
            "ok": False, "reason": "expired",
        })
        with self.assertRaisesRegex(RuntimeError, "expired"):
            await send

    async def test_commands_need_a_live_session_and_an_offered_part(self):
        runtime = _runtime(LIVE_CAPABILITIES)
        with self.assertRaisesRegex(RuntimeError, "not connected"):
            await runtime.async_send_command("t1", "wake")
        session, connection, noise = await _establish(runtime)
        session.offered = ("telemetry",)
        with self.assertRaisesRegex(RuntimeError, "does not support"):
            await runtime.async_send_command("t1", "wake")
        session.offered = ("wake", "backlight", "telemetry")
        with self.assertRaisesRegex(RuntimeError, "Invalid command value"):
            await runtime.async_send_command("t1", "backlight", {"on": True, "brightness": 101})
        with self.assertRaisesRegex(RuntimeError, "Invalid command value"):
            await runtime.async_send_command("t1", "wake", {"on": True})
        self.assertEqual(noise.sent, [], "nothing was sent")
        self.assertEqual(connection.events, [])

    async def test_pending_commands_are_bounded(self):
        runtime = _runtime(LIVE_CAPABILITIES)
        session, *_ = await _establish(runtime)
        tasks = [
            asyncio.create_task(runtime.async_send_command("t1", "wake"))
            for _ in range(live.MAX_PENDING_COMMANDS)
        ]
        await asyncio.sleep(0)
        with self.assertRaisesRegex(RuntimeError, "Too many"):
            await runtime.async_send_command("t1", "wake")
        runtime._close_session(session.session_id)
        for task in tasks:
            with self.assertRaisesRegex(RuntimeError, "ended"):
                await task

    async def test_an_unanswered_command_times_out_and_is_not_kept(self):
        runtime = _runtime(LIVE_CAPABILITIES)
        session, *_ = await _establish(runtime)
        with patch.object(runtime_module, "RESULT_TIMEOUT", 0.05):
            with self.assertRaisesRegex(RuntimeError, "in time"):
                await runtime.async_send_command("t1", "wake")
        self.assertEqual(session.pending_commands, {})

    async def test_a_closed_session_fails_its_commands_and_nothing_is_replayed(self):
        runtime = _runtime(LIVE_CAPABILITIES)
        session, connection, noise = await _establish(runtime)
        send = asyncio.create_task(runtime.async_send_command("t1", "wake"))
        await asyncio.sleep(0)
        runtime._close_session(session.session_id)
        with self.assertRaisesRegex(RuntimeError, "ended"):
            await send
        new_session, new_connection, new_noise = await _establish(runtime, 9)
        self.assertEqual(new_noise.sent, [])
        self.assertEqual(new_connection.events, [])
        self.assertEqual(new_session.pending_commands, {})

    async def test_an_unknown_or_repeated_result_ends_the_session(self):
        runtime = _runtime(LIVE_CAPABILITIES)
        session, connection, noise = await _establish(runtime)
        with self.assertRaises(RuntimeError):
            await _receive(runtime, session, connection, noise, {
                "type": "command_result", "command_id": "never-sent", "ok": True, "reason": None,
            })
        self.assertEqual(session.phase, "closed")

    async def test_telemetry_is_kept_for_the_session_only(self):
        runtime = _runtime(LIVE_CAPABILITIES)
        session, connection, noise = await _establish(runtime)
        self.assertIsNone(runtime.telemetry("t1"))
        await _receive(runtime, session, connection, noise, deepcopy(TELEMETRY))
        self.assertEqual(runtime.telemetry("t1")["uptime_s"], 61)
        self.assertEqual(runtime.telemetry("t1")["backlight"], {"on": True, "brightness": 80})
        runtime._close_session(session.session_id)
        self.assertIsNone(runtime.telemetry("t1"), "unavailable without a session")

    async def test_bad_or_unoffered_telemetry_ends_the_session(self):
        runtime = _runtime(LIVE_CAPABILITIES)
        session, connection, noise = await _establish(runtime)
        bad = {**TELEMETRY, "ssid": "home"}
        with self.assertRaises(RuntimeError):
            await _receive(runtime, session, connection, noise, bad)
        self.assertEqual(session.phase, "closed")
        older = _runtime({"orientations": ["portrait"], "brightness": True})
        session, connection, noise = await _establish(older)
        with self.assertRaises(RuntimeError):
            await _receive(older, session, connection, noise, deepcopy(TELEMETRY))
        self.assertEqual(session.phase, "closed")

    async def test_a_backlight_command_changes_no_setting_or_revision(self):
        runtime = _runtime(LIVE_CAPABILITIES)
        session, connection, noise = await _establish(runtime)
        await _receive(runtime, session, connection, noise, deepcopy(TELEMETRY))
        revision = runtime.configuration.revision
        settings = deepcopy(runtime.configuration.snapshot("t1").settings)
        send = asyncio.create_task(
            runtime.async_send_command("t1", "backlight", {"on": False, "brightness": 20})
        )
        await asyncio.sleep(0)
        await _receive(runtime, session, connection, noise, {
            "type": "command_result", "command_id": noise.sent[-1]["command_id"],
            "ok": True, "reason": None,
        })
        await send
        self.assertEqual(runtime.telemetry("t1")["backlight"], {"on": False, "brightness": 20})
        self.assertEqual(runtime.configuration.revision, revision)
        self.assertEqual(runtime.configuration.snapshot("t1").settings, settings)
        self.assertEqual(runtime.applied, {})

    async def test_the_availability_of_a_part_follows_the_session(self):
        runtime = _runtime(LIVE_CAPABILITIES)
        self.assertFalse(runtime.live_part_offered("t1", "wake"))
        session, *_ = await _establish(runtime)
        self.assertTrue(runtime.live_part_offered("t1", "wake"))
        runtime._close_session(session.session_id)
        self.assertFalse(runtime.live_part_offered("t1", "wake"))


if __name__ == "__main__":
    unittest.main()
