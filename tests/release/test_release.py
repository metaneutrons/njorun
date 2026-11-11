# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: GPL-3.0-or-later
"""Release tool: build, sign and verify with a fake espflash and a throwaway key."""
import base64
import contextlib
import io
import json
import os
import pathlib
import re
import stat
import sys
import tempfile
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/release"))

import fixtures  # noqa: E402
import manifest as mf  # noqa: E402
import release  # noqa: E402

TABLE = """# test table
nvs, data, nvs, 0x9000, 0x6000,
ota_0, app, ota_0, 0x20000, 0x1000,
ota_1, app, ota_1, 0x21000, 0x1000,
"""
COMMIT = fixtures.COMMIT


def fake_merged(app_size):
    return b"\xff" * 0x20000 + b"\xe9" + bytes(range(256)) * (app_size // 256) \
        + b"\x01" * (app_size % 256 - 1 if app_size % 256 else 0)


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.dir = pathlib.Path(directory.name)
        self.table = self.dir / "partitions.csv"
        self.table.write_text(TABLE)
        self.elf = self.dir / "firmware.elf"
        self.elf.write_bytes(b"elf")
        (self.dir / "build").mkdir()
        (self.dir / "build" / "bootloader.bin").write_bytes(b"boot")
        self.key = fixtures.test_key("current")
        self.public = mf.public_raw(self.key.public_key())
        self.keys_file = self.dir / "keys.json"
        self.write_keys(self.public)
        self.key_file = self.dir / "signing.pem"
        from cryptography.hazmat.primitives import serialization
        self.key_file.write_bytes(self.key.private_bytes(
            serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption()))
        self.fake_bin = self.dir / "bin"
        self.fake_bin.mkdir()
        self.set_image(fake_merged(0x800))

    def write_keys(self, *raw):
        names = ("current", "next")
        keys = {name: base64.b64encode(raw[i]).decode() if i < len(raw) else None
                for i, name in enumerate(names)}
        self.keys_file.write_text(json.dumps({"keys": keys}))

    def set_image(self, merged):
        (self.dir / "merged.in").write_bytes(merged)
        script = self.fake_bin / "espflash"
        script.write_text('#!/bin/sh\n'
                          'if [ "$1" = "--version" ]; then echo "espflash 4.7.0"; exit 0; fi\n'
                          f'for last; do :; done; cp "{self.dir}/merged.in" "$last"\n')
        script.chmod(script.stat().st_mode | stat.S_IEXEC)

    def run_tool(self, *argv):
        env = {"PATH": f"{self.fake_bin}{os.pathsep}{os.environ['PATH']}"}
        with mock.patch.dict(os.environ, env), contextlib.redirect_stdout(io.StringIO()):
            release.main(list(argv))

    def build_args(self, out=None, **override):
        args = {"--elf": str(self.elf), "--partition-table": str(self.table),
                "--hardware-revision": "1", "--partition-layout": "test-layout-v1",
                "--version": "0.1.0-beta.1", "--channel": "beta", "--source-commit": COMMIT,
                "--out": str(out or self.dir / "out"), "--key-file": str(self.key_file),
                "--public-keys": str(self.keys_file)}
        args.update(override)
        flat = []
        for name, value in args.items():
            if value is not None:
                flat += [name, value]
        return ["build", *flat]

    def verify_args(self, **override):
        args = {"--dir": str(self.dir / "out"), "--hardware-revision": "1",
                "--public-keys": str(self.keys_file), "--partition-table": str(self.table)}
        args.update(override)
        return ["verify", *[part for item in args.items() for part in item]]

    def test_build_then_verify(self):
        self.run_tool(*self.build_args())
        out = self.dir / "out"
        factory = (out / "njorun-d1001-0.1.0-beta.1-factory.bin").read_bytes()
        app = (out / "njorun-d1001-0.1.0-beta.1-app.bin").read_bytes()
        self.assertEqual(factory[:0x20000], b"\xff" * 0x20000)
        self.assertEqual(app, factory[0x20000:])
        self.run_tool(*self.verify_args())
        parsed = json.loads((out / "manifest.json").read_text())
        self.assertEqual(parsed["boards"][0]["artifacts"]["app"]["size"], len(app))

    def test_key_from_environment(self):
        pem = self.key_file.read_text()
        self.key_file.unlink()
        argv = self.build_args(**{"--key-file": None})
        with mock.patch.dict(os.environ, {release.KEY_ENV: pem}):
            self.run_tool(*argv)
        self.run_tool(*self.verify_args())

    def test_missing_key_is_an_error(self):
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop(release.KEY_ENV, None)
            with self.assertRaisesRegex(SystemExit, "no signing key"):
                self.run_tool(*self.build_args(**{"--key-file": None}))

    def test_wrong_key_is_refused_at_build(self):
        self.write_keys(mf.public_raw(fixtures.test_key("other").public_key()))
        with self.assertRaisesRegex(SystemExit, "not one of the trusted"):
            self.run_tool(*self.build_args())

    def test_no_configured_public_key_is_refused(self):
        self.keys_file.write_text(json.dumps({"keys": {"current": None, "next": None}}))
        with self.assertRaisesRegex(SystemExit, "no release public key"):
            self.run_tool(*self.build_args())

    def test_next_key_may_sign(self):
        self.write_keys(mf.public_raw(fixtures.test_key("other").public_key()), self.public)
        self.run_tool(*self.build_args())
        self.run_tool(*self.verify_args())

    def test_verify_with_wrong_key(self):
        self.run_tool(*self.build_args())
        other = base64.b64encode(mf.public_raw(fixtures.test_key("other").public_key())).decode()
        with self.assertRaisesRegex(SystemExit, "bad_signature"):
            self.run_tool(*self.verify_args(**{"--public-key": other}))

    def test_tampered_image(self):
        self.run_tool(*self.build_args())
        path = self.dir / "out" / "njorun-d1001-0.1.0-beta.1-app.bin"
        data = bytearray(path.read_bytes())
        data[10] ^= 1
        path.write_bytes(bytes(data))
        with self.assertRaisesRegex(SystemExit, "digest_mismatch"):
            self.run_tool(*self.verify_args())

    def test_tampered_manifest(self):
        self.run_tool(*self.build_args())
        path = self.dir / "out" / "manifest.json"
        path.write_bytes(path.read_bytes().replace(b"0.1.0-beta.1", b"0.1.1-beta.1", 1))
        with self.assertRaisesRegex(SystemExit, "bad_signature"):
            self.run_tool(*self.verify_args())

    def test_wrong_board_and_revision_and_channel(self):
        self.run_tool(*self.build_args())
        for override, code in (({"--board": "other"}, "board_mismatch"),
                               ({"--hardware-revision": "9"}, "revision_unsupported"),
                               ({"--channel": "stable"}, "channel_mismatch")):
            with self.subTest(code), self.assertRaisesRegex(SystemExit, code):
                self.run_tool(*self.verify_args(**override))

    def test_missing_artifact_file(self):
        self.run_tool(*self.build_args())
        (self.dir / "out" / "njorun-d1001-0.1.0-beta.1-factory.bin").unlink()
        with self.assertRaisesRegex(SystemExit, "missing file"):
            self.run_tool(*self.verify_args())

    def test_image_over_slot_is_refused_and_nothing_is_written(self):
        self.set_image(fake_merged(0x1001))
        with self.assertRaisesRegex(SystemExit, "slot holds"):
            self.run_tool(*self.build_args())
        self.assertFalse((self.dir / "out").exists())

    def test_verify_checks_slot_size(self):
        self.run_tool(*self.build_args())
        small = self.dir / "small.csv"
        small.write_text(TABLE.replace("0x1000", "0x100"))
        with self.assertRaisesRegex(SystemExit, "oversize"):
            self.run_tool(*self.verify_args(**{"--partition-table": str(small)}))

    def test_merged_image_without_application_is_refused(self):
        self.set_image(b"\xff" * 0x20010)
        with self.assertRaisesRegex(SystemExit, "no application image"):
            self.run_tool(*self.build_args())

    def test_unpinned_espflash_is_refused(self):
        script = self.fake_bin / "espflash"
        script.write_text('#!/bin/sh\necho "espflash 4.5.0"\n')
        with self.assertRaisesRegex(SystemExit, "is pinned"):
            self.run_tool(*self.build_args())

    def test_bad_channel_version_pair(self):
        with self.assertRaisesRegex(SystemExit, "malformed"):
            self.run_tool(*self.build_args(**{"--version": "1.0.0", "--channel": "beta"}))

    def test_keygen_refuses_repository_and_overwrite(self):
        with self.assertRaisesRegex(SystemExit, "outside the repository"):
            self.run_tool("keygen", "--out", str(ROOT / "key.pem"))
        target = self.dir / "new.pem"
        self.run_tool("keygen", "--out", str(target))
        self.assertEqual(target.stat().st_mode & 0o777, 0o600)
        with self.assertRaisesRegex(SystemExit, "refusing to overwrite"):
            self.run_tool("keygen", "--out", str(target))

    def test_pinned_version_matches_manifest_file(self):
        self.assertRegex(release.pinned_espflash_version(), r"^\d+\.\d+\.\d+$")


class WorkflowTests(unittest.TestCase):
    """The release workflow, the firmware build and the partition table must agree."""

    def read(self, path):
        return (ROOT / path).read_text()

    def test_layout_id_matches_the_partition_table_header(self):
        header = self.read("experiments/p4-build/partitions.d1001.csv")
        layout = re.search(r"\(layout id ([a-z0-9._-]+),", header)
        self.assertIsNotNone(layout, "the CSV header names the layout ID")
        workflow = self.read(".github/workflows/release.yml")
        used = re.search(r"--partition-layout (\S+)", workflow)
        self.assertIsNotNone(used)
        self.assertEqual(used.group(1), layout.group(1))

    def test_release_version_reaches_the_firmware_build(self):
        release_workflow = self.read(".github/workflows/release.yml")
        self.assertIn("version: ${{ inputs.version }}", release_workflow)
        build = self.read(".github/workflows/p4-build.yml")
        self.assertIn("NJORUN_VERSION", build)
        self.assertIn("inputs.version", build)
        firmware = self.read("experiments/p4-build/src/ota.rs")
        self.assertIn('option_env!("NJORUN_VERSION")', firmware)


if __name__ == "__main__":
    unittest.main()
