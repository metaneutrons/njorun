#!/usr/bin/env python3
"""Build and open isolated portrait and landscape first-run fixtures on macOS."""

from __future__ import annotations

import os
from pathlib import Path
import plistlib
import shutil
import signal
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "target" / "first-run-demo"
BINARY = ROOT / "target" / "debug" / "examples" / "first_run"


def stop_previous_instances() -> None:
    result = subprocess.run(
        ["pgrep", "-f", "/target/first-run-demo/.*/Contents/MacOS/first_run"],
        capture_output=True,
        check=False,
        text=True,
    )
    for value in result.stdout.splitlines():
        pid = int(value)
        command = subprocess.run(
            ["ps", "-p", str(pid), "-o", "command="],
            capture_output=True,
            check=False,
            text=True,
        ).stdout
        if str(OUTPUT) in command:
            os.kill(pid, signal.SIGTERM)


def create_app(orientation: str, label: str) -> Path:
    app = OUTPUT / f"Njörun Erststart – {label}.app"
    executable = app / "Contents" / "MacOS" / "first_run"
    executable.parent.mkdir(parents=True, exist_ok=True)
    executable.unlink(missing_ok=True)
    shutil.copy2(BINARY, executable)
    info = {
        "CFBundleDisplayName": f"Njörun Erststart – {label}",
        "CFBundleName": f"Njörun Erststart – {label}",
        "CFBundleIdentifier": f"dev.njorun.simulator.first-run.{orientation}",
        "CFBundleExecutable": "first_run",
        "CFBundlePackageType": "APPL",
        "CFBundleVersion": "1",
        "NSHighResolutionCapable": True,
        "LSEnvironment": {
            "NJORUN_PHYSICAL_PREVIEW": "1",
            "SLINT_SCALE_FACTOR": "1",
        },
    }
    (app / "Contents" / "Info.plist").write_bytes(plistlib.dumps(info))
    subprocess.run(["codesign", "--force", "--sign", "-", str(app)], check=True)
    return app


def main() -> int:
    if sys.platform != "darwin":
        print("This app-bundle launcher requires macOS", file=sys.stderr)
        return 2
    subprocess.run(
        [
            "cargo",
            "build",
            "-p",
            "njorun-simulator",
            "--features",
            "qualification",
            "--example",
            "first_run",
            "--locked",
        ],
        cwd=ROOT,
        check=True,
    )
    stop_previous_instances()
    OUTPUT.mkdir(parents=True, exist_ok=True)
    apps = [
        (orientation, create_app(orientation, label))
        for orientation, label in (("portrait", "Hochformat"), ("landscape", "Querformat"))
    ]
    for orientation, app in apps:
        state = OUTPUT / f"{orientation}-state.json"
        state.unlink(missing_ok=True)
        subprocess.run(
            ["open", "-n", str(app), "--args", orientation, str(state), "fixture"],
            check=True,
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
