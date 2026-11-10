# Firmware release manifest v1

Implements the artifact side of decision D7 in
[plan 003](../plans/003-d1001-ha-management.md) and of the blueprint's
[update and recovery contract](../plans/001-embedded-ha-blueprint.md#update-and-recovery-contract).
Every firmware release publishes one manifest with a detached Ed25519 signature
next to the images it describes. Home Assistant, the installer page and the
tablet verify it with the same rules. The reference implementation is
`scripts/release/manifest.py`; the shared test vectors are
[`protocol/fixtures/firmware-manifest.json`](../../protocol/fixtures/firmware-manifest.json).

## Trust model

The open-source variant has no secure boot and no flash encryption. Nothing in
the bootloader checks this signature. **The signature check lives in the
application** (the tablet's OTA code, the Home Assistant integration, the HTTPS
installer). It protects the update path against network attackers and corrupted
downloads. It does not protect against someone with physical access to the
tablet, who can flash any image over USB.

The public keys are a build-time setting of each verifier, never read from a
release. A fork or a self-built firmware embeds its own keys and signs its own
releases. A manifest cannot introduce a key.

## Published files

A release (GitHub Release for `stable`, prerelease for `beta`) contains:

| File | Content |
| --- | --- |
| `manifest.json` | The manifest, UTF-8 JSON |
| `manifest.json.sig` | Detached signature, see below |
| `njorun-<board>-<version>-factory.bin` | Merged image written at flash address 0: bootloader, partition table, first application |
| `njorun-<board>-<version>-app.bin` | Application image for an OTA slot, byte-identical to the part of the factory image at the `ota_0` offset |

File names inside the manifest are plain names, resolved next to the manifest.
They contain only `A-Z a-z 0-9 . _ -`, start with an alphanumeric character and
are at most 128 characters long.

## Signature and canonical bytes

The signed message is **the exact bytes of `manifest.json` as published**. There
is no canonicalization step: verifiers must not parse and re-serialize before
verifying, and must not normalize line endings or whitespace. Whoever serves or
caches the file must deliver it byte for byte. The reference tool writes sorted
keys, two-space indent, UTF-8 without BOM and one trailing newline; verifiers do
not depend on that layout.

`manifest.json.sig` holds the 64-byte Ed25519 signature (RFC 8032, pure
Ed25519) as standard padded base64 (RFC 4648 section 4), optionally followed by a
single `\n`. Anything else is `bad_signature`.

Verification order, each failure being a stable reason code:

1. Reject a manifest larger than 65,536 bytes (`malformed`) before any parsing.
2. Decode the signature (`bad_signature`).
3. Try each trusted public key (current, then next). If none verifies the exact
   manifest bytes: `bad_signature`.
4. Only now parse the manifest: valid UTF-8 JSON, no duplicate object keys, no
   `NaN`/`Infinity`, exactly the fields below (`malformed`, or
   `unsupported_format` for another `format`).
5. `signing_key_id` must equal the key ID of the key that verified
   (`key_id_mismatch`).
6. The caller's board, hardware revision and, where known, channel must match an
   entry (`board_mismatch`, `revision_unsupported`, `channel_mismatch`).
7. Each image is checked against its entry: size (`size_mismatch`), SHA-256
   (`digest_mismatch`) and, for `app`, the size of the target slot
   (`oversize`). A streaming verifier checks size while receiving and the
   digest at the end, and switches the boot slot only after both pass.

## Keys and rotation

Key ID: the first 8 bytes of SHA-256 over the raw 32-byte public key, 16
lowercase hex characters. Public keys are written as standard padded base64 of
the raw 32 bytes.

Every verifier carries two public keys, **current** and **next**, and accepts a
signature from either. Rotation: (1) release firmware that embeds the new key
as `next`; (2) once the fleet runs it, sign releases with the former `next` key;
(3) a later firmware demotes it to `current` and embeds a fresh `next`. The
former current key is then retired. A leaked key is handled by rotating on the
same path, which needs a release that all tablets can still verify, so the
`next` key must not be kept in the same place as `current`.

## Manifest fields

```json
{
  "format": 1,
  "product": "njorun",
  "release": {
    "version": "0.1.0-beta.1",
    "channel": "beta",
    "source_commit": "0123456789abcdef0123456789abcdef01234567"
  },
  "signing_key_id": "0123456789abcdef",
  "boards": [
    {
      "board": "d1001",
      "hardware_revisions": ["1"],
      "partition_layout": "d1001-32m-2xota8m-v1",
      "partition_table_sha256": "<64 hex>",
      "coprocessor": {"chip": "esp32-c6", "min_firmware": null},
      "min_integration_protocol": 1,
      "artifacts": {
        "factory": {"name": "njorun-d1001-0.1.0-beta.1-factory.bin", "size": 123, "sha256": "<64 hex>"},
        "app": {"name": "njorun-d1001-0.1.0-beta.1-app.bin", "size": 123, "sha256": "<64 hex>"}
      }
    }
  ]
}
```

Objects have exactly these keys; unknown or missing keys are `malformed`, so a
field can only be added in a new `format`.

- `format`: integer, `1`.
- `product`: `njorun`.
- `release.version`: SemVer 2.0.0 without build metadata. `channel` is
  `stable` or `beta`. A stable version has no prerelease suffix; a beta version
  ends in `-beta.N`. Release metadata therefore binds channel and version
  together, and the channel cannot be changed by renaming a release.
- `release.source_commit`: 40 lowercase hex characters, the commit that was built.
- `signing_key_id`: key ID of the signing key, inside the signed bytes.
- `boards`: non-empty list, one entry per board ID, unique.
  - `board`: board ID (`d1001`), `[a-z0-9][a-z0-9._-]{0,63}`.
  - `hardware_revisions`: non-empty list of revision IDs the image supports.
  - `partition_layout`: layout ID. A change of the table needs a new ID and a
    USB reflash; OTA never changes the table. `partition_table_sha256` is the
    digest of the CSV the images were built with, so the ID cannot drift from the
    table.
  - `coprocessor`: `chip` (`esp32-c6`) and `min_firmware`, the lowest
    coprocessor firmware version this image works with, or `null` when it makes
    no demand.
  - `min_integration_protocol`: lowest integration management protocol version
    (integer) that can drive the update; Home Assistant offers the update only
    to an integration that speaks it.
  - `artifacts.factory` and `artifacts.app`: `name`, `size` (bytes, at least 1)
    and `sha256` (64 lowercase hex characters) of the file.

The tablet reports its board and hardware revision; the layout and coprocessor
fields are compared with what it reports before it accepts an install command.

## Test vectors

[`firmware-manifest.json`](../../protocol/fixtures/firmware-manifest.json) holds
public keys and cases `{name, manifest, signature, trusted_keys, request,
artifact?, slot_size?, expect}`. `manifest` is the exact signed text. `expect`
is `ok` or the reason code of the first failing step in the order above. Python
(`tests/release`) checks all cases today; the firmware and the integration add
their checks against the same file when they implement verification. The keys
are deterministic throwaway keys derived by `scripts/release/fixtures.py`; the
fixture holds no private material.
