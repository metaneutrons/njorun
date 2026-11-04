# Repository standards and bootstrap scope

Njörun follows the hardened public-repository profile. The initial repository
contains its blueprint, tracking and community files; it contains no firmware,
Cargo workspace or HA integration yet. Original project-owned work is licensed
GPL-3.0-or-later. No Vellum or vendor code has been imported.

## Planning-stage checks

One cheap Linux CI lane validates repository structure, local Markdown links,
Conventional Commit/PR hygiene, hook configuration, workflow syntax and a full
history secret scan. `CI Success` is the stable required aggregate; workflows
have no path filters. Actions use full commit pins. Dependabot maintains the
Actions inputs. Local lefthook hooks enforce staged guards and secret scanning.

The personal-account repository uses strict up-to-date PR checks and serial
squash merges, with no bypass actors. Merge queue is not available for personal
repositories. Maintainers must recheck the exact head and base immediately before
merging. Release tags are immutable. No code support is inferred from these checks.

## Staged applicability of the hardened standard

| Requirement | Implementation boundary |
| --- | --- |
| Rust/ESP-IDF exact toolchain pins, Cargo lockfile, unsafe boundaries, formatting, Clippy, nextest, coverage floor and cargo-deny | M0, when the real workspace and qualified SDK exist |
| Host/embedded component-aware CI and conservative unknown-change handling | M0, before merging executable product code |
| Python dependency lock, integration tests and HACS validation | M2H, with the integration |
| Stable release-please publication from main, manual promotion of qualified commits from protected develop to versioned beta prereleases, protected release environments, restricted App authentication, signed OTA metadata and firmware | M3/M4, before any release tag |
| SBOM, keyless distribution signatures, attestations, clean-install qualification, staged release and published-byte readback | M4, before publication |

These are deferred implementation gates, not waived guarantees. Empty Cargo
packages, arbitrary compiler pins, inactive publish workflows and unused signing
secrets would not demonstrate them. Therefore none are created in the planning
bootstrap. Add language-specific dependency update entries with their manifests.
No release environment is needed until publication is implemented; credentials
must then live only in the protected environment. Firmware signature verification
on the device is separate from GitHub attestations.

Use the [blueprint](plans/001-embedded-ha-blueprint.md#public-repository-standard)
and its linked milestone issues for acceptance. This document records application
of the standard; it is not a second execution ledger.
