"""The USB provisioning tool against the frames Vellum's client wrote (plan 005)."""

import importlib.util
import json
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("usb_provision", ROOT / "scripts" / "usb-provision.py")
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)
FIXTURE = json.loads((ROOT / "protocol/fixtures/improv-frames.json").read_text())


def host_case(name):
    return next(item for item in FIXTURE["host_to_device"] if item["name"] == name)


class EncoderTests(unittest.TestCase):
    def test_simple_commands_match_vellum(self):
        for name, command in (("get_state", tool.GET_STATE), ("scan_wifi", tool.SCAN_WIFI),
                              ("get_device_info", tool.GET_DEVICE_INFO)):
            self.assertEqual(tool.rpc_command(command).hex(), host_case(name)["frame"], name)

    def test_profiles_match_vellum_byte_for_byte(self):
        cases = {
            "wifi_minimal": dict(ssid="MyNet", password="s3cret!!"),
            "wifi_with_url": dict(ssid="MyNet", password="s3cret!!", url="https://home.example.org"),
            "wifi_url_and_time": dict(ssid="MyNet", password="s3cret!!",
                                      url="https://home.example.org", utc=1_790_000_000),
            "wifi_time_only": dict(ssid="MyNet", password="s3cret!!", utc=1_790_000_000),
            "wifi_voucher_and_ntp": dict(ssid="Net", password="pw", url="https://h.io",
                                         voucher="tok123", ntp="pool.ntp.org"),
            "wifi_open_utf8": dict(ssid="Café-Netz", password=""),
        }
        for name, fields in cases.items():
            ssid, password = fields.pop("ssid"), fields.pop("password")
            self.assertEqual(tool.wifi_settings(ssid, password, **fields).hex(),
                             host_case(name)["frame"], name)

    def test_a_profile_over_253_bytes_is_refused(self):
        with self.assertRaises(ValueError):
            tool.wifi_settings_payload("n", "p", url="https://" + "a" * 250)


class ParserTests(unittest.TestCase):
    def test_every_frame_of_the_device_is_read(self):
        for item in FIXTURE["device_to_host"]:
            frames = tool.Parser().push(bytes.fromhex(item["frame"]))
            self.assertEqual(len(frames), 1, item["name"])
            frame_type, payload = frames[0]
            if item["kind"] == "state":
                self.assertEqual((frame_type, payload), (tool.CURRENT_STATE, bytes([item["state"]])))
            elif item["kind"] == "error":
                self.assertEqual((frame_type, payload), (tool.ERROR_STATE, bytes([item["error"]])))
            else:
                self.assertEqual(frame_type, tool.RPC_RESULT)
                self.assertEqual(tool.decode_result(payload), (item["cmd"], item["strings"]), item["name"])

    def test_log_text_chunks_and_the_magic_in_text(self):
        frame = bytes.fromhex(FIXTURE["device_to_host"][0]["frame"])
        stream = b"I (1) app: IMPROV is a word\r\n" + frame + b"\r\nmore"
        parser = tool.Parser()
        found = []
        for index in range(0, len(stream), 3):
            found += parser.push(stream[index:index + 3])
        self.assertEqual(len(found), 1)

    def test_a_wrong_sum_is_skipped(self):
        good = bytes.fromhex(FIXTURE["device_to_host"][1]["frame"])
        bad = bytearray(bytes.fromhex(FIXTURE["device_to_host"][0]["frame"]))
        bad[-1] ^= 0xFF
        self.assertEqual(len(tool.Parser().push(bytes(bad) + good)), 1)


if __name__ == "__main__":
    unittest.main()
