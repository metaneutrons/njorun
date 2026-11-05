#!/usr/bin/env python3
"""Select host qualification conservatively; documentation changes remain cheap."""
import argparse
import pathlib
import subprocess


def needs_host(paths):
    """Unknown inputs require qualification; only known prose is exempt."""
    return any(not (path.endswith(".md") or path == "LICENSE") for path in paths)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    result = subprocess.run(
        ["git", "diff", "--name-only", "--no-renames", "-z", args.base, args.head, "--"],
        capture_output=True, check=False,
    ) if args.base else None
    host = True
    if result is not None and result.returncode == 0:
        paths = result.stdout.decode().split("\0")
        host = needs_host(path for path in paths if path)
    with args.output.open("a") as output:
        output.write(f"host={str(host).lower()}\n")


if __name__ == "__main__":
    main()
