# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: GPL-3.0-or-later
"""Firmware release manifest: build, sign and verify (docs/protocols/003-firmware-manifest.md).

The signature covers the exact bytes of the published manifest file. Nothing in
this module re-serializes a manifest before verifying it.
"""
import base64
import binascii
import hashlib
import json
import re

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519

FORMAT = 1
PRODUCT = "njorun"
MAX_MANIFEST_BYTES = 64 * 1024
CHANNELS = ("stable", "beta")
KINDS = ("factory", "app")

SEMVER = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
                    r"(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
IDENT = re.compile(r"^[a-z0-9][a-z0-9._-]{0,63}$")
FILE_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
KEY_ID = re.compile(r"^[0-9a-f]{16}$")


class ManifestError(Exception):
    """A manifest or image was rejected. `code` is the stable reason of the specification."""

    def __init__(self, code, detail=""):
        super().__init__(f"{code}: {detail}" if detail else code)
        self.code = code
        self.detail = detail


def key_id(public_raw):
    """Key ID: first 8 bytes of SHA-256 over the raw 32-byte public key, lowercase hex."""
    return hashlib.sha256(public_raw).digest()[:8].hex()


def public_raw(public_key):
    return public_key.public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)


def decode_public_key(text):
    """Decode a standard padded base64 raw Ed25519 public key."""
    try:
        raw = base64.b64decode(text, validate=True)
    except (binascii.Error, ValueError, TypeError) as error:
        raise ManifestError("malformed", "public key is not base64") from error
    if len(raw) != 32:
        raise ManifestError("malformed", "public key is not 32 bytes")
    return raw


def load_private_key(pem):
    key = serialization.load_pem_private_key(pem, password=None)
    if not isinstance(key, ed25519.Ed25519PrivateKey):
        raise ManifestError("malformed", "private key is not Ed25519")
    return key


def sha256_hex(data):
    return hashlib.sha256(data).hexdigest()


def artifact_entry(name, data):
    return {"name": name, "size": len(data), "sha256": sha256_hex(data)}


def build_manifest(*, version, channel, source_commit, signing_key_id, boards):
    """Return the canonical publication bytes of a manifest.

    Keys sorted, two-space indent, UTF-8, one trailing newline. Verifiers never depend on
    this layout: the bytes as published are what is signed.
    """
    manifest = {
        "format": FORMAT,
        "product": PRODUCT,
        "release": {"version": version, "channel": channel, "source_commit": source_commit},
        "signing_key_id": signing_key_id,
        "boards": boards,
    }
    validate(manifest)
    return (json.dumps(manifest, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode()


def sign(manifest_bytes, private_key):
    """Detached signature, standard padded base64 of the 64 raw bytes plus a newline."""
    manifest = parse(manifest_bytes)
    expected = key_id(public_raw(private_key.public_key()))
    if manifest["signing_key_id"] != expected:
        raise ManifestError("key_id_mismatch",
                            f"manifest names {manifest['signing_key_id']}, key is {expected}")
    return base64.b64encode(private_key.sign(manifest_bytes)) + b"\n"


def _no_duplicates(pairs):
    result = {}
    for name, value in pairs:
        if name in result:
            raise ManifestError("malformed", f"duplicate key {name!r}")
        result[name] = value
    return result


def parse(manifest_bytes):
    """Parse and validate. Only call on bytes whose signature was verified, or to read a
    manifest this process just built."""
    if len(manifest_bytes) > MAX_MANIFEST_BYTES:
        raise ManifestError("malformed", "manifest too large")
    try:
        manifest = json.loads(manifest_bytes.decode("utf-8"), object_pairs_hook=_no_duplicates,
                              parse_constant=lambda name: (_ for _ in ()).throw(
                                  ManifestError("malformed", f"constant {name}")))
    except (UnicodeDecodeError, json.JSONDecodeError, RecursionError) as error:
        raise ManifestError("malformed", "not valid UTF-8 JSON") from error
    validate(manifest)
    return manifest


def _require(condition, detail):
    if not condition:
        raise ManifestError("malformed", detail)


def _exact_keys(obj, keys, where):
    _require(isinstance(obj, dict), f"{where} is not an object")
    _require(set(obj) == set(keys), f"{where} must have exactly the keys {sorted(keys)}")


def _text(value, pattern, where):
    _require(isinstance(value, str) and pattern.fullmatch(value) is not None,
             f"{where} is malformed")


def _integer(value, where, minimum):
    _require(isinstance(value, int) and not isinstance(value, bool) and value >= minimum,
             f"{where} must be an integer of at least {minimum}")


def validate(manifest):
    _exact_keys(manifest, ("format", "product", "release", "signing_key_id", "boards"),
                "manifest")
    if manifest["format"] != FORMAT:
        raise ManifestError("unsupported_format", str(manifest["format"]))
    _require(manifest["product"] == PRODUCT, "product is not njorun")
    release = manifest["release"]
    _exact_keys(release, ("version", "channel", "source_commit"), "release")
    _text(release["version"], SEMVER, "release.version")
    _require(release["channel"] in CHANNELS, "release.channel is not stable or beta")
    prerelease = "-" in release["version"]
    if release["channel"] == "stable":
        _require(not prerelease, "a stable version must not carry a prerelease suffix")
    else:
        _require(re.search(r"-beta\.[0-9]+$", release["version"]) is not None,
                 "a beta version must end in -beta.N")
    _text(release["source_commit"], COMMIT, "release.source_commit")
    _text(manifest["signing_key_id"], KEY_ID, "signing_key_id")
    boards = manifest["boards"]
    _require(isinstance(boards, list) and boards, "boards must be a non-empty list")
    names = [entry.get("board") if isinstance(entry, dict) else None for entry in boards]
    _require(len(set(names)) == len(names), "board IDs must be unique")
    for entry in boards:
        _exact_keys(entry, ("board", "hardware_revisions", "partition_layout",
                            "partition_table_sha256", "coprocessor",
                            "min_integration_protocol", "artifacts"), "board entry")
        _text(entry["board"], IDENT, "board")
        revisions = entry["hardware_revisions"]
        _require(isinstance(revisions, list) and revisions
                 and len(set(revisions)) == len(revisions), "hardware_revisions is malformed")
        for revision in revisions:
            _text(revision, IDENT, "hardware revision")
        _text(entry["partition_layout"], IDENT, "partition_layout")
        _text(entry["partition_table_sha256"], SHA256, "partition_table_sha256")
        coprocessor = entry["coprocessor"]
        _exact_keys(coprocessor, ("chip", "min_firmware"), "coprocessor")
        _text(coprocessor["chip"], IDENT, "coprocessor.chip")
        if coprocessor["min_firmware"] is not None:
            _text(coprocessor["min_firmware"], SEMVER, "coprocessor.min_firmware")
        _integer(entry["min_integration_protocol"], "min_integration_protocol", 1)
        artifacts = entry["artifacts"]
        _exact_keys(artifacts, KINDS, "artifacts")
        for kind in KINDS:
            artifact = artifacts[kind]
            _exact_keys(artifact, ("name", "size", "sha256"), f"artifact {kind}")
            _text(artifact["name"], FILE_NAME, f"artifact {kind} name")
            _integer(artifact["size"], f"artifact {kind} size", 1)
            _text(artifact["sha256"], SHA256, f"artifact {kind} sha256")
        _require(artifacts["factory"]["name"] != artifacts["app"]["name"],
                 "artifact names must differ")


def verify(manifest_bytes, signature_text, trusted_keys):
    """Verify the detached signature and return the parsed manifest.

    `trusted_keys` is a list of raw 32-byte Ed25519 public keys (current and next).
    Signature first, then parsing: nothing in an unverified manifest is interpreted.
    """
    if len(manifest_bytes) > MAX_MANIFEST_BYTES:
        raise ManifestError("malformed", "manifest too large")
    if isinstance(signature_text, str):
        signature_text = signature_text.encode()
    stripped = signature_text[:-1] if signature_text.endswith(b"\n") else signature_text
    try:
        signature = base64.b64decode(stripped, validate=True)
    except (binascii.Error, ValueError) as error:
        raise ManifestError("bad_signature", "signature is not base64") from error
    if len(signature) != 64:
        raise ManifestError("bad_signature", "signature is not 64 bytes")
    signer = None
    for raw in trusted_keys:
        try:
            ed25519.Ed25519PublicKey.from_public_bytes(raw).verify(signature, manifest_bytes)
        except InvalidSignature:
            continue
        signer = raw
        break
    if signer is None:
        raise ManifestError("bad_signature", "no trusted key verifies the manifest")
    manifest = parse(manifest_bytes)
    if manifest["signing_key_id"] != key_id(signer):
        raise ManifestError("key_id_mismatch",
                            f"manifest names {manifest['signing_key_id']}, signed by {key_id(signer)}")
    return manifest


def select_board(manifest, board, hardware_revision, channel=None):
    """Return the entry for this board and revision, or raise board_mismatch,
    revision_unsupported or channel_mismatch."""
    if channel is not None and manifest["release"]["channel"] != channel:
        raise ManifestError("channel_mismatch", manifest["release"]["channel"])
    for entry in manifest["boards"]:
        if entry["board"] == board:
            if hardware_revision not in entry["hardware_revisions"]:
                raise ManifestError("revision_unsupported", hardware_revision)
            return entry
    raise ManifestError("board_mismatch", board)


def check_artifact(entry, kind, data, slot_size=None):
    """Check bytes against the manifest entry: size, SHA-256 and (app only) slot size."""
    artifact = entry["artifacts"][kind]
    if len(data) != artifact["size"]:
        raise ManifestError("size_mismatch", f"{kind}: {len(data)} != {artifact['size']}")
    if sha256_hex(data) != artifact["sha256"]:
        raise ManifestError("digest_mismatch", kind)
    if kind == "app" and slot_size is not None and len(data) > slot_size:
        raise ManifestError("oversize", f"{len(data)} > {slot_size}")
