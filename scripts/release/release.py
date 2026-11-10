#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: GPL-3.0-or-later
"""Build, sign and verify Njörun firmware releases (docs/protocols/003-firmware-manifest.md).

  keygen   create a signing key outside the repository and print its public half
  build    build factory.bin and app.bin with espflash, write manifest.json, sign it
  verify   check a published release directory against trusted public keys

The private key is read only from the RELEASE_SIGNING_KEY environment variable (PEM text)
or from the file named by --key-file. It is never read from the repository.
"""
import argparse
import base64
import importlib.util
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import manifest as mf  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[2]
KEYS_FILE = ROOT / "protocol/release-keys.json"
TOOLS_FILE = ROOT / "scripts/ci/firmware-tools.json"
KEY_ENV = "RELEASE_SIGNING_KEY"
SLOT_MAGIC = 0xE9  # first byte of an ESP application image

_spec = importlib.util.spec_from_file_location(
    "check_image_size", ROOT / "scripts/ci/check-image-size.py")
sizes = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sizes)


class ReleaseError(SystemExit):
    def __init__(self, message):
        super().__init__(f"error: {message}")


def pinned_espflash_version():
    for tool in json.loads(TOOLS_FILE.read_text()):
        if tool["name"] == "espflash":
            return tool["version"]
    raise ReleaseError(f"{TOOLS_FILE} does not pin espflash")


def check_espflash_version():
    out = subprocess.run(["espflash", "--version"], check=True, capture_output=True,
                         text=True).stdout
    found = re.search(r"\b(\d+\.\d+\.\d+)\b", out)
    pinned = pinned_espflash_version()
    if not found or found.group(1) != pinned:
        raise ReleaseError(f"espflash {found.group(1) if found else '?'} found, "
                           f"{pinned} is pinned in {TOOLS_FILE.name}")


def partition_offset(table, name):
    for line in pathlib.Path(table).read_text().splitlines():
        cells = [cell.strip() for cell in line.split(",")]
        if line.strip() and not line.lstrip().startswith("#") and cells[0] == name:
            return sizes.parse_number(cells[3])
    raise ReleaseError(f"{table}: no partition {name}")


def build_images(elf, bootloader, table, work):
    """Return (factory, app) bytes the way scripts/flash-app.sh does: one merged image from
    address 0, the application image being its part from the ota_0 offset."""
    merged = work / "merged.bin"
    subprocess.run(["espflash", "save-image", "--chip", "esp32p4", "--flash-size", "32mb",
                    "--merge", "--skip-padding", "--bootloader", str(bootloader),
                    "--partition-table", str(table), str(elf), str(merged)],
                   check=True, stdout=subprocess.DEVNULL)
    factory = merged.read_bytes()
    offset = partition_offset(table, "ota_0")
    if len(factory) <= offset or factory[offset] != SLOT_MAGIC:
        raise ReleaseError("the merged image has no application image at the ota_0 offset")
    return factory, factory[offset:]


def read_public_keys(path=KEYS_FILE):
    """Trusted public keys (current, next) of this repository or fork: a build-time setting."""
    data = json.loads(pathlib.Path(path).read_text())
    keys = [data["keys"][name] for name in ("current", "next") if data["keys"].get(name)]
    if not keys:
        raise ReleaseError(f"no release public key is configured in {path}")
    return [mf.decode_public_key(key) for key in keys]


def load_signing_key(key_file):
    if key_file:
        pem = pathlib.Path(key_file).read_bytes()
    elif os.environ.get(KEY_ENV):
        pem = os.environ[KEY_ENV].encode()
    else:
        raise ReleaseError(f"no signing key: set {KEY_ENV} or pass --key-file")
    try:
        return mf.load_private_key(pem)
    except (ValueError, TypeError, mf.ManifestError) as error:
        raise ReleaseError("the signing key is not a PEM Ed25519 private key") from error


def release_names(board, version):
    return (f"njorun-{board}-{version}-factory.bin", f"njorun-{board}-{version}-app.bin")


def cmd_build(args):
    if not args.skip_espflash_version_check:
        check_espflash_version()
    key = load_signing_key(args.key_file)
    public = mf.public_raw(key.public_key())
    if public not in read_public_keys(args.public_keys):
        raise ReleaseError("the signing key is not one of the trusted public keys of "
                           f"{args.public_keys}")
    budget, _ = sizes.slot_budget(args.partition_table)
    with tempfile.TemporaryDirectory() as directory:
        factory, app = build_images(args.elf, args.bootloader, args.partition_table,
                                    pathlib.Path(directory))
    if len(app) > budget:
        raise ReleaseError(f"the application image is {len(app):,} bytes, "
                           f"the slot holds {budget:,}")
    factory_name, app_name = release_names(args.board, args.version)
    entry = {
        "board": args.board,
        "hardware_revisions": args.hardware_revision,
        "partition_layout": args.partition_layout,
        "partition_table_sha256": mf.sha256_hex(args.partition_table.read_bytes()),
        "coprocessor": {"chip": "esp32-c6", "min_firmware": args.coprocessor_min_firmware},
        "min_integration_protocol": args.min_integration_protocol,
        "artifacts": {"factory": mf.artifact_entry(factory_name, factory),
                      "app": mf.artifact_entry(app_name, app)},
    }
    try:
        text = mf.build_manifest(version=args.version, channel=args.channel,
                                 source_commit=args.source_commit,
                                 signing_key_id=mf.key_id(public), boards=[entry])
        signature = mf.sign(text, key)
    except mf.ManifestError as error:
        raise ReleaseError(str(error)) from error
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    (out / factory_name).write_bytes(factory)
    (out / app_name).write_bytes(app)
    (out / "manifest.json").write_bytes(text)
    (out / "manifest.json.sig").write_bytes(signature)
    # The release is only worth publishing if it passes the verifier the consumers run.
    verify_directory(out, read_public_keys(args.public_keys), args.board,
                     args.hardware_revision[0], args.channel, budget)
    print(f"wrote {factory_name} ({len(factory):,} bytes), {app_name} ({len(app):,} bytes), "
          f"manifest.json and manifest.json.sig to {out}")


def verify_directory(directory, trusted, board, revision, channel, slot_size):
    directory = pathlib.Path(directory)
    try:
        parsed = mf.verify((directory / "manifest.json").read_bytes(),
                           (directory / "manifest.json.sig").read_bytes(), trusted)
        entry = mf.select_board(parsed, board, revision, channel)
        for kind in mf.KINDS:
            name = entry["artifacts"][kind]["name"]
            mf.check_artifact(entry, kind, (directory / name).read_bytes(), slot_size)
    except mf.ManifestError as error:
        raise ReleaseError(f"verification failed: {error}") from error
    except FileNotFoundError as error:
        raise ReleaseError(f"missing file: {error.filename}") from error
    return parsed


def cmd_verify(args):
    if args.public_key:
        trusted = [mf.decode_public_key(key) for key in args.public_key]
    else:
        trusted = read_public_keys(args.public_keys)
    budget = sizes.slot_budget(args.partition_table)[0] if args.partition_table else None
    parsed = verify_directory(args.dir, trusted, args.board, args.hardware_revision,
                              args.channel, budget)
    release = parsed["release"]
    print(f"ok: {args.board} {release['version']} ({release['channel']}) "
          f"commit {release['source_commit']} signed by {parsed['signing_key_id']}")


def cmd_keygen(args):
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric import ed25519
    out = args.out.resolve()
    if out.is_relative_to(ROOT):
        raise ReleaseError("keep the private key outside the repository")
    key = ed25519.Ed25519PrivateKey.generate()
    pem = key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                            serialization.NoEncryption())
    try:
        descriptor = os.open(out, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError as error:
        raise ReleaseError(f"{out} exists, refusing to overwrite") from error
    with os.fdopen(descriptor, "wb") as handle:
        handle.write(pem)
    public = mf.public_raw(key.public_key())
    print(f"private key written to {out} (mode 0600)")
    print(f"public key (base64): {base64.b64encode(public).decode()}")
    print(f"key id: {mf.key_id(public)}")


def parser():
    top = argparse.ArgumentParser(description=__doc__,
                                  formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = top.add_subparsers(dest="command", required=True)
    keygen = sub.add_parser("keygen", help=cmd_keygen.__doc__)
    keygen.add_argument("--out", type=pathlib.Path, required=True)
    keygen.set_defaults(run=cmd_keygen)

    build = sub.add_parser("build")
    build.add_argument("--elf", type=pathlib.Path, default=ROOT / sizes.DEFAULT_ELF)
    build.add_argument("--bootloader", type=pathlib.Path)
    build.add_argument("--partition-table", type=pathlib.Path, default=ROOT / sizes.DEFAULT_TABLE)
    build.add_argument("--board", default="d1001")
    build.add_argument("--hardware-revision", action="append", required=True,
                       help="supported hardware revision, repeatable")
    build.add_argument("--partition-layout", required=True)
    build.add_argument("--coprocessor-min-firmware")
    build.add_argument("--min-integration-protocol", type=int, default=1)
    build.add_argument("--version", required=True)
    build.add_argument("--channel", choices=mf.CHANNELS, required=True)
    build.add_argument("--source-commit", required=True)
    build.add_argument("--out", type=pathlib.Path, required=True)
    build.add_argument("--key-file", type=pathlib.Path)
    build.add_argument("--public-keys", type=pathlib.Path, default=KEYS_FILE)
    build.add_argument("--skip-espflash-version-check", action="store_true",
                       help="local dry runs only; CI always checks the pinned version")
    build.set_defaults(run=cmd_build)

    check = sub.add_parser("verify")
    check.add_argument("--dir", type=pathlib.Path, required=True)
    check.add_argument("--board", default="d1001")
    check.add_argument("--hardware-revision", required=True)
    check.add_argument("--channel", choices=mf.CHANNELS)
    check.add_argument("--public-key", action="append",
                       help="base64 raw public key, repeatable; default: protocol/release-keys.json")
    check.add_argument("--public-keys", type=pathlib.Path, default=KEYS_FILE)
    check.add_argument("--partition-table", type=pathlib.Path,
                       help="also check the application image against its slot size")
    check.set_defaults(run=cmd_verify)
    return top


def main(argv=None):
    args = parser().parse_args(argv)
    if args.command == "build" and args.bootloader is None:
        args.bootloader = args.elf.parent / "build" / "bootloader.bin"
    args.run(args)


if __name__ == "__main__":
    main()
