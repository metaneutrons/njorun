#!/usr/bin/env python3
"""Install digest-pinned Linux CI binaries without executing remote installers."""
import argparse
import gzip
import hashlib
import io
import json
import os
import pathlib
import tarfile
import urllib.request
import zipfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--manifest", type=pathlib.Path,
                    default=pathlib.Path(__file__).with_name("tools.json"))
args = parser.parse_args()

out = pathlib.Path(os.environ["RUNNER_TEMP"]) / "njorun-tools"
out.mkdir(exist_ok=True)
for tool in json.loads(args.manifest.read_text()):
    data = urllib.request.urlopen(tool["url"], timeout=60).read()
    if hashlib.sha256(data).hexdigest() != tool["sha256"]:
        raise SystemExit(f"Checksum mismatch: {tool['name']}")
    if tool["url"].endswith(".tar.gz"):
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
            member = archive.getmember(tool.get("member", tool["name"]))
            if not member.isfile():
                raise SystemExit("Expected regular executable")
            payload = archive.extractfile(member).read()
    elif tool["url"].endswith(".zip"):
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            info = archive.getinfo(tool.get("member", tool["name"]))
            if info.is_dir() or (info.external_attr >> 28) not in (0, 0x8):
                raise SystemExit("Expected regular executable")
            payload = archive.read(info)
    else:
        payload = gzip.decompress(data)
    target = out / tool["name"]
    target.write_bytes(payload)
    target.chmod(0o755)
with open(os.environ["GITHUB_PATH"], "a") as path:
    path.write(str(out) + "\n")
