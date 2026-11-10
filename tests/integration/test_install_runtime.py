"""Install protocol of decision D7: the runtime state machine against the shared fixture."""

from __future__ import annotations

import asyncio
from pathlib import Path
import sys
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_live as base  # noqa: E402  (reuses the stubbed runtime module and helpers)

runtime_module = base.runtime_module
RuntimeError = base.RuntimeError
install_module = base.importlib.import_module(f"{base.PACKAGE_NAME}.install")
FIXTURE = base.json.loads(
    (base.ROOT / "protocol/fixtures/install-messages.json").read_text()
)
CAPABILITIES = {
    "orientations": ["portrait"],
    "brightness": True,
    "live": ["wake", "backlight", "telemetry"],
    "ota": {"board": "reterminal-d1001"},
}
SIZE = 1048576
IMAGE = {
    "version": "0.2.0",
    "channel": "stable",
    "size": SIZE,
    "sha256": "0123456789abcdef" * 4,
    "url": "https://ha.local:8123/api/njorun/firmware/x",
    "token": "A" * 43,
}


def _runtime(capabilities=CAPABILITIES):
    return base._runtime(capabilities)


def result(install_id, state="accepted", reason=None):
    return {"type": "install_result", "install_id": install_id, "state": state, "reason": reason}


def progress(install_id, stage, done, total=SIZE):
    return {
        "type": "install_progress",
        "install_id": install_id,
        "stage": stage,
        "done": done,
        "total": total,
    }


def outcome(install_id, state="installed", version="0.2.0"):
    return {"type": "install_outcome", "install_id": install_id, "state": state, "version": version}


async def _send(runtime, session, connection, noise, answer=None):
    """Send an install and let the tablet answer; returns the task's result."""
    task = asyncio.create_task(runtime.async_send_install("t1", **IMAGE))
    await asyncio.sleep(0)
    sent = noise.sent[-1]
    if answer is not None:
        await base._receive(runtime, session, connection, noise, answer(sent["install_id"]))
    return sent, task


class CapabilityTests(unittest.TestCase):
    def test_capabilities_carry_an_optional_ota_board(self):
        plain = {"orientations": ["portrait"], "brightness": True}
        self.assertEqual(base.model.validate_capabilities(CAPABILITIES), CAPABILITIES)
        self.assertEqual(base.model.validate_capabilities(plain), plain)
        for ota in ({}, {"board": ""}, {"board": 1}, {"board": "x", "extra": 1}, "x", True):
            with self.assertRaises(base.model.ConfigurationError, msg=ota):
                base.model.validate_capabilities({**plain, "ota": ota})


class InstallRecordTests(unittest.TestCase):
    def record(self, state="accepted"):
        record = install_module.InstallRecord("i1", "0.2.0", SIZE, int(time.time()) + 300)
        if state == "accepted":
            record.receive_result(None)
        return record

    def test_stages_run_forward_and_rate_is_bounded(self):
        record = self.record()
        now = time.time()
        self.assertTrue(record.receive_progress("download", 0, SIZE, now=now, monotonic=0.0))
        self.assertFalse(record.receive_progress("download", 5, SIZE, now=now, monotonic=0.4))
        self.assertTrue(record.receive_progress("download", 6, SIZE, now=now, monotonic=1.0))
        self.assertTrue(
            record.receive_progress("download", SIZE, SIZE, now=now, monotonic=1.1),
            "the end of a stage is never limited",
        )
        self.assertTrue(record.receive_progress("verify", 0, SIZE, now=now, monotonic=1.2))
        with self.assertRaises(install_module.InstallError):
            record.receive_progress("download", SIZE, SIZE, now=now, monotonic=9.0)
        self.assertTrue(record.receive_progress("verify", SIZE, SIZE, now=now, monotonic=2.0))
        with self.assertRaises(install_module.InstallError):
            record.receive_progress("verify", 1, SIZE, now=now, monotonic=19.0)

    def test_total_must_match_the_size_and_reboot_must_be_complete(self):
        record = self.record()
        now = time.time()
        with self.assertRaises(install_module.InstallError):
            record.receive_progress("download", 1, SIZE + 1, now=now, monotonic=0.0)
        with self.assertRaises(install_module.InstallError):
            record.receive_progress("reboot", 1, SIZE, now=now, monotonic=0.0)

    def test_progress_and_outcome_are_dropped_after_the_window(self):
        record = self.record()
        late = record.window_end() + 1
        self.assertFalse(record.receive_progress("download", 0, SIZE, now=late, monotonic=0.0))
        self.assertFalse(record.receive_outcome("installed", "0.2.0", now=late))
        self.assertEqual(record.state, "accepted")

    def test_an_installed_outcome_must_report_the_requested_version(self):
        record = self.record()
        with self.assertRaises(install_module.InstallError):
            record.receive_outcome("installed", "0.1.0", now=time.time())
        self.assertTrue(record.receive_outcome("rolled_back", "0.1.0", now=time.time()))
        self.assertEqual(record.reported_version, "0.1.0")
        self.assertFalse(record.receive_outcome("rolled_back", "0.1.0", now=time.time()))

    def test_a_result_is_only_expected_once(self):
        record = self.record(state="sent")
        record.receive_result("busy")
        self.assertEqual((record.state, record.reason), ("rejected", "busy"))
        with self.assertRaises(install_module.InstallError):
            record.receive_result(None)


class InstallRuntimeTests(unittest.IsolatedAsyncioTestCase):
    async def test_the_negotiated_flag_follows_the_reported_capability(self):
        runtime = _runtime()
        await base._establish(runtime)
        self.assertTrue(runtime.ota_offered("t1"))
        self.assertEqual(runtime.tablet_board("t1"), "reterminal-d1001")
        other = _runtime({"orientations": ["portrait"], "brightness": True})
        session, *_ = await base._establish(other)
        self.assertFalse(session.ota)
        self.assertFalse(other.ota_offered("t1"))
        self.assertIsNone(other.tablet_board("t1"))

    async def test_an_install_is_sent_and_accepted(self):
        runtime = _runtime()
        session, connection, noise = await base._establish(runtime)
        sent, task = await _send(
            runtime, session, connection, noise, lambda install_id: result(install_id)
        )
        self.assertTrue(install_module.valid_install(sent))
        self.assertLessEqual(sent["expires_at"], int(time.time()) + install_module.INSTALL_TTL)
        self.assertEqual(await task, sent["install_id"])
        status = runtime.install_status("t1")
        self.assertEqual((status["state"], status["version"]), ("accepted", "0.2.0"))

    async def test_each_reject_reason_is_an_error_and_stays_visible(self):
        for reason in FIXTURE["reject_reasons"]:
            runtime = _runtime()
            session, connection, noise = await base._establish(runtime)
            _, task = await _send(
                runtime,
                session,
                connection,
                noise,
                lambda install_id, reason=reason: result(install_id, "rejected", reason),
            )
            with self.assertRaises(RuntimeError) as caught:
                await task
            self.assertIn(reason, str(caught.exception))
            status = runtime.install_status("t1")
            self.assertEqual((status["state"], status["reason"]), ("rejected", reason))
            # A rejected install is final: HA may send the next one at once.
            _, retry = await _send(
                runtime, session, connection, noise, lambda install_id: result(install_id)
            )
            await retry

    async def test_installs_need_a_live_session_and_the_capability(self):
        runtime = _runtime()
        with self.assertRaises(RuntimeError):
            await runtime.async_send_install("t1", **IMAGE)
        plain = _runtime({"orientations": ["portrait"], "brightness": True})
        await base._establish(plain)
        with self.assertRaises(RuntimeError):
            await plain.async_send_install("t1", **IMAGE)
        self.assertEqual(plain.installs, {})

    async def test_invalid_parameters_are_refused_before_sending(self):
        runtime = _runtime()
        _, _, noise = await base._establish(runtime)
        for change in ({"size": 0}, {"channel": "nightly"}, {"token": "short"}, {"url": "ftp://x"}):
            with self.assertRaises(RuntimeError):
                await runtime.async_send_install("t1", **{**IMAGE, **change})
        self.assertEqual(noise.sent, [])
        self.assertEqual(runtime.installs, {})

    async def test_only_one_install_is_open_per_tablet(self):
        runtime = _runtime()
        session, connection, noise = await base._establish(runtime)
        _, task = await _send(
            runtime, session, connection, noise, lambda install_id: result(install_id)
        )
        await task
        with self.assertRaises(RuntimeError):
            await runtime.async_send_install("t1", **IMAGE)
        self.assertEqual(len(noise.sent), 1)

    async def test_an_unanswered_install_times_out_and_is_not_kept(self):
        runtime = _runtime()
        await base._establish(runtime)
        with patch.object(runtime_module, "INSTALL_RESULT_TIMEOUT", 0.01):
            with self.assertRaises(RuntimeError):
                await runtime.async_send_install("t1", **IMAGE)
        self.assertIsNone(runtime.install_status("t1"))

    async def test_an_unexpected_result_ends_the_session(self):
        runtime = _runtime()
        session, connection, noise = await base._establish(runtime)
        with self.assertRaises(RuntimeError):
            await base._receive(runtime, session, connection, noise, result("nobody"))
        self.assertFalse(runtime.ota_offered("t1"))

    async def test_progress_and_outcome_complete_an_accepted_install(self):
        runtime = _runtime()
        session, connection, noise = await base._establish(runtime)
        sent, task = await _send(
            runtime, session, connection, noise, lambda install_id: result(install_id)
        )
        await task
        install_id = sent["install_id"]
        with patch.object(runtime_module.time, "monotonic", side_effect=[0.0, 5.0, 10.0]):
            for stage, done in (("download", 0), ("download", SIZE // 2), ("write", SIZE)):
                await base._receive(
                    runtime, session, connection, noise, progress(install_id, stage, done)
                )
        status = runtime.install_status("t1")
        self.assertEqual((status["stage"], status["done"]), ("write", SIZE))
        self.assertEqual(status["state"], "accepted", "progress is not an outcome")
        await base._receive(runtime, session, connection, noise, outcome(install_id))
        self.assertEqual(runtime.install_status("t1")["state"], "installed")

    async def test_an_outcome_arrives_on_a_later_session(self):
        runtime = _runtime()
        session, connection, noise = await base._establish(runtime)
        sent, task = await _send(
            runtime, session, connection, noise, lambda install_id: result(install_id)
        )
        await task
        runtime._close_session(session.session_id)
        self.assertEqual(runtime.install_status("t1")["state"], "accepted")
        session2, connection2, noise2 = await base._establish(runtime, subscription_id=6)
        await base._receive(
            runtime, session2, connection2, noise2, outcome(sent["install_id"], "rolled_back", "0.1.0")
        )
        status = runtime.install_status("t1")
        self.assertEqual((status["state"], status["reported_version"]), ("rolled_back", "0.1.0"))

    async def test_unknown_progress_and_outcome_are_dropped_not_fatal(self):
        runtime = _runtime()
        session, connection, noise = await base._establish(runtime)
        await base._receive(runtime, session, connection, noise, progress("ghost", "download", 1))
        await base._receive(runtime, session, connection, noise, outcome("ghost"))
        self.assertTrue(runtime.ota_offered("t1"))
        self.assertIsNone(runtime.install_status("t1"))

    async def test_malformed_backward_or_unnegotiated_messages_end_the_session(self):
        cases = {
            "malformed": lambda i: {**progress(i, "download", 1), "extra": 1},
            "unknown stage": lambda i: progress(i, "flash", 1),
            "backwards": lambda i: progress(i, "download", 1),
            "wrong version": lambda i: outcome(i, "installed", "0.1.0"),
        }
        for label, build in cases.items():
            runtime = _runtime()
            session, connection, noise = await base._establish(runtime)
            sent, task = await _send(
                runtime, session, connection, noise, lambda i: result(i)
            )
            await task
            if label == "backwards":
                await base._receive(
                    runtime, session, connection, noise, progress(sent["install_id"], "verify", 1)
                )
            with self.assertRaises(RuntimeError, msg=label):
                await base._receive(
                    runtime, session, connection, noise, build(sent["install_id"])
                )
            self.assertFalse(runtime.ota_offered("t1"), label)
        plain = _runtime({"orientations": ["portrait"], "brightness": True})
        session, connection, noise = await base._establish(plain)
        with self.assertRaises(RuntimeError):
            await base._receive(plain, session, connection, noise, progress("i", "download", 1))

    async def test_a_session_that_ends_before_the_result_fails_the_install(self):
        runtime = _runtime()
        session, *_ = await base._establish(runtime)
        task = asyncio.create_task(runtime.async_send_install("t1", **IMAGE))
        await asyncio.sleep(0)
        runtime._close_session(session.session_id)
        with self.assertRaises(RuntimeError):
            await task
        self.assertIsNone(runtime.install_status("t1"))

    async def test_nothing_is_replayed_after_a_reconnect(self):
        runtime = _runtime()
        session, connection, noise = await base._establish(runtime)
        _, task = await _send(
            runtime, session, connection, noise, lambda install_id: result(install_id)
        )
        await task
        runtime._close_session(session.session_id)
        _, _, noise2 = await base._establish(runtime, subscription_id=6)
        self.assertEqual(noise2.sent, [])


if __name__ == "__main__":
    unittest.main()
