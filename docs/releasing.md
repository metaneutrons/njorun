# Releasing firmware

Firmware ships through GitHub Releases with an Ed25519-signed manifest
(decision D7 in [plan 003](plans/003-d1001-ha-management.md); format in
[protocol 003](protocols/003-firmware-manifest.md)). The
[Release workflow](../.github/workflows/release.yml) builds, signs and publishes.
It has never run for real: it needs a signing key that only the owner can create.

## What the workflow does

Manual dispatch with an exact commit SHA, the channel (`stable` or `beta`), the
version and the hardware revision. It then:

1. **guard**: checks the SHA is a full lowercase hash, that the commit is on
   `main` (stable) or `develop` (beta), that the version fits the channel
   (`1.2.3` or `1.2.3-beta.4`), that the tag `v<version>` does not exist yet and
   that the commit has a successful `CI Success` check. A missing `develop`
   branch fails here.
2. **build**: the reusable [P4 build](../.github/workflows/p4-build.yml), the
   same two-clean-builds comparison and slot-size check as in CI, on that exact
   commit.
3. **publish** (protected environment `release`): stops at once if the secret
   `RELEASE_SIGNING_KEY` is absent, builds `factory.bin` and `app.bin` with the
   pinned `espflash`, writes and signs `manifest.json`, verifies the result with
   the repository's public keys, and creates the GitHub Release (a prerelease for
   `beta`) with the images, the manifest and the signature.

The tool is `scripts/release/release.py` (`keygen`, `build`, `verify`). To check a
downloaded release:

```sh
uv run --locked python scripts/release/release.py verify --dir ./release \
  --hardware-revision 1 --partition-table experiments/p4-build/partitions.d1001.csv
```

## One-time setup by the owner

1. Create the key pair on a trusted machine, outside the repository (the tool
   refuses a path inside it and refuses to overwrite):

   ```sh
   uv run --locked python scripts/release/release.py keygen --out ~/njorun-release-key.pem
   ```

   It prints the public key in base64 and its key ID. Create a second pair the
   same way for `next` and keep that private key offline, apart from the first.
2. Put the **public** keys into [`protocol/release-keys.json`](../protocol/release-keys.json)
   (`current` and `next`) and merge that through a pull request. This file is the
   build-time setting: the firmware (item 4 of #6), the integration and the
   installer page read their trusted keys from it.
3. In the repository settings create the environment `release` with required
   reviewers (and, once `main` and `develop` are protected, a deployment branch
   rule), then add the environment secret `RELEASE_SIGNING_KEY` with the **content**
   of the `current` private key file (PEM, including the BEGIN and END lines).
   Do not store it as a repository secret and never commit it.
4. Create the `develop` branch (and its protection) before the first beta.
5. For stable releases, release-please is not wired yet. Until it is, the
   maintainer passes the version it will name.

The first dispatch should be a `beta`, to see the whole path work before a stable.

## Forks and self-built firmware

A fork replaces the two public keys in `protocol/release-keys.json` with its own,
creates its own `RELEASE_SIGNING_KEY` and signs its own releases. Nothing else is
tied to the project's key. Firmware built from the fork embeds the fork's keys and
so accepts only the fork's releases. The signature check is in the application,
not in the bootloader (no secure boot), so it protects the update path, not a
tablet in someone's hands.

## What still needs the owner or later items

- The signing keys, the `release` environment and the secret (above).
- The CI path of `bootloader.bin`: `release/build/bootloader.bin` is taken from
  `scripts/flash-app.sh` and the Darwin build; the first CI run of the build
  workflow confirms it for the Linux build directory.
- The OTA slot decision (headroom, [#81](https://github.com/metaneutrons/njorun/issues/81)):
  a change needs a USB reflash and a new `partition_layout` ID.
- The hardware revision names the D1001 will report, and the C6 minimum firmware
  once a C6 image exists (the manifest carries `null` until then).
- The firmware's Rust verifier (`crates/management/src/firmware.rs`) runs against
  [`firmware-manifest.json`](../protocol/fixtures/firmware-manifest.json); the integration's
  verifier (item 6) is still open.
- The firmware reports the version it runs from `NJORUN_VERSION` at build time and falls
  back to the crate version. The release workflow passes the release version to the P4
  build (`version` input of `p4-build.yml`), so the installed firmware reports the version
  of the manifest; otherwise an install would end as `rolled_back`. The layout ID given to
  `release.py build` is the one in the header of `partitions.d1001.csv` and in
  `njorun_app_core::ota::PARTITION_LAYOUT`; tests pin all three.
- No image has been written to a tablet; nothing here is hardware evidence.
