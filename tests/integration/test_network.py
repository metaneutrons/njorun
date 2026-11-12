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


class FormTests(unittest.TestCase):
    def test_dhcp_needs_nothing_else(self):
        self.assertEqual(network.ip_from_text("dhcp", "x", "y", "z", "w"), {"mode": "dhcp"})

    def test_a_static_setup_is_built_from_the_texts(self):
        ip = network.ip_from_text(
            "static", " 192.168.1.50 ", "24", "192.168.1.1", "192.168.1.1, 9.9.9.9")
        self.assertEqual(ip, {
            "mode": "static", "address": "192.168.1.50", "prefix": 24,
            "gateway": "192.168.1.1", "dns": ["192.168.1.1", "9.9.9.9"]})
        self.assertEqual(
            network.ip_from_text("static", "192.168.1.50", "24", "192.168.1.1", "")["dns"], [])

    def test_the_problem_names_the_field_to_correct(self):
        cases = [
            (("static", "", "24", "192.168.1.1", ""), "shape"),
            (("static", "nope", "24", "192.168.1.1", ""), "address"),
            (("static", "192.168.1.50", "x", "192.168.1.1", ""), "prefix"),
            (("static", "192.168.1.50", "31", "192.168.1.1", ""), "prefix"),
            (("static", "192.168.1.50", "24", "nope", ""), "gateway"),
            (("static", "192.168.1.50", "24", "10.0.0.1", ""), "gateway"),
            (("static", "192.168.1.50", "24", "192.168.1.1", "nope"), "dns"),
            (("static", "192.168.1.50", "24", "192.168.1.1", "1.1.1.1 8.8.8.8 9.9.9.9"), "dns"),
            (("static", "192.168.1.0", "24", "192.168.1.1", ""), "address"),
            (("auto", "", "", "", ""), "parse"),
        ]
        for args, problem in cases:
            with self.subTest(args):
                with self.assertRaises(network.NetworkError) as caught:
                    network.ip_from_text(*args)
                self.assertEqual(caught.exception.problem, problem)


class SensorStateTests(unittest.TestCase):
    STATE = {"wifi": {"mode": "dhcp", "address": "192.168.1.20", "ssid": "Home",
                      "carries_session": True}}

    def test_nothing_set(self):
        self.assertEqual(network.describe_network(None), ("not_set", {}))

    def test_waiting_and_pending(self):
        status = {"revision": 5, "result": None, "pending": False}
        self.assertEqual(network.describe_network(status), ("waiting", {"revision": 5}))
        status["pending"] = True
        self.assertEqual(network.describe_network(status)[0], "pending")

    def test_an_old_result_does_not_count_for_a_newer_revision(self):
        old = {"revision": 4, "result": "applied", "proof": "session", "state": self.STATE}
        status = {"revision": 5, "result": old, "pending": True}
        self.assertEqual(network.describe_network(status)[0], "pending")

    def test_applied_and_reverted_carry_proof_or_reason_and_no_secret(self):
        applied = {"revision": 5, "result": "applied", "proof": "session", "state": self.STATE}
        state, attributes = network.describe_network(
            {"revision": 5, "result": applied, "pending": False})
        self.assertEqual(state, "applied")
        self.assertEqual(attributes["proof"], "session")
        self.assertEqual(attributes["wifi"],
                         {"mode": "dhcp", "address": "192.168.1.20", "ssid": "Home"})
        reverted = {"revision": 5, "result": "reverted", "reason": "no_address", "state": self.STATE}
        state, attributes = network.describe_network(
            {"revision": 5, "result": reverted, "pending": False})
        self.assertEqual((state, attributes["reason"]), ("reverted", "no_address"))
        self.assertNotIn("carries_session", attributes["wifi"])


if __name__ == "__main__":
    unittest.main()
