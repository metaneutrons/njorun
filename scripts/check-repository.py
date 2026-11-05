#!/usr/bin/env python3
"""Validate repository contracts and the bounded M0 prototype boundary."""
import pathlib
import re
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parent.parent
files = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).decode().split("\0")
errors = []
required = ["README.md", "LICENSE", "SECURITY.md", "CONTRIBUTING.md", ".editorconfig",
            ".gitignore", ".github/CODEOWNERS", ".github/dependabot.yml",
            "docs/plans/001-embedded-ha-blueprint.md", "docs/repository-standards.md",
            "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "deny.toml"]
for name in required:
    if not (root / name).is_file():
        errors.append(f"Missing required file: {name}")
for name in filter(None, files):
    path = root / name
    if path.stat().st_size > 5 * 1024 * 1024:
        errors.append(f"Oversized tracked file: {name}")
    if any(part in {"target", "build", "dist", "node_modules", "coverage", "__pycache__"} for part in path.relative_to(root).parts):
        errors.append(f"Generated output tracked: {name}")
    unqualified_native = path.suffix in {".slint", ".c", ".cpp"}
    unqualified_rust = path.suffix == ".rs" and not name.startswith("crates/pairing/")
    unqualified_manifest = path.name == "Cargo.toml" and name not in {
        "Cargo.toml", "crates/pairing/Cargo.toml"}
    if unqualified_native or unqualified_rust or unqualified_manifest or name.startswith("custom_components/"):
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
if errors:
    print("\n".join(errors), file=sys.stderr)
    sys.exit(1)
print("Repository checks passed")
