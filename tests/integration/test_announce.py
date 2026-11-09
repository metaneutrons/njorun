"""A tablet announces itself, an administrator approves it after comparing the code."""

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

# Imported before the stubbed load: patch.dict drops modules first imported inside it, and a
# second copy of cryptography would reject the first copy's serialization classes.
from cryptography.hazmat.primitives import serialization  # noqa: F401
from cryptography.hazmat.primitives.asymmetric import x25519  # noqa: F401
import noise.connection  # noqa: F401


ROOT = Path(__file__).resolve().parents[2]
INTEGRATION = ROOT / "custom_components" / "njorun"
PACKAGE_NAME = "njorun_announce_test"
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
spec = importlib.util.spec_from_file_location(
    f"{PACKAGE_NAME}.runtime", INTEGRATION / "runtime.py"
)
runtime_module = importlib.util.module_from_spec(spec)
with patch.dict(sys.modules, stub_modules):
    sys.modules[spec.name] = runtime_module
    spec.loader.exec_module(runtime_module)

Configuration = runtime_module.Configuration
NjorunRuntime = runtime_module.NjorunRuntime
RuntimeError = runtime_module.RuntimeError
comparison_code = runtime_module.comparison_code

CAPABILITIES = {"orientations": ["portrait", "landscape"], "brightness": True}
TABLET_KEY = base64.b64encode(bytes(range(32))).decode()
OTHER_KEY = base64.b64encode(bytes(range(1, 33))).decode()


class _MemoryStore:
    def __init__(self):
        self.data = None

    async def async_save(self, data):
        self.data = deepcopy(data)


def _runtime():
    config = Configuration()
    config.add_profile("home", "Home", {}, 0)
    runtime = NjorunRuntime.__new__(NjorunRuntime)
    runtime.configuration = config
    runtime.bindings = {}
    runtime.applied = {}
    runtime.sessions = {}
    runtime.session_by_tablet = {}
    runtime.pending_operations = {}
    runtime.completed_operations = set()
    runtime.announcements = {}
    runtime.entity_adders = {}
    runtime.entities = {}
    runtime._registered_platforms = set()
    runtime._entities_created = set()
    runtime._lock = asyncio.Lock()
    runtime._closed = False
    runtime.store = _MemoryStore()
    return runtime


SIGNED_IN = types.SimpleNamespace(user=types.SimpleNamespace(id="user"))


def _announce(runtime, tablet_id="njorun-1", key=TABLET_KEY, connection=SIGNED_IN, **changes):
    args = {
        "tablet_id": tablet_id,
        "name": "Kitchen",
        "tablet_public_key": key,
        "capabilities": CAPABILITIES,
    }
    args.update(changes)
    return runtime.async_announce(connection, **args)


class ComparisonCodeTests(unittest.TestCase):
    def test_matches_the_shared_fixture(self):
        fixture = json.loads((ROOT / "protocol/fixtures/comparison-code.json").read_text())
        for case in fixture["cases"]:
            key = base64.b64decode(case["tablet_public_key"])
            self.assertEqual(comparison_code(key), case["code"])


class AnnounceTests(unittest.IsolatedAsyncioTestCase):
    async def test_announced_tablet_waits_then_is_approved(self):
        runtime = _runtime()
        result = _announce(runtime)
        self.assertEqual(result["status"], "pending")
        self.assertEqual(result["code"], comparison_code(bytes(range(32))))
        self.assertEqual(
            runtime.pending_announcements(),
            [{"tablet_id": "njorun-1", "name": "Kitchen", "code": result["code"]}],
        )
        self.assertIsNone(runtime.store.data, "announcing persists nothing")

        enrolled = await runtime.async_approve(
            tablet_id="njorun-1",
            profile_id="home",
            expected_revision=runtime.configuration.revision,
        )
        self.assertEqual(enrolled["status"], "pending")
        self.assertEqual(runtime.pending_announcements(), [])
        self.assertIn("njorun-1", runtime.configuration.tablets)

        approved = _announce(runtime)
        self.assertEqual(approved["status"], "approved")
        self.assertEqual(approved["binding_id"], enrolled["binding_id"])
        self.assertEqual(approved["ha_public_key"], enrolled["ha_public_key"])

    async def test_another_key_under_a_paired_id_is_a_conflict(self):
        runtime = _runtime()
        _announce(runtime)
        await runtime.async_approve(
            tablet_id="njorun-1",
            profile_id="home",
            expected_revision=runtime.configuration.revision,
        )
        self.assertEqual(_announce(runtime, key=OTHER_KEY), {"status": "conflict"})

    async def test_revoked_binding_lets_the_tablet_wait_again(self):
        runtime = _runtime()
        _announce(runtime)
        enrolled = await runtime.async_approve(
            tablet_id="njorun-1",
            profile_id="home",
            expected_revision=runtime.configuration.revision,
        )
        runtime.bindings[enrolled["binding_id"]]["status"] = "revoked"
        self.assertEqual(_announce(runtime)["status"], "pending")

    async def test_approval_needs_a_live_announcement(self):
        runtime = _runtime()
        with self.assertRaises(RuntimeError):
            await runtime.async_approve(
                tablet_id="njorun-1", profile_id="home", expected_revision=0
            )
        _announce(runtime)
        runtime.announcements["njorun-1"]["expires_at"] = 0
        self.assertEqual(runtime.pending_announcements(), [])
        with self.assertRaises(RuntimeError):
            await runtime.async_approve(
                tablet_id="njorun-1",
                profile_id="home",
                expected_revision=runtime.configuration.revision,
            )

    def test_invalid_announcements_are_refused(self):
        runtime = _runtime()
        with self.assertRaises(RuntimeError):
            _announce(runtime, connection=types.SimpleNamespace())
        for changes in (
            {"tablet_id": ""},
            {"tablet_id": "x" * 129},
            {"name": " "},
            {"key": "AAAA"},
            {"capabilities": {"orientations": ["upside"], "brightness": True}},
        ):
            with self.subTest(changes=changes), self.assertRaises(RuntimeError):
                _announce(runtime, **changes)
        runtime._closed = True
        with self.assertRaises(RuntimeError):
            _announce(runtime)

    async def test_a_newly_waiting_tablet_is_offered_once(self):
        runtime = _runtime()
        offered = []
        runtime.on_announcement = offered.append
        _announce(runtime)
        _announce(runtime)
        _announce(runtime, tablet_id="njorun-2")
        self.assertEqual(offered, ["njorun-1", "njorun-2"])
        runtime.announcements["njorun-1"]["expires_at"] = 0
        _announce(runtime)
        self.assertEqual(offered, ["njorun-1", "njorun-2", "njorun-1"])

    async def test_announcement_lookup_forgets_expired_tablets(self):
        runtime = _runtime()
        _announce(runtime)
        self.assertEqual(
            runtime.announcement("njorun-1"),
            {"tablet_id": "njorun-1", "name": "Kitchen", "code": comparison_code(bytes(range(32)))},
        )
        self.assertIsNotNone(runtime.announcement_expires_at("njorun-1"))
        runtime.announcements["njorun-1"]["expires_at"] = 0
        self.assertIsNone(runtime.announcement("njorun-1"))
        self.assertIsNone(runtime.announcement_expires_at("njorun-1"))

    async def _enrolled(self):
        runtime = _runtime()
        await runtime.async_configure(
            {"operation": "add_profile", "profile_id": "other", "name": "Other"},
            expected_revision=runtime.configuration.revision,
        )
        _announce(runtime)
        await runtime.async_approve(
            tablet_id="njorun-1",
            profile_id="home",
            expected_revision=runtime.configuration.revision,
        )
        return runtime

    async def test_use_profile_values_clears_the_tablets_own_settings(self):
        runtime = await self._enrolled()
        await runtime.async_set_setting("njorun-1", "theme", "dark")
        self.assertEqual(runtime.configuration.tablets["njorun-1"]["overrides"], {"theme": "dark"})
        await runtime.async_use_profile_values("njorun-1")
        self.assertEqual(runtime.configuration.tablets["njorun-1"]["overrides"], {})
        revision = runtime.configuration.revision
        await runtime.async_use_profile_values("njorun-1")
        self.assertEqual(runtime.configuration.revision, revision, "nothing to clear, nothing to send")
        with self.assertRaises(RuntimeError):
            await runtime.async_use_profile_values("missing")

    async def test_choosing_another_profile_keeps_the_tablets_own_settings(self):
        runtime = await self._enrolled()
        await runtime.async_set_setting("njorun-1", "theme", "dark")
        await runtime.async_set_profile("njorun-1", "other")
        tablet = runtime.configuration.tablets["njorun-1"]
        self.assertEqual(tablet["profile_id"], "other")
        self.assertEqual(tablet["overrides"], {"theme": "dark"})
        revision = runtime.configuration.revision
        await runtime.async_set_profile("njorun-1", "other")
        self.assertEqual(runtime.configuration.revision, revision)
        with self.assertRaises(RuntimeError):
            await runtime.async_set_profile("njorun-1", "nope")

    async def test_the_log_sink_reaches_every_tablet_through_a_new_revision(self):
        runtime = await self._enrolled()
        before = runtime.configuration.tablets["njorun-1"]["revision"]
        await runtime.async_set_log_sink("https://loki.example.org")
        self.assertEqual(runtime.configuration.log_sink, "https://loki.example.org")
        self.assertGreater(runtime.configuration.tablets["njorun-1"]["revision"], before)
        self.assertEqual(runtime.store.data["configuration"]["log_sink"], "https://loki.example.org")
        with self.assertRaises(RuntimeError):
            await runtime.async_set_log_sink("http://user:pw@host")
        await runtime.async_set_log_sink("")
        self.assertEqual(runtime.configuration.log_sink, "")

    async def test_removing_a_tablet_forgets_its_trust_and_lets_it_wait_again(self):
        runtime = await self._enrolled()
        await runtime.async_remove_tablet("njorun-1")
        self.assertNotIn("njorun-1", runtime.configuration.tablets)
        self.assertEqual(runtime.bindings, {})
        self.assertEqual(_announce(runtime)["status"], "pending")

    async def test_deleting_all_entities_lets_a_returning_tablet_get_new_ones(self):
        runtime = await self._enrolled()
        first, second = object(), object()
        runtime._entities_created = {("select", "njorun-1"), ("select", "njorun-2")}
        runtime.add_entity("njorun-1", first)
        runtime.add_entity("njorun-1", second)
        runtime.remove_entity("njorun-1", first)
        self.assertIn(("select", "njorun-1"), runtime._entities_created)
        runtime.remove_entity("njorun-1", second)
        self.assertEqual(runtime._entities_created, {("select", "njorun-2")})

    def test_waiting_list_is_bounded(self):
        runtime = _runtime()
        for index in range(runtime_module.MAX_ANNOUNCEMENTS):
            _announce(runtime, tablet_id=f"t-{index}")
        with self.assertRaises(RuntimeError):
            _announce(runtime, tablet_id="one-too-many")
        self.assertEqual(_announce(runtime, tablet_id="t-0")["status"], "pending")


if __name__ == "__main__":
    unittest.main()
