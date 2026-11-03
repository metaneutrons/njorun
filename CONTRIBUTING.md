# Contributing to Njörun

## Current project stage

Njörun is in planning and repository-bootstrap work. It has no firmware, Cargo
workspace, or Home Assistant integration yet, and it has no product build or
test command. Early contributions can improve the plan and repository
documentation. Do not infer implementation support from the roadmap.

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
    fix(repository-check): accept nested documentation links

Do not add AI attribution trailers or generated-by lines.

## Local checks

Install the hook and repository-check tools. On macOS, Homebrew can install
them:

    brew install lefthook gitleaks actionlint
    lefthook install

Before opening a pull request, run:

    python3 scripts/check-repository.py
    actionlint

The repository check validates the planning-stage repository contracts;
actionlint checks GitHub Actions workflows. There are no product build or test
commands yet. Once executable components are added, follow the commands
documented for those components and include the results in the pull request.

## Third-party code and licensing

Do not copy code from Vellum. It is an engineering reference only. Any future
proposal to reuse third-party code must resolve provenance and licensing before
that code is imported.

Njörun is licensed under GPL-3.0-or-later. By submitting a contribution, you
agree that it may be distributed under that license. You retain copyright in
your contribution; no copyright assignment or Contributor License Agreement is
required.
