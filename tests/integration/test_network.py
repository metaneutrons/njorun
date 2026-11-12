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
MESSAGES = json.loads(
    (ROOT / "protocol/fixtures/network-messages.json").read_text(encoding="utf-8"))


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


class NetworkMessageTests(unittest.TestCase):
    def check(self, message):
        return network.check_network_message(
            message,
            now=MESSAGES["now"],
            last_revision=MESSAGES["last_revision"],
            interfaces=MESSAGES["interfaces"],
        )

    def test_accepted_messages_pass(self):
        for case in MESSAGES["accepted"]:
            with self.subTest(case["name"]):
                self.check(case["message"])

    def test_refused_messages_are_refused_for_the_stated_reason(self):
        for case in MESSAGES["refused"]:
            with self.subTest(case["name"]):
                with self.assertRaises(network.NetworkError) as caught:
                    self.check(case["message"])
                self.assertEqual(caught.exception.problem, case["rejection"])

    def test_a_refusal_does_not_name_the_password(self):
        for case in MESSAGES["refused"]:
            wifi = case["message"].get("wifi") if isinstance(case["message"], dict) else None
            password = (wifi or {}).get("password") if isinstance(wifi, dict) else None
            if not password:
                continue
            with self.assertRaises(network.NetworkError) as caught:
                self.check(case["message"])
            self.assertNotIn(password, str(caught.exception))

    def test_valid_results_parse_and_invalid_ones_do_not(self):
        for index, payload in enumerate(MESSAGES["results"]["valid"]):
            with self.subTest(valid=index):
                network.parse_network_result(payload, MESSAGES["interfaces"])
        for index, payload in enumerate(MESSAGES["results"]["invalid"]):
            with self.subTest(invalid=index):
                with self.assertRaises(network.NetworkError):
                    network.parse_network_result(payload, MESSAGES["interfaces"])

    def test_a_result_for_an_interface_the_board_lacks_is_invalid(self):
        payload = dict(MESSAGES["results"]["valid"][0])
        payload["state"] = {"ethernet": {"mode": "dhcp", "address": None, "carries_session": False}}
        with self.assertRaises(network.NetworkError):
            network.parse_network_result(payload, ["wifi"])
        network.parse_network_result(payload, ["wifi", "ethernet"])

    def test_the_built_message_is_what_the_tablet_accepts(self):
        config = {"wifi": {"ssid": "Home", "password": "correct horse"}}
        message = network.build_network_message("op-9", 5, 1_000_000_060, config, ["wifi"])
        self.assertEqual(message["type"], "network")
        self.check(message)
        with self.assertRaises(network.NetworkError):
            network.build_network_message("op-9", 5, 1_000_000_060, {}, ["wifi"])


if __name__ == "__main__":
    unittest.main()
