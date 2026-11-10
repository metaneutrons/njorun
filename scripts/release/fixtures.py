# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: GPL-3.0-or-later
"""Generate protocol/fixtures/firmware-manifest.json from deterministic throwaway test keys.

The test keys derive from SHA-256 of a fixed label and protect nothing. The fixture holds
public keys only. Regenerate with:  uv run --locked python scripts/release/fixtures.py
"""
import base64
import hashlib
import json
import pathlib

from cryptography.hazmat.primitives.asymmetric import ed25519

import manifest as mf

ROOT = pathlib.Path(__file__).resolve().parents[2]
FIXTURE = ROOT / "protocol/fixtures/firmware-manifest.json"
COMMIT = "0123456789abcdef0123456789abcdef01234567"
TABLE_SHA = hashlib.sha256(b"fixture partition table").hexdigest()


def test_key(label):
    seed = hashlib.sha256(f"njorun firmware manifest test key: {label}".encode()).digest()
    return ed25519.Ed25519PrivateKey.from_private_bytes(seed)


def b64(data):
    return base64.b64encode(data).decode()


def images():
    return {"factory": bytes(range(96)), "app": bytes((7 * i) % 256 for i in range(64))}


def board_entry(data):
    return {
        "board": "d1001",
        "hardware_revisions": ["1"],
        "partition_layout": "d1001-32m-2xota8m-v1",
        "partition_table_sha256": TABLE_SHA,
        "coprocessor": {"chip": "esp32-c6", "min_firmware": None},
        "min_integration_protocol": 1,
        "artifacts": {
            "factory": mf.artifact_entry("njorun-d1001-0.1.0-beta.1-factory.bin",
                                         data["factory"]),
            "app": mf.artifact_entry("njorun-d1001-0.1.0-beta.1-app.bin", data["app"]),
        },
    }


def build():
    current, following, other = test_key("current"), test_key("next"), test_key("other")
    ids = {name: mf.key_id(mf.public_raw(key.public_key()))
           for name, key in (("current", current), ("next", following), ("other", other))}
    data = images()

    def manifest_bytes(key_name, **override):
        return mf.build_manifest(version="0.1.0-beta.1", channel="beta", source_commit=COMMIT,
                                 signing_key_id=ids[key_name],
                                 boards=[board_entry(data)], **override)

    good = manifest_bytes("current")
    good_sig = mf.sign(good, current)
    trusted = [b64(mf.public_raw(current.public_key())), b64(mf.public_raw(following.public_key()))]
    request = {"board": "d1001", "hardware_revision": "1", "channel": "beta"}

    def case(name, expect, manifest=good, signature=good_sig, **extra):
        entry = {"name": name, "manifest": manifest.decode(), "signature": signature.decode(),
                 "trusted_keys": trusted, "request": request, "expect": expect}
        entry.update(extra)
        return entry

    def raw_sign(text, key):
        return base64.b64encode(key.sign(text)) + b"\n"

    next_manifest = manifest_bytes("next")
    wrong_id = good.replace(ids["current"].encode(), ids["next"].encode())
    format_two = good.replace(b'"format": 1', b'"format": 2')
    duplicate = good.replace(b'  "product": "njorun",\n',
                             b'  "product": "njorun",\n  "product": "njorun",\n')
    tampered = good.replace(b"0.1.0-beta.1", b"0.1.1-beta.1", 1)
    app = data["app"]
    cases = [
        case("valid_signed_by_current_key", "ok"),
        case("valid_signed_by_next_key", "ok", next_manifest, mf.sign(next_manifest, following)),
        case("tampered_manifest_text", "bad_signature", tampered),
        case("signer_not_trusted", "bad_signature", trusted_keys=[trusted[1]]),
        case("signature_not_base64", "bad_signature", signature=b"not base64!\n"),
        case("signature_wrong_length", "bad_signature", signature=b64(b"\0" * 63).encode()),
        case("key_id_names_other_key", "key_id_mismatch", wrong_id, raw_sign(wrong_id, current)),
        case("unsupported_format_signed", "unsupported_format", format_two,
             raw_sign(format_two, current)),
        case("duplicate_key_signed", "malformed", duplicate, raw_sign(duplicate, current)),
        case("wrong_board", "board_mismatch", request={**request, "board": "d2000"}),
        case("unsupported_hardware_revision", "revision_unsupported",
             request={**request, "hardware_revision": "2"}),
        case("wrong_channel", "channel_mismatch", request={**request, "channel": "stable"}),
        case("artifact_matches", "ok", artifact={"kind": "app", "data": b64(app)}),
        case("artifact_digest_mismatch", "digest_mismatch",
             artifact={"kind": "app", "data": b64(bytes([app[0] ^ 1]) + app[1:])}),
        case("artifact_size_mismatch", "size_mismatch",
             artifact={"kind": "factory", "data": b64(data["factory"][:-1])}),
        case("app_larger_than_slot", "oversize",
             artifact={"kind": "app", "data": b64(app)}, slot_size=len(app) - 1),
    ]
    return {
        "description": ("Firmware manifest test vectors (docs/protocols/003-firmware-manifest.md). "
                        "Keys are deterministic throwaway test keys, see scripts/release/fixtures.py. "
                        "'manifest' is the exact text that is signed; 'expect' is 'ok' or the stable "
                        "rejection code. Check order: signature, key ID, board, revision, channel, "
                        "artifact."),
        "keys": {name: {"public_key": b64(mf.public_raw(key.public_key())), "key_id": ids[name]}
                 for name, key in (("current", current), ("next", following), ("other", other))},
        "cases": cases,
    }


if __name__ == "__main__":
    FIXTURE.write_text(json.dumps(build(), indent=2) + "\n")
