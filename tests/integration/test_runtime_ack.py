"""Deterministic regression tests for serialized HA runtime acknowledgements."""

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


ROOT = Path(__file__).resolve().parents[2]
INTEGRATION = ROOT / "custom_components" / "njorun"
PACKAGE_NAME = "njorun_runtime_ack_test"
package = types.ModuleType(PACKAGE_NAME)
package.__path__ = [str(INTEGRATION)]
sys.modules[PACKAGE_NAME] = package


class _Store:
    def __class_getitem__(cls, _item):
        return cls


class _ConfigEntry:
    pass


class _HomeAssistant:
    pass


ha = types.ModuleType("homeassistant")
ha.__path__ = []
config_entries = types.ModuleType("homeassistant.config_entries")
config_entries.ConfigEntry = _ConfigEntry
core = types.ModuleType("homeassistant.core")
core.HomeAssistant = _HomeAssistant
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
PendingOperation = runtime_module.PendingOperation
Session = runtime_module.Session
RuntimeError = runtime_module.RuntimeError


class _Noise:
    def __init__(self, payload: bytes):
        self.payload = payload

    def decrypt(self, _frame: bytes) -> bytes:
        return self.payload


class _Connection:
    subscriptions = {}


class _MemoryStore:
    def __init__(self):
        self.data = None

    async def async_save(self, data):
        self.data = deepcopy(data)


def _runtime_for_ack(revision_value: int | float = 2):
    config = Configuration()
    config.add_profile("shared", "Shared", {}, 0)
    config.add_tablet(
        "one",
        "One",
        "shared",
        {"orientations": ["portrait", "landscape"], "brightness": True},
        config.revision,
    )
    settings = config.snapshot("one").settings
    payload = {
        "type": "applied",
        "operation_id": "operation-1",
        "revision": revision_value,
        "settings": settings,
    }
    runtime = NjorunRuntime.__new__(NjorunRuntime)
    runtime.configuration = config
    runtime.bindings = {
        "binding-1": {"tablet_id": "one", "status": "active"}
    }
    runtime.applied = {}
    runtime.sessions = {}
    runtime.session_by_tablet = {"one": "session-1"}
    runtime.pending_operations = {}
    runtime.completed_operations = set()
    runtime.entity_adders = {}
    runtime.entities = {}
    runtime._registered_platforms = set()
    runtime._entities_created = set()
    runtime._lock = asyncio.Lock()
    runtime._closed = False
    runtime.store = _MemoryStore()
    runtime.hass = types.SimpleNamespace(loop=asyncio.get_running_loop())
    connection = _Connection()
    session = Session(
        session_id="session-1",
        binding_id="binding-1",
        tablet_id="one",
        context=b"",
        connection=connection,
        expires_at=runtime_module.time.time() + 60,
        phase="active",
        noise=_Noise(json.dumps(payload).encode()),
    )
    runtime.sessions[session.session_id] = session
    operation = PendingOperation(
        operation_id="operation-1",
        session_id=session.session_id,
        binding_id=session.binding_id,
        tablet_id=session.tablet_id,
        revision=config.tablets["one"]["revision"],
        settings=settings,
        expires_at=runtime_module.time.time() + 30,
    )
    runtime.pending_operations[operation.operation_id] = operation
    return runtime, connection, session, base64.b64encode(b"frame").decode()


class RuntimeAckTests(unittest.IsolatedAsyncioTestCase):
    async def test_json_float_revision_is_rejected_without_persisting(self):
        runtime, connection, _session, frame = _runtime_for_ack(2.0)
        with self.assertRaises(RuntimeError):
            await runtime.async_receive_ack(connection, "session-1", frame)
        self.assertIsNone(runtime.store.data)
        self.assertIn("operation-1", runtime.pending_operations)
        self.assertEqual(runtime.applied, {})

    async def test_ack_waiting_behind_removal_cannot_restore_removed_tablet(self):
        await self._assert_mutation_wins_ack("remove_tablet")

    async def test_ack_waiting_behind_revocation_cannot_commit(self):
        await self._assert_mutation_wins_ack("revoke")

    async def _assert_mutation_wins_ack(self, operation):
        runtime, connection, _session, frame = _runtime_for_ack()
        expected_revision = runtime.configuration.revision
        await runtime._lock.acquire()
        mutation = asyncio.create_task(
            runtime.async_configure(
                {"operation": operation, "tablet_id": "one"},
                expected_revision=expected_revision,
            )
        )
        await asyncio.sleep(0)
        ack = asyncio.create_task(
            runtime.async_receive_ack(connection, "session-1", frame)
        )
        await asyncio.sleep(0)
        self.assertFalse(mutation.done())
        self.assertFalse(ack.done())
        runtime._lock.release()
        await mutation
        with self.assertRaises(RuntimeError):
            await ack
        self.assertNotIn("one", runtime.applied)
        self.assertIsNotNone(runtime.store.data)
        self.assertNotIn("one", runtime.store.data["applied"])
        if operation == "remove_tablet":
            restored = Configuration.restore(runtime.store.data["configuration"])
            self.assertNotIn("one", restored.tablets)
            self.assertNotIn("one", runtime.configuration.tablets)
        else:
            self.assertEqual(runtime.bindings["binding-1"]["status"], "revoked")
            self.assertNotIn("session-1", runtime.sessions)


if __name__ == "__main__":
    unittest.main()
