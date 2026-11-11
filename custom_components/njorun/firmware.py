# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Firmware releases for the D1001 (decision D7): discovery, verification and the image cache.

Pure logic without Home Assistant imports. Home Assistant is the only party that talks to
GitHub. A release counts only after its manifest verified against the build-time keys; an
image counts only after its size and SHA-256 match the signed entry. See
docs/protocols/003-firmware-manifest.md.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field
import hashlib
import json
import logging
import os
from pathlib import Path
import re
import shutil
from typing import Any, Protocol

from .firmware_manifest import (
    SEMVER,
    ManifestError,
    check_artifact,
    select_board,
    verify,
)

_LOGGER = logging.getLogger(__name__)

REPOSITORY = "metaneutrons/njorun"
API_URL = "https://api.github.com"
DOWNLOAD_PREFIX = "https://github.com/{repo}/releases/download/"
MANIFEST_NAME = "manifest.json"
SIGNATURE_NAME = "manifest.json.sig"
MAX_RELEASES = 20
MAX_IMAGE_BYTES = 32 * 1024 * 1024
KEEP_VERSIONS = 3
MIN_PROTOCOL = 1


class FetchError(Exception):
    """GitHub could not be reached or answered with something unusable."""


class Fetcher(Protocol):
    async def fetch(self, url: str, *, limit: int, json_api: bool = False) -> bytes:
        """The response body of `url`, at most `limit` bytes."""


class AiohttpFetcher:
    """The only network code of the module: HTTPS GET with a size limit."""

    def __init__(self, session: Any) -> None:
        self._session = session

    async def fetch(self, url: str, *, limit: int, json_api: bool = False) -> bytes:
        import aiohttp

        headers = {"User-Agent": "njorun-home-assistant"}
        if json_api:
            headers["Accept"] = "application/vnd.github+json"
            headers["X-GitHub-Api-Version"] = "2022-11-28"
        try:
            async with self._session.get(
                url, headers=headers, timeout=aiohttp.ClientTimeout(total=300)
            ) as response:
                if response.status != 200:
                    raise FetchError(f"{url} answered {response.status}")
                declared = response.content_length
                if declared is not None and declared > limit:
                    raise FetchError(f"{url} is larger than {limit} bytes")
                body = bytearray()
                async for chunk in response.content.iter_chunked(65536):
                    body += chunk
                    if len(body) > limit:
                        raise FetchError(f"{url} is larger than {limit} bytes")
                return bytes(body)
        except (aiohttp.ClientError, TimeoutError) as err:
            raise FetchError(f"{url}: {err}") from err


def semver_key(version: str) -> tuple:
    """SemVer 2.0.0 precedence as a sortable key (a release sorts above its prereleases)."""
    match = SEMVER.fullmatch(version)
    if match is None:
        raise ValueError(f"not a version: {version!r}")
    core = tuple(int(match.group(i)) for i in (1, 2, 3))
    if match.group(4) is None:
        return (core, 1, ())
    identifiers = []
    for part in match.group(4)[1:].split("."):
        # Numeric identifiers sort below alphanumeric ones and by value.
        identifiers.append((0, int(part), "") if part.isdigit() else (1, 0, part))
    return (core, 0, tuple(identifiers))


def is_newer(candidate: str, installed: str) -> bool:
    return semver_key(candidate) > semver_key(installed)


@dataclass(frozen=True)
class Release:
    """A GitHub release whose manifest verified."""

    version: str
    channel: str
    manifest_bytes: bytes
    signature: bytes
    manifest: dict[str, Any]
    assets: dict[str, str]
    url: str
    published: str


@dataclass(frozen=True)
class Offer:
    """A release that fits one tablet: its board entry and the image file name."""

    release: Release
    entry: dict[str, Any]

    @property
    def version(self) -> str:
        return self.release.version

    @property
    def app(self) -> dict[str, Any]:
        return self.entry["artifacts"]["app"]


def entry_for(release: Release, ota: dict[str, str] | None) -> dict[str, Any]:
    """The manifest entry for the tablet, or ManifestError naming why it does not fit.

    The tablet must report board, hardware revision and partition layout; an update is
    never offered on a guess. Codes beyond the specification's: `layout_mismatch`,
    `unreported`, `protocol_too_old` and `coprocessor_unverifiable`.
    """
    if not ota:
        raise ManifestError("unreported", "the tablet reports no ota capability")
    if "hardware_revision" not in ota or "partition_layout" not in ota:
        raise ManifestError("unreported", "the tablet reports no hardware revision or layout")
    entry = select_board(release.manifest, ota["board"], ota["hardware_revision"])
    if entry["partition_layout"] != ota["partition_layout"]:
        raise ManifestError("layout_mismatch", entry["partition_layout"])
    if entry["min_integration_protocol"] > MIN_PROTOCOL:
        raise ManifestError("protocol_too_old", str(entry["min_integration_protocol"]))
    if entry["coprocessor"]["min_firmware"] is not None:
        # The tablet does not report the coprocessor firmware yet, so a demand cannot be met.
        raise ManifestError("coprocessor_unverifiable", entry["coprocessor"]["chip"])
    return entry


class ReleaseCatalog:
    """The verified releases of the repository, refreshed from GitHub."""

    def __init__(self, fetcher: Fetcher, keys: list[bytes], repository: str = REPOSITORY) -> None:
        self._fetcher = fetcher
        self._keys = keys
        self._repository = repository
        self._verified: dict[str, Release | None] = {}
        self.fetcher = fetcher
        self.releases: list[Release] = []
        self.problems: list[str] = []

    async def refresh(self) -> None:
        """Read the release list and verify every manifest not seen before.

        A release whose manifest does not verify is skipped and noted in `problems`; it never
        replaces an older good one silently in the log. A failing list request raises
        FetchError and keeps the previous result.
        """
        body = await self._fetcher.fetch(
            f"{API_URL}/repos/{self._repository}/releases?per_page={MAX_RELEASES}",
            limit=2 * 1024 * 1024,
            json_api=True,
        )
        try:
            listing = json.loads(body)
        except ValueError as err:
            raise FetchError("the release list is not JSON") from err
        if not isinstance(listing, list):
            raise FetchError("the release list is not a list")
        releases: list[Release] = []
        problems: list[str] = []
        seen = set()
        for item in listing[:MAX_RELEASES]:
            if not isinstance(item, dict) or item.get("draft") is True:
                continue
            tag = item.get("tag_name")
            assets = self._assets(item)
            if (
                not isinstance(tag, str)
                or not tag.startswith("v")
                or MANIFEST_NAME not in assets
                or SIGNATURE_NAME not in assets
            ):
                continue
            key = f"{tag}|{assets[MANIFEST_NAME]}|{assets[SIGNATURE_NAME]}"
            seen.add(key)
            if key not in self._verified:
                try:
                    self._verified[key] = await self._load(item, tag, assets)
                except ManifestError as err:
                    problems.append(f"{tag}: {err}")
                    _LOGGER.warning("Release %s rejected: %s", tag, err)
                    self._verified[key] = None
                except FetchError as err:
                    problems.append(f"{tag}: {err}")
                    _LOGGER.warning("Release %s not read: %s", tag, err)
                    continue  # try again at the next refresh
            if (release := self._verified[key]) is not None:
                releases.append(release)
        for key in set(self._verified) - seen:
            del self._verified[key]
        releases.sort(key=lambda release: semver_key(release.version), reverse=True)
        self.releases, self.problems = releases, problems

    def _assets(self, item: dict[str, Any]) -> dict[str, str]:
        """File name to download URL, only for files GitHub serves for this repository."""
        prefix = DOWNLOAD_PREFIX.format(repo=self._repository)
        result = {}
        for asset in item.get("assets") or ():
            if not isinstance(asset, dict):
                continue
            name, url = asset.get("name"), asset.get("browser_download_url")
            if isinstance(name, str) and isinstance(url, str) and url.startswith(prefix):
                result[name] = url
        return result

    async def _load(self, item: dict[str, Any], tag: str, assets: dict[str, str]) -> Release:
        manifest_bytes = await self._fetcher.fetch(assets[MANIFEST_NAME], limit=64 * 1024)
        signature = await self._fetcher.fetch(assets[SIGNATURE_NAME], limit=1024)
        manifest = verify(manifest_bytes, signature, self._keys)
        release = manifest["release"]
        if tag != f"v{release['version']}":
            raise ManifestError("malformed", "the tag does not name the manifest's version")
        # The channel is signed; the GitHub flag only has to agree with it.
        if (release["channel"] == "beta") != bool(item.get("prerelease")):
            raise ManifestError("channel_mismatch", "the release flag disagrees with the manifest")
        for board in manifest["boards"]:
            if board["artifacts"]["app"]["name"] not in assets:
                raise ManifestError("malformed", "the release lacks an image the manifest names")
        url = item.get("html_url")
        return Release(
            version=release["version"],
            channel=release["channel"],
            manifest_bytes=manifest_bytes,
            signature=signature,
            manifest=manifest,
            assets=assets,
            url=url if isinstance(url, str) and url.startswith("https://github.com/") else "",
            published=str(item.get("published_at") or ""),
        )

    def offers(self, ota: dict[str, str] | None) -> list[Offer]:
        """Every verified release that fits the tablet, newest first, any channel."""
        result = []
        for release in self.releases:
            try:
                result.append(Offer(release, entry_for(release, ota)))
            except ManifestError:
                continue
        return result

    def latest(self, channel: str, ota: dict[str, str] | None) -> Offer | None:
        """The newest release of the tablet's channel that fits it."""
        for offer in self.offers(ota):
            if offer.release.channel == channel:
                return offer
        return None

    def find(self, version: str, ota: dict[str, str] | None) -> Offer | None:
        for offer in self.offers(ota):
            if offer.version == version:
                return offer
        return None

    def incompatibility(self, channel: str, ota: dict[str, str] | None) -> str | None:
        """Why no release of the channel fits the tablet (a reason code), None if one does."""
        reasons = []
        for release in self.releases:
            if release.channel != channel:
                continue
            try:
                entry_for(release, ota)
                return None
            except ManifestError as err:
                reasons.append(err.code)
        return reasons[0] if reasons else None


@dataclass(frozen=True)
class CachedImage:
    """A verified release on disk: the exact manifest and signature bytes and the image."""

    version: str
    manifest_bytes: bytes
    signature: bytes
    image_path: Path
    name: str
    size: int
    sha256: str
    files: dict[str, Path] = field(default_factory=dict)


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


class FirmwareCache:
    """Verified releases below one directory, `<root>/<version>/{manifest,signature,images}`.

    Everything read back is checked again: the manifest signature and the image digest.
    A file that fails is deleted. All methods block; call them in an executor.
    """

    def __init__(self, root: Path, keys: list[bytes], keep: int = KEEP_VERSIONS) -> None:
        self._root = root
        self._keys = keys
        self._keep = keep

    def _directory(self, version: str) -> Path:
        if SEMVER.fullmatch(version) is None:
            raise ValueError("not a version")
        return self._root / version

    def store(self, release: Release, entry: dict[str, Any], image: bytes) -> CachedImage:
        """Write manifest, signature and image (each atomic). The image must already match."""
        check_artifact(entry, "app", image)
        directory = self._directory(release.version)
        directory.mkdir(parents=True, exist_ok=True)
        name = entry["artifacts"]["app"]["name"]
        for file_name, data in (
            (name, image),
            (MANIFEST_NAME, release.manifest_bytes),
            (SIGNATURE_NAME, release.signature),
        ):
            temporary = directory / f".{file_name}.part"
            temporary.write_bytes(data)
            os.replace(temporary, directory / file_name)
        cached = self.load(release.version, entry)
        if cached is None:
            raise ManifestError("digest_mismatch", "the cached image did not read back")
        return cached

    def load(self, version: str, entry: dict[str, Any]) -> CachedImage | None:
        """The verified cache entry, or None (a failing file is removed)."""
        directory = self._directory(version)
        artifact = entry["artifacts"]["app"]
        paths = {
            MANIFEST_NAME: directory / MANIFEST_NAME,
            SIGNATURE_NAME: directory / SIGNATURE_NAME,
            artifact["name"]: directory / artifact["name"],
        }
        if not all(path.is_file() for path in paths.values()):
            return None
        try:
            manifest_bytes = paths[MANIFEST_NAME].read_bytes()
            signature = paths[SIGNATURE_NAME].read_bytes()
            manifest = verify(manifest_bytes, signature, self._keys)
            image = paths[artifact["name"]]
            if (
                manifest["release"]["version"] != version
                or image.stat().st_size != artifact["size"]
                or _sha256_file(image) != artifact["sha256"]
            ):
                raise ManifestError("digest_mismatch", artifact["name"])
        except (ManifestError, OSError) as err:
            _LOGGER.warning("Cached firmware %s discarded: %s", version, err)
            shutil.rmtree(directory, ignore_errors=True)
            return None
        return CachedImage(
            version,
            manifest_bytes,
            signature,
            image,
            artifact["name"],
            artifact["size"],
            artifact["sha256"],
            paths,
        )

    def prune(self, protect: set[str] = frozenset()) -> list[str]:
        """Keep the newest versions and every protected one; returns the removed versions."""
        if not self._root.is_dir():
            return []
        found = []
        for child in self._root.iterdir():
            if child.is_dir() and SEMVER.fullmatch(child.name):
                found.append(child.name)
            elif child.is_dir():
                shutil.rmtree(child, ignore_errors=True)
        found.sort(key=semver_key, reverse=True)
        removed = []
        for version in found[self._keep :]:
            if version not in protect:
                shutil.rmtree(self._root / version, ignore_errors=True)
                removed.append(version)
        return removed


async def ensure_image(
    fetcher: Fetcher, cache: FirmwareCache, offer: Offer
) -> CachedImage:
    """The offer's image from the cache, downloaded and verified first if needed."""
    cached = await asyncio.to_thread(cache.load, offer.version, offer.entry)
    if cached is not None:
        return cached
    app = offer.app
    url = offer.release.assets.get(app["name"])
    if url is None:
        raise FetchError(f"the release has no file {app['name']}")
    if app["size"] > MAX_IMAGE_BYTES:
        raise ManifestError("oversize", str(app["size"]))
    data = await fetcher.fetch(url, limit=app["size"])
    # check_artifact runs again inside store; failing here keeps the message precise.
    check_artifact(offer.entry, "app", data)
    return await asyncio.to_thread(cache.store, offer.release, offer.entry, data)


def in_update_window(minutes_now: int, update_time: str, hours: int = 2) -> bool:
    """Whether the local time (minutes since midnight) is within `hours` after `update_time`."""
    match = re.fullmatch(r"([01][0-9]|2[0-3]):([0-5][0-9])", update_time)
    if match is None:
        return False
    start = int(match.group(1)) * 60 + int(match.group(2))
    return (minutes_now - start) % (24 * 60) < hours * 60
