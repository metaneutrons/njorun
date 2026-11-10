"""A restarted tablet takes over its stale session once it proves its key."""

from __future__ import annotations

import asyncio
import base64
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
PACKAGE_NAME = "njorun_takeover_test"
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


class _Noise:
    def __init__(self, payload):
        self.payload = payload

    def decrypt(self, _frame):
        return json.dumps(self.payload).encode()


class _Connection:
    def __init__(self):
        self.user = types.SimpleNamespace(id="user")
        self.subscriptions = {}


class _MemoryStore:
    async def async_save(self, _data):
        pass


def _runtime():
    config = Configuration()
    config.add_profile("home", "Home", {}, 0)
    config.add_tablet(
        "t1", "Kitchen", "home", {"orientations": ["portrait"], "brightness": True}, config.revision
    )
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


def _confirm_payload(session):
    return {
        "type": "confirm",
        "protocol": 1,
        "binding_id": session.binding_id,
        "session_id": session.session_id,
    }


async def _establish(runtime, connection, subscription_id, *, valid=True):
    """start_session, then skip the Noise handshake and confirm with a stub cipher."""
    started = runtime.async_start_session(connection, "b1")
    session = runtime.sessions[started["session_id"]]
    session.phase = "await_confirm"
    payload = _confirm_payload(session)
    if not valid:
        payload["binding_id"] = "other"
    session.noise = _Noise(payload)
    frame = base64.b64encode(b"frame").decode()
    await runtime.async_confirm_and_subscribe(connection, session.session_id, frame, subscription_id)
    return session


class TakeoverTests(unittest.IsolatedAsyncioTestCase):
    async def test_a_restarted_tablet_replaces_its_stale_session(self):
        runtime = _runtime()
        old_connection, new_connection = _Connection(), _Connection()
        old = await _establish(runtime, old_connection, 5)
        self.assertTrue(runtime.is_available("t1"))

        new = await _establish(runtime, new_connection, 7)
        self.assertEqual(runtime.session_by_tablet["t1"], new.session_id)
        self.assertEqual(old.phase, "closed")
        self.assertNotIn(old.session_id, runtime.sessions)
        self.assertEqual(old_connection.subscriptions, {})
        self.assertIn(7, new_connection.subscriptions)
        self.assertTrue(runtime.is_available("t1"))

    async def test_starting_a_session_alone_does_not_disturb_the_live_one(self):
        runtime = _runtime()
        connection = _Connection()
        live = await _establish(runtime, connection, 5)
        runtime.async_start_session(_Connection(), "b1")
        self.assertEqual(runtime.session_by_tablet["t1"], live.session_id)
        self.assertEqual(live.phase, "active")
        self.assertTrue(runtime.is_available("t1"))

    async def test_a_failed_proof_leaves_the_live_session_alone(self):
        runtime = _runtime()
        live = await _establish(runtime, _Connection(), 5)
        with self.assertRaises(RuntimeError):
            await _establish(runtime, _Connection(), 7, valid=False)
        self.assertEqual(runtime.session_by_tablet["t1"], live.session_id)
        self.assertEqual(live.phase, "active")
        self.assertTrue(runtime.is_available("t1"))

    async def test_handshakes_in_flight_are_bounded(self):
        runtime = _runtime()
        started = [
            runtime.async_start_session(_Connection(), "b1")["session_id"]
            for _ in range(runtime_module.MAX_HANDSHAKES_PER_TABLET + 2)
        ]
        open_ids = [sid for sid in started if sid in runtime.sessions]
        self.assertEqual(len(open_ids), runtime_module.MAX_HANDSHAKES_PER_TABLET)
        self.assertEqual(open_ids, started[-runtime_module.MAX_HANDSHAKES_PER_TABLET:])

    async def test_session_start_still_needs_a_signed_in_user_and_a_live_binding(self):
        runtime = _runtime()
        with self.assertRaises(RuntimeError):
            runtime.async_start_session(types.SimpleNamespace(), "b1")
        with self.assertRaises(RuntimeError):
            runtime.async_start_session(_Connection(), "unknown")
        runtime.bindings["b1"]["status"] = "revoked"
        with self.assertRaises(RuntimeError):
            runtime.async_start_session(_Connection(), "b1")


if __name__ == "__main__":
    unittest.main()
