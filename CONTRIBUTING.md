# Contributing to Njörun

## Current project stage

Njörun is in M0 feasibility work. The Cargo workspace contains a host-only
pairing experiment; firmware and the Home Assistant integration are not yet
implemented. Do not infer product support from this experiment or the roadmap.

Read the [project blueprint](docs/plans/001-embedded-ha-blueprint.md) and
[repository standards](docs/repository-standards.md) before proposing changes.
The blueprint owns product scope, design decisions, dependencies, and normative
acceptance criteria. The linked epic and milestone issues own execution status,
blockers, implementation pull requests, and evidence. Update the blueprint in a
reviewed change when an accepted decision or criterion changes; do not copy its
acceptance checklist into issue or pull request text.

## Changes and pull requests

For substantial work, use the linked epic or milestone issue. Open a focused
issue before starting work when no relevant tracking issue exists. Keep each
pull request to one change kind. Link the relevant plan section and tracking
issue when applicable.

Use a short, lowercase, hyphen-separated branch name prefixed by its change
kind, such as:

    docs/community-files
    fix/repository-check

Pull request titles and commit messages use Conventional Commits. Allowed types
are feat, fix, docs, style, refactor, perf, test, build, ci, chore, and revert.
An optional scope is lowercase. Keep the subject in English and at most 100
characters. Mark breaking changes with an exclamation mark after the type and
document them in the body with a BREAKING CHANGE: footer.

Examples:

    docs: clarify the planning-stage status
    fix: correct a broken plan link

Do not add AI attribution trailers or generated-by lines.

## Local checks

Install the hook and repository-check tools. On macOS, Homebrew can install
them:

    brew install lefthook gitleaks actionlint
    lefthook install

Before opening a pull request, run:

    python3 scripts/check-repository.py
    actionlint

The repository check validates structure and local links; actionlint checks
GitHub Actions workflows. Rustup reads the exact compiler pin from
`rust-toolchain.toml`. Install the host qualification tools:

    cargo install cargo-nextest --version 0.9.150 --locked
    cargo install cargo-llvm-cov --version 0.9.1 --locked
    cargo install cargo-deny --version 0.20.2 --locked

Run the host experiment checks:

    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
    cargo nextest run --workspace --all-features --locked
    cargo llvm-cov --workspace --all-features --locked --ignore-filename-regex '/tests/' --fail-under-lines 90
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
    cargo deny --locked check
    python3 -m unittest discover -s scripts/ci -p 'test_*.py'

CI uses digest-pinned Linux tool archives from `scripts/ci/rust-tools.json`.
The host crate declares no optional feature combinations and is not published.
These checks do not qualify ESP-IDF, the board, or the initial trust ceremony.

## Disposable HA experiment

With Docker running, execute:

    python3 experiments/ha-callback/qualify.py

This pulls the pinned official HA image, creates a disposable instance bound to
loopback and random test credentials, exercises its auth API, then removes its
container and configuration. It does not connect to an existing HA installation.
It reports sanitized results and exits nonzero on probe or cleanup failure.
See the [recorded scope and limitations](docs/research/002-host-experiments.md).
CI checks Python syntax; the live Docker experiment is run deliberately rather
than starting a fresh HA instance on every documentation or Rust change.

## Third-party code and licensing

Do not copy code from Vellum. It is an engineering reference only. Any future
proposal to reuse third-party code must resolve provenance and licensing before
that code is imported.

Njörun is licensed under GPL-3.0-or-later. By submitting a contribution, you
agree that it may be distributed under that license. You retain copyright in
your contribution; no copyright assignment or Contributor License Agreement is
required.
