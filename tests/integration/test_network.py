"""Network settings: the shared fixture decides, plus the secrets rule (plan 006)."""
import importlib.util
import json
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "njorun_network", ROOT / "custom_components/njorun/network.py")
network = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = network
SPEC.loader.exec_module(network)

CASES = json.loads((ROOT / "protocol/fixtures/network-cases.json").read_text(encoding="utf-8"))


class NetworkCaseTests(unittest.TestCase):
    def test_every_case_is_decided_as_the_fixture_says(self):
        self.assertGreaterEqual(len(CASES), 40)
        for case in CASES:
            with self.subTest(case["name"]):
                if case["valid"]:
                    network.validate_network(case["config"], case["interfaces"])
                    continue
                with self.assertRaises(network.NetworkError) as caught:
                    network.validate_network(case["config"], case["interfaces"])
                self.assertEqual(caught.exception.problem, case["problem"])

    def test_no_message_names_a_password(self):
        for case in CASES:
            password = (case["config"].get("wifi") or {}).get("password")
            if not password or case["valid"]:
                continue
            with self.assertRaises(network.NetworkError) as caught:
                network.validate_network(case["config"], case["interfaces"])
            self.assertNotIn(password, str(caught.exception))

    def test_redacted_hides_the_password_and_keeps_the_rest(self):
        config = {"wifi": {"ssid": "Home", "password": "correct horse", "ip": {"mode": "dhcp"}}}
        shown = network.redacted(config)
        self.assertEqual(shown["wifi"]["password"], "<set>")
        self.assertEqual(shown["wifi"]["ssid"], "Home")
        self.assertEqual(config["wifi"]["password"], "correct horse", "the original is untouched")
        self.assertEqual(network.redacted({"wifi": {"ssid": "Open"}}), {"wifi": {"ssid": "Open"}})


if __name__ == "__main__":
    unittest.main()
