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
    else:
        payload = gzip.decompress(data)
    target = out / tool["name"]
    target.write_bytes(payload)
    target.chmod(0o755)
with open(os.environ["GITHUB_PATH"], "a") as path:
    path.write(str(out) + "\n")
