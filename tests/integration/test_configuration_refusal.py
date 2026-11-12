"""A tablet that does not apply a configuration says why (plan 006, O4)."""

from __future__ import annotations

import json
from pathlib import Path
import sys
import unittest
from unittest.mock import MagicMock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_runtime_ack as base  # noqa: E402  (reuses the stubbed runtime module and harness)

RuntimeError = base.RuntimeError


def _refusal(reason="invalid", **changes):
    payload = {
        "type": "rejected",
        "operation_id": "operation-1",
        "revision": None,
        "reason": reason,
    }
    payload.update(changes)
    return payload


async def _harness(**changes):
    runtime, connection, session, frame = base._runtime_for_ack()
    payload = _refusal(**changes)
    if payload["revision"] is None:
        payload["revision"] = runtime.pending_operations["operation-1"].revision
    session.noise = base._Noise(json.dumps(payload).encode())
    runtime._raise_configuration_issue = MagicMock()
    runtime._clear_configuration_issue = MagicMock()
    runtime.async_write_tablet_state = MagicMock()
    return runtime, connection, session, frame


class ConfigurationRefusalTests(unittest.IsolatedAsyncioTestCase):
    async def test_a_matching_refusal_ends_the_operation_and_shows_the_reason(self):
        runtime, connection, _session, frame = await _harness(reason="invalid")
        await runtime.async_receive_ack(connection, "session-1", frame)
        runtime._raise_configuration_issue.assert_called_once_with("one", "invalid")
        self.assertNotIn("operation-1", runtime.pending_operations)
        self.assertIn("operation-1", runtime.completed_operations)
        # A refusal never counts as applied state.
        self.assertEqual(runtime.applied, {})
        self.assertIsNone(runtime.store.data)

    async def test_every_documented_reason_is_accepted(self):
        for reason in ("malformed", "expired", "stale", "invalid", "storage"):
            runtime, connection, _session, frame = await _harness(reason=reason)
            await runtime.async_receive_ack(connection, "session-1", frame)
            runtime._raise_configuration_issue.assert_called_once_with("one", reason)

    async def test_an_unknown_reason_or_extra_field_ends_the_session(self):
        for changes in ({"reason": "because"}, {"extra": 1}):
            runtime, connection, _session, frame = await _harness(**changes)
            runtime._close_session = MagicMock()
            with self.assertRaises(RuntimeError):
                await runtime.async_receive_ack(connection, "session-1", frame)
            runtime._close_session.assert_called_once_with("session-1")
            runtime._raise_configuration_issue.assert_not_called()

    async def test_a_refusal_for_another_revision_or_operation_changes_nothing(self):
        for changes in ({"revision": 99}, {"operation_id": "operation-2"}):
            runtime, connection, _session, frame = await _harness(**changes)
            with self.assertRaises(RuntimeError):
                await runtime.async_receive_ack(connection, "session-1", frame)
            runtime._raise_configuration_issue.assert_not_called()
            self.assertIn("operation-1", runtime.pending_operations)

    async def test_a_refusal_cannot_be_replayed(self):
        runtime, connection, _session, frame = await _harness()
        await runtime.async_receive_ack(connection, "session-1", frame)
        with self.assertRaises(RuntimeError):
            await runtime.async_receive_ack(connection, "session-1", frame)
        runtime._raise_configuration_issue.assert_called_once()


if __name__ == "__main__":
    unittest.main()
