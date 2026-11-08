#!/usr/bin/env python3
"""Validate repository contracts and declared executable component boundaries."""
import pathlib
import re
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parent.parent
files = subprocess.check_output(
    ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=root
).decode().split("\0")
errors = []
required = ["README.md", "LICENSE", "SECURITY.md", "CONTRIBUTING.md", ".editorconfig",
            ".gitignore", ".github/CODEOWNERS", ".github/dependabot.yml",
            "docs/plans/001-embedded-ha-blueprint.md", "docs/repository-standards.md",
            "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "deny.toml",
            "LICENSES/README.md", "LICENSES/Apache-2.0.txt"]
for name in required:
    if not (root / name).is_file():
        errors.append(f"Missing required file: {name}")
for name in filter(None, files):
    path = root / name
    # `git ls-files --cached` includes deleted paths until the change is staged.
    # Required files are checked separately above; ignore other absent paths.
    if not path.exists():
        continue
    if path.stat().st_size > 5 * 1024 * 1024:
        errors.append(f"Oversized tracked file: {name}")
    if any(part in {"target", "build", "dist", "node_modules", "coverage", "__pycache__"} for part in path.relative_to(root).parts):
        errors.append(f"Generated output tracked: {name}")
    # Host components share the workspace gate; the isolated P4 experiment has
    # its own CI lane comparing two clean builds of the shared application core.
    host_components = ("crates/pairing/", "crates/management/", "crates/app-core/",
                       "crates/ha-client/", "crates/simulator/")
    p4_components = "experiments/p4-build/components/njorun_d1001/"
    unqualified_native = (
        path.suffix in {".c", ".cpp"} and not name.startswith(p4_components)
    ) or (path.suffix == ".slint" and not name.startswith("crates/simulator/ui/"))
    unqualified_rust = path.suffix == ".rs" and not name.startswith(
        (*host_components, "experiments/p4-build/"))
    unqualified_manifest = path.name == "Cargo.toml" and name not in {
        "Cargo.toml", "crates/pairing/Cargo.toml", "crates/management/Cargo.toml",
        "crates/app-core/Cargo.toml",
        "crates/ha-client/Cargo.toml", "crates/simulator/Cargo.toml",
        "experiments/p4-build/Cargo.toml"}
    if unqualified_native or unqualified_rust or unqualified_manifest or (
            name.startswith("custom_components/") and not name.startswith("custom_components/njorun/")):
        errors.append(f"Extend CI before adding another executable component: {name}")
    if path.suffix == ".md":
        text = path.read_text()
        for target in re.findall(r"\]\(([^)]+)\)", text):
            target = target.split("#", 1)[0]
            if not target or "://" in target or target.startswith("mailto:"):
                continue
            resolved = (path.parent / target).resolve()
            if not resolved.is_relative_to(root) or not resolved.exists():
                errors.append(f"Broken or external local link in {name}: {target}")
plan = (root / "docs/plans/001-embedded-ha-blueprint.md").read_text()
ids = re.findall(r"^- (M[0-9]+[A-Z]?-A[0-9]+):", plan, re.M)
if len(ids) != len(set(ids)) or len(ids) < 25:
    errors.append("Blueprint criterion IDs are missing or duplicated")

license_scope = (root / "LICENSES/README.md").read_text()
if "`custom_components/njorun/**` | Apache License 2.0 (`Apache-2.0`)" not in license_scope:
    errors.append("Home Assistant integration Apache-2.0 scope is missing")
if "Project-owned Rust, Slint and embedded firmware sources" not in license_scope:
    errors.append("Rust/display GPL-3.0-or-later scope is missing")

apache_header = "# SPDX-License-Identifier: Apache-2.0"
for path in sorted((root / "custom_components/njorun").glob("*.py")):
    if apache_header not in path.read_text().splitlines()[:5]:
        errors.append(f"Missing Apache-2.0 SPDX header: {path.relative_to(root)}")

if 'license = "GPL-3.0-or-later"' not in (root / "Cargo.toml").read_text():
    errors.append("Rust workspace license must be GPL-3.0-or-later")
if 'license = "GPL-3.0-or-later"' not in (
        root / "experiments/p4-build/Cargo.toml").read_text():
    errors.append("P4 firmware experiment license must be GPL-3.0-or-later")
if errors:
    print("\n".join(errors), file=sys.stderr)
    sys.exit(1)
print("Repository checks passed")
