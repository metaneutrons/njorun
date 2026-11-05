# Repository standards and bootstrap scope

Njörun follows the hardened public-repository profile. The repository
contains its blueprint, host pairing/core/client crates, a native simulator,
and a development HA integration. Their qualification is in progress; no
D1001 firmware release is available. Original project-owned work is licensed
GPL-3.0-or-later. No Vellum or vendor code has been imported.

## Current checks

One cheap Linux CI lane validates repository structure, local Markdown links,
Conventional Commit/PR hygiene, hook configuration, workflow syntax and a full
history secret scan. `CI Success` is the stable required aggregate; workflows
have no path filters. Actions use full commit pins. Dependabot maintains the
Actions, Cargo, uv and Rust toolchain inputs. Local lefthook hooks enforce staged
guards, formatting and secret scanning; pre-push checks run Clippy and nextest.

A tested classifier exempts documentation-only changes from host checks and
qualifies unknown changes conservatively. The Linux host lane enforces formatting,
Clippy, nextest, a 90% application line coverage floor (excluding test bodies),
rustdoc and cargo-deny. Coverage includes unit tests, native UI qualification,
and the instrumented simulator's disposable HA scenario. Rust 1.99.1 was
selected from the stable distribution manifest for this host experiment; this is
not an ESP-IDF compiler qualification. The committed lockfile and digest-pinned
CI tools make its dependency inputs explicit. The expanded host job builds the
native simulator, installs the locked Python environment using a digest-pinned
uv binary, runs Python lint/model tests, and executes the disposable HA scenario.
The workflow changes still require an actual Linux CI run. A separate P4 lane
builds the shared application core twice from clean build outputs and compares
the complete ELFs byte-for-byte. This permits the isolated experiment in the
component allowlist without claiming board runtime qualification.
Both Cargo workspaces undergo dependency/license/source audits. The P4 workspace
explicitly declares the project license and exact `esp-idf-sys` version; its
upstream Git source is allowlisted individually and Git dependencies require
revision pins, following the [cargo-deny source policy](https://embarkstudios.github.io/cargo-deny/checks/sources/cfg.html).

The personal-account repository uses strict up-to-date PR checks and serial
squash merges, with no bypass actors. Merge queue is not available for personal
repositories. Maintainers must recheck the exact head and base immediately before
merging. Release tags are immutable. No code support is inferred from these checks.

## Staged applicability of the hardened standard

| Requirement | Implementation boundary |
| --- | --- |
| Host Rust pins, Cargo lockfile, unsafe prohibition, formatting, Clippy, nextest, coverage floor and cargo-deny | Active for the host workspace; generated Slint bindings have a package-local scoped unsafe allowance |
| ESP-IDF toolchain pins, FFI unsafe boundaries and hardware qualification | M0, before embedded code is integrated |
| Host component selection and conservative unknown-change handling | Active; embedded lanes must be added with their components |
| Python dependency lock, lint/model tests and real HA scenario | Verified in [Linux CI](https://github.com/metaneutrons/njorun/actions/runs/41977188586) |
| HACS/hassfest packaging validation | Still required before integration publication |
| Stable release-please publication from main, manual promotion of qualified commits from protected develop to versioned beta prereleases, protected release environments, restricted App authentication, signed OTA metadata and firmware | M3/M4, before any release tag |
| SBOM, keyless distribution signatures, attestations, clean-install qualification, staged release and published-byte readback | M4, before publication |

These are deferred implementation gates, not waived guarantees. Empty Cargo
packages, arbitrary compiler pins, inactive publish workflows and unused signing
secrets would not demonstrate them. The host experiment activates only the applicable gates. Add language-specific
dependency update entries with their manifests.
No release environment is needed until publication is implemented; credentials
must then live only in the protected environment. Firmware signature verification
on the device is separate from GitHub attestations.

Use the [blueprint](plans/001-embedded-ha-blueprint.md#public-repository-standard)
and its linked milestone issues for acceptance. This document records application
of the standard; it is not a second execution ledger.
