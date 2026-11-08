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

    def test_waiting_list_is_bounded(self):
        runtime = _runtime()
        for index in range(runtime_module.MAX_ANNOUNCEMENTS):
            _announce(runtime, tablet_id=f"t-{index}")
        with self.assertRaises(RuntimeError):
            _announce(runtime, tablet_id="one-too-many")
        self.assertEqual(_announce(runtime, tablet_id="t-0")["status"], "pending")


if __name__ == "__main__":
    unittest.main()
