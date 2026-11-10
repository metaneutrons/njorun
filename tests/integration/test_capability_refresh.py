"""Capability refresh of decision D8: the confirmation carries the tablet's capabilities."""

from __future__ import annotations

import base64
import json
from pathlib import Path
import sys
import types
import unittest
from unittest.mock import MagicMock, patch

import test_live as base

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = json.loads((ROOT / "protocol/fixtures/confirm-messages.json").read_text())

OLD = {"orientations": ["portrait", "landscape"], "brightness": True}
NEW = {**OLD, "live": ["wake", "backlight", "telemetry"]}


class _SavingStore:
    def __init__(self):
        self.saved = []

    async def async_save(self, data):
        self.saved.append(data)


def _runtime(stored):
    runtime = base._runtime(stored)
    runtime.store = _SavingStore()
    runtime._capability_issues = set()
    runtime.async_add_tablet_entities = MagicMock()
    runtime._raise_capability_issue = MagicMock(
        side_effect=lambda tablet_id, _reason: runtime._capability_issues.add(tablet_id)
    )
    return runtime


async def _confirm(runtime, capabilities=..., subscription_id=5):
    connection = base._Connection()
    started = runtime.async_start_session(connection, "b1")
    session = runtime.sessions[started["session_id"]]
    session.phase = "await_confirm"
    noise = base._Noise()
    noise.incoming = {
        "type": "confirm",
        "protocol": 1,
        "binding_id": session.binding_id,
        "session_id": session.session_id,
    }
    if capabilities is not ...:
        noise.incoming["capabilities"] = capabilities
    session.noise = noise
    await runtime.async_confirm_and_subscribe(
        connection,
        session.session_id,
        base64.b64encode(b"frame").decode(),
        subscription_id,
    )
    return session


class CapabilityRefreshTests(unittest.IsolatedAsyncioTestCase):
    async def test_new_capabilities_are_stored_before_the_parts_are_offered(self):
        runtime = _runtime(OLD)
        before = runtime.configuration.revision
        session = await _confirm(runtime, NEW)
        tablet = runtime.configuration.tablets["t1"]
        self.assertEqual(tablet["capabilities"], NEW)
        self.assertEqual(runtime.configuration.revision, before + 1)
        self.assertEqual(tablet["revision"], before + 1)
        self.assertEqual(session.offered, ("wake", "backlight", "telemetry"))
        self.assertEqual(session.phase, "active")
        saved = runtime.store.saved[-1]["configuration"]
        self.assertEqual(saved["tablets"]["t1"]["capabilities"], NEW)
        self.assertEqual(saved["revision"], before + 1)
        runtime.async_add_tablet_entities.assert_called_with("t1")

    async def test_an_unknown_part_is_never_offered(self):
        # HA implements only the parts it knows; validation already rejects anything else.
        runtime = _runtime(OLD)
        session = await _confirm(runtime, {**OLD, "live": ["wake"]})
        self.assertEqual(session.offered, ("wake",))

    async def test_equal_capabilities_change_nothing(self):
        runtime = _runtime(NEW)
        before = runtime.configuration.revision
        await _confirm(runtime, NEW)
        self.assertEqual(runtime.configuration.revision, before)
        self.assertEqual(runtime.store.saved, [])
        runtime.async_add_tablet_entities.assert_not_called()

    async def test_a_missing_field_keeps_the_stored_capabilities(self):
        runtime = _runtime(NEW)
        before = runtime.configuration.revision
        session = await _confirm(runtime)
        self.assertEqual(runtime.configuration.tablets["t1"]["capabilities"], NEW)
        self.assertEqual(runtime.configuration.revision, before)
        self.assertEqual(session.offered, ("wake", "backlight", "telemetry"))

    async def test_capabilities_that_break_a_setting_are_refused_not_fatal(self):
        runtime = _runtime(OLD)  # the default profile selects portrait
        before = runtime.configuration.revision
        session = await _confirm(runtime, {"orientations": ["landscape"], "brightness": True})
        self.assertEqual(session.phase, "active")
        self.assertEqual(runtime.configuration.tablets["t1"]["capabilities"], OLD)
        self.assertEqual(runtime.configuration.revision, before)
        self.assertEqual(runtime.store.saved, [])
        runtime._raise_capability_issue.assert_called_once()
        self.assertIn("t1", runtime._capability_issues)

    async def test_an_override_counts_too(self):
        runtime = _runtime({"orientations": ["portrait", "landscape"], "brightness": True})
        config = runtime.configuration
        config.set_overrides("t1", {"orientation": "landscape"}, config.revision)
        await _confirm(runtime, {"orientations": ["portrait"], "brightness": True})
        self.assertEqual(
            runtime.configuration.tablets["t1"]["capabilities"]["orientations"],
            ["portrait", "landscape"],
        )

    async def test_a_later_fitting_report_clears_the_issue(self):
        runtime = _runtime(OLD)
        await _confirm(runtime, {"orientations": ["landscape"], "brightness": True})
        cleared = MagicMock()
        runtime._clear_capability_issue = cleared
        await _confirm(runtime, NEW, subscription_id=6)
        cleared.assert_called_with("t1")
        self.assertEqual(runtime.configuration.tablets["t1"]["capabilities"], NEW)

    async def test_fixture_capabilities(self):
        for value in FIXTURE["confirm"]["capabilities"]["valid"]:
            runtime = _runtime(OLD)
            runtime.configuration.tablets["t1"]["capabilities"] = value
            await _confirm(runtime, value)
            self.assertEqual(runtime.configuration.tablets["t1"]["capabilities"], value)
        for value in FIXTURE["confirm"]["capabilities"]["invalid"]:
            runtime = _runtime(OLD)
            with self.assertRaises(base.RuntimeError, msg=value):
                await _confirm(runtime, value)
            self.assertEqual(runtime.sessions, {}, value)
            self.assertEqual(runtime.configuration.tablets["t1"]["capabilities"], OLD)

    async def test_other_extra_fields_still_close_the_session(self):
        runtime = _runtime(OLD)
        connection = base._Connection()
        started = runtime.async_start_session(connection, "b1")
        session = runtime.sessions[started["session_id"]]
        session.phase = "await_confirm"
        noise = base._Noise()
        noise.incoming = {
            "type": "confirm",
            "protocol": 1,
            "binding_id": session.binding_id,
            "session_id": session.session_id,
            "capabilities": NEW,
            "extra": 1,
        }
        session.noise = noise
        with self.assertRaises(base.RuntimeError):
            await runtime.async_confirm_and_subscribe(
                connection, session.session_id, base64.b64encode(b"f").decode(), 5
            )
        self.assertEqual(runtime.configuration.tablets["t1"]["capabilities"], OLD)

    async def test_enrollment_with_other_capabilities_is_still_refused(self):
        runtime = _runtime(OLD)
        runtime.bindings = {}
        with self.assertRaises(base.RuntimeError):
            await runtime.async_enroll(
                tablet_id="t1",
                name="Kitchen",
                profile_id="home",
                tablet_public_key=base64.b64encode(bytes([7]) * 32).decode(),
                capabilities=NEW,
                expected_revision=runtime.configuration.revision,
            )


class EntityRemovalTests(unittest.IsolatedAsyncioTestCase):
    async def test_entities_without_their_capability_are_removed(self):
        runtime = _runtime(NEW)
        runtime.configuration.tablets["t1"]["capabilities"] = {**OLD, "live": ["wake"]}
        keep = MagicMock(entity_id="button.wake")
        keep.supported_by.return_value = True
        drop = MagicMock(entity_id="light.backlight")
        drop.supported_by.return_value = False
        runtime.entities = {"t1": {keep, drop}}
        registry = MagicMock()
        er = types.ModuleType("homeassistant.helpers.entity_registry")
        er.async_get = lambda _hass: registry
        helpers = types.ModuleType("homeassistant.helpers")
        helpers.entity_registry = er
        modules = {
            "homeassistant": types.ModuleType("homeassistant"),
            "homeassistant.helpers": helpers,
            "homeassistant.helpers.entity_registry": er,
        }
        with patch.dict(sys.modules, modules):
            runtime._remove_unsupported_entities("t1")
        registry.async_remove.assert_called_once_with("light.backlight")


if __name__ == "__main__":
    unittest.main()
