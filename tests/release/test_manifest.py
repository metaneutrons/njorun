# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: GPL-3.0-or-later
"""Firmware manifest: shared test vectors and negative cases."""
import base64
import json
import pathlib
import sys
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/release"))

import fixtures  # noqa: E402
import manifest as mf  # noqa: E402


def run_case(case):
    """Reference check order of the specification. Returns 'ok' or the rejection code."""
    try:
        trusted = [base64.b64decode(key) for key in case["trusted_keys"]]
        parsed = mf.verify(case["manifest"].encode(), case["signature"], trusted)
        request = case["request"]
        entry = mf.select_board(parsed, request["board"], request["hardware_revision"],
                                request["channel"])
        if "artifact" in case:
            mf.check_artifact(entry, case["artifact"]["kind"],
                              base64.b64decode(case["artifact"]["data"]), case.get("slot_size"))
    except mf.ManifestError as error:
        return error.code
    return "ok"


class VectorTests(unittest.TestCase):
    def test_committed_fixture_is_current(self):
        committed = json.loads(fixtures.FIXTURE.read_text())
        self.assertEqual(committed, fixtures.build(),
                         "regenerate: uv run --locked python scripts/release/fixtures.py")

    def test_every_vector(self):
        committed = json.loads(fixtures.FIXTURE.read_text())
        self.assertGreaterEqual(len(committed["cases"]), 10)
        for case in committed["cases"]:
            with self.subTest(case["name"]):
                self.assertEqual(run_case(case), case["expect"])

    def test_vectors_contain_no_private_key_material(self):
        text = fixtures.FIXTURE.read_text()
        self.assertNotIn("PRIVATE", text)
        self.assertNotIn("seed", json.dumps(json.loads(text)["keys"]))


class SchemaTests(unittest.TestCase):
    def setUp(self):
        self.key = fixtures.test_key("current")
        self.id = mf.key_id(mf.public_raw(self.key.public_key()))
        self.data = fixtures.images()

    def build(self, **override):
        args = dict(version="1.2.3", channel="stable", source_commit=fixtures.COMMIT,
                    signing_key_id=self.id, boards=[fixtures.board_entry(self.data)])
        args.update(override)
        return mf.build_manifest(**args)

    def test_round_trip(self):
        text = self.build()
        signature = mf.sign(text, self.key)
        parsed = mf.verify(text, signature, [mf.public_raw(self.key.public_key())])
        self.assertEqual(parsed["release"]["version"], "1.2.3")

    def test_channel_and_version_must_agree(self):
        with self.assertRaises(mf.ManifestError):
            self.build(version="1.2.3-beta.1", channel="stable")
        with self.assertRaises(mf.ManifestError):
            self.build(version="1.2.3", channel="beta")

    def test_bad_inputs_rejected(self):
        board = fixtures.board_entry(self.data)
        for bad in (dict(source_commit="abc"), dict(signing_key_id="zz"),
                    dict(boards=[]), dict(boards=[board, board])):
            with self.subTest(bad), self.assertRaises(mf.ManifestError):
                self.build(**bad)
        path_name = fixtures.board_entry(self.data)
        path_name["artifacts"]["app"]["name"] = "../app.bin"
        with self.assertRaises(mf.ManifestError):
            self.build(boards=[path_name])

    def test_unknown_and_missing_fields_rejected(self):
        parsed = json.loads(self.build())
        parsed["extra"] = 1
        with self.assertRaises(mf.ManifestError):
            mf.validate(parsed)
        del parsed["extra"], parsed["boards"]
        with self.assertRaises(mf.ManifestError):
            mf.validate(parsed)

    def test_sign_refuses_key_that_is_not_named(self):
        text = self.build()
        with self.assertRaises(mf.ManifestError) as raised:
            mf.sign(text, fixtures.test_key("other"))
        self.assertEqual(raised.exception.code, "key_id_mismatch")

    def test_oversized_manifest_rejected_before_parsing(self):
        with self.assertRaises(mf.ManifestError) as raised:
            mf.verify(b" " * (mf.MAX_MANIFEST_BYTES + 1), b"", [])
        self.assertEqual(raised.exception.code, "malformed")

    def test_nan_and_non_utf8_rejected(self):
        for bad in (b'{"format": NaN}', b"\xff\xfe"):
            with self.assertRaises(mf.ManifestError):
                mf.parse(bad)


if __name__ == "__main__":
    unittest.main()
