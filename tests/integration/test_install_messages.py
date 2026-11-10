"""Install messages of decision D7 against the shared fixture."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = json.loads((ROOT / "protocol/fixtures/install-messages.json").read_text())
_spec = importlib.util.spec_from_file_location(
    "njorun_install_under_test", ROOT / "custom_components/njorun/install.py"
)
install = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(install)


class InstallMessageTests(unittest.TestCase):
    def test_install_messages_follow_the_fixture(self) -> None:
        for message in FIXTURE["install"]["valid"]:
            self.assertTrue(install.valid_install(message), message)
        for message in FIXTURE["install"]["invalid"]:
            self.assertFalse(install.valid_install(message), message)

    def test_results_follow_the_fixture(self) -> None:
        for message in FIXTURE["install_result"]["valid"]:
            self.assertEqual(
                install.parse_install_result(message), (message["install_id"], message["reason"])
            )
        for message in FIXTURE["install_result"]["invalid"]:
            self.assertIsNone(install.parse_install_result(message), message)

    def test_progress_follows_the_fixture(self) -> None:
        for message in FIXTURE["install_progress"]["valid"]:
            self.assertEqual(
                install.parse_install_progress(message),
                (message["install_id"], message["stage"], message["done"], message["total"]),
            )
        for message in FIXTURE["install_progress"]["invalid"]:
            self.assertIsNone(install.parse_install_progress(message), message)

    def test_outcomes_follow_the_fixture(self) -> None:
        for message in FIXTURE["install_outcome"]["valid"]:
            self.assertEqual(
                install.parse_install_outcome(message),
                (message["install_id"], message["state"], message["version"]),
            )
        for message in FIXTURE["install_outcome"]["invalid"]:
            self.assertIsNone(install.parse_install_outcome(message), message)

    def test_fixture_sets_match_the_constants(self) -> None:
        self.assertEqual(tuple(FIXTURE["reject_reasons"]), install.REJECT_REASONS)
        self.assertEqual(tuple(FIXTURE["stages"]), install.STAGES)
        self.assertEqual(tuple(FIXTURE["outcome_states"]), install.OUTCOME_STATES)
        self.assertEqual(tuple(FIXTURE["channels"]), install.CHANNELS)
        limits = FIXTURE["limits"]
        self.assertEqual(limits["command_ttl_seconds"], install.INSTALL_TTL)
        self.assertEqual(limits["result_timeout_seconds"], install.RESULT_TIMEOUT)
        self.assertEqual(limits["outcome_window_seconds"], install.OUTCOME_WINDOW)
        self.assertEqual(limits["slot_bytes"], install.SLOT_BYTES)
        self.assertEqual(limits["version_chars"], install.MAX_VERSION_CHARS)
        self.assertEqual(limits["url_chars"], install.MAX_URL_CHARS)


if __name__ == "__main__":
    unittest.main()
