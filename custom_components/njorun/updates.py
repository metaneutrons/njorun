# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Firmware updates of the tablets (decision D7): offers, installs and the update policy.

Pure logic without Home Assistant imports. The manager joins the release catalog, the image
cache, the download grants and the runtime's install command. See
docs/protocols/002-ha-management.md#firmware-install.
"""

from __future__ import annotations

import asyncio
from collections.abc import Callable
import logging
import time
from typing import Any

from .firmware import (
    FetchError,
    FirmwareCache,
    Offer,
    ReleaseCatalog,
    ensure_image,
    in_update_window,
    is_newer,
    semver_key,
)
from .firmware_manifest import SEMVER, ManifestError
from .firmware_serve import GrantRegistry
from .install import INSTALL_TTL
from .runtime import NjorunRuntime, RuntimeError

_LOGGER = logging.getLogger(__name__)

REFRESH_INTERVAL = 6 * 60 * 60
MIN_REFRESH_GAP = 5 * 60
DOWNLOAD_PATH = "/api/njorun/firmware"
# Share of the whole install each stage covers, for the update entity's progress bar.
STAGE_RANGE = {"download": (0, 50), "verify": (50, 60), "write": (60, 95), "reboot": (95, 100)}

REASON_TEXT = {
    "unreported": "The tablet does not report its hardware revision and partition layout",
    "board_mismatch": "No release for this board",
    "revision_unsupported": "No release for this hardware revision",
    "layout_mismatch": "The partition layout of this tablet needs a USB reflash",
    "protocol_too_old": "The release needs a newer Njörun integration",
    "coprocessor_unverifiable": "The release needs a coprocessor firmware HA cannot check",
}


class UpdateError(RuntimeError):
    """An install was refused before anything reached the tablet."""


def progress_percentage(install: dict[str, Any] | None) -> int | None:
    """An indicative 0 to 100 over the stages of an open install."""
    if install is None or install["state"] != "accepted":
        return None
    stage = install["stage"]
    if stage is None:
        return 0
    low, high = STAGE_RANGE[stage]
    return low + (high - low) * install["done"] // install["total"]


class FirmwareManager:
    def __init__(
        self,
        runtime: NjorunRuntime,
        catalog: ReleaseCatalog,
        cache: FirmwareCache,
        registry: GrantRegistry,
        base_url: Callable[[], str],
        clock: Callable[[], float] = time.time,
    ) -> None:
        self.runtime = runtime
        self.catalog = catalog
        self.cache = cache
        self.registry = registry
        self._base_url = base_url
        self._clock = clock
        self._refresh_lock = asyncio.Lock()
        self._install_locks: dict[str, asyncio.Lock] = {}
        self._last_refresh = 0.0
        self.last_error: str | None = None
        self._known_version: dict[str, str] = {}
        self._attempted: dict[str, str] = {}

    # --- state read by the update entity -------------------------------------------------

    def settings(self, tablet_id: str) -> dict[str, Any]:
        return self.runtime.configuration.snapshot(tablet_id).settings

    def installed_version(self, tablet_id: str) -> str | None:
        """The running version: live telemetry, the tablet's last report or what was seen last."""
        telemetry = self.runtime.telemetry(tablet_id)
        version = telemetry.get("firmware_version") if telemetry else None
        if version is None:
            status = self.runtime.install_status(tablet_id)
            if status and status["state"] in ("installed", "rolled_back"):
                version = status["reported_version"]
        if isinstance(version, str) and SEMVER.fullmatch(version):
            self._known_version[tablet_id] = version
        return self._known_version.get(tablet_id)

    def latest(self, tablet_id: str) -> Offer | None:
        """The newest verified release of the tablet's channel that fits it."""
        return self.catalog.latest(
            self.settings(tablet_id)["update_channel"], self.runtime.tablet_ota(tablet_id)
        )

    def why_no_update(self, tablet_id: str) -> str | None:
        """A reason text when nothing fits the tablet, None when an offer exists or none known."""
        code = self.catalog.incompatibility(
            self.settings(tablet_id)["update_channel"], self.runtime.tablet_ota(tablet_id)
        )
        return REASON_TEXT.get(code) if code else None

    def install_open(self, tablet_id: str) -> bool:
        """Whether the tablet's latest install still waits for the tablet (grants need this)."""
        status = self.runtime.install_status(tablet_id)
        return status is not None and status["state"] in ("sent", "accepted")

    # --- refresh -------------------------------------------------------------------------

    async def async_refresh(self, *, force: bool = False) -> None:
        """Read GitHub, verify new manifests and prune the cache; rate limited."""
        async with self._refresh_lock:
            if not force and self._clock() - self._last_refresh < MIN_REFRESH_GAP:
                return
            self._last_refresh = self._clock()
            try:
                await self.catalog.refresh()
                self.last_error = None
            except FetchError as err:
                self.last_error = str(err)
                _LOGGER.warning("Could not read the firmware releases: %s", err)
            else:
                await asyncio.to_thread(self.cache.prune, self.registry.versions_in_use())
        for tablet_id in tuple(self.runtime.configuration.tablets):
            self.runtime.async_write_tablet_state(tablet_id)

    # --- install -------------------------------------------------------------------------

    async def async_install(
        self, tablet_id: str, version: str | None = None, *, allow_downgrade: bool = False
    ) -> str:
        """Cache the image, send the install command and return its install ID.

        Without `version` the newest release of the tablet's channel is installed. Anything
        not newer than the running version is refused unless `allow_downgrade` says so.
        """
        lock = self._install_locks.setdefault(tablet_id, asyncio.Lock())
        if lock.locked():
            raise UpdateError("An install of this tablet is already starting")
        async with lock:
            ota = self.runtime.tablet_ota(tablet_id)
            if ota is None:
                raise UpdateError("The tablet does not support firmware updates")
            offer = (
                self.catalog.find(version, ota)
                if version is not None
                else self.latest(tablet_id)
            )
            if offer is None:
                raise UpdateError(
                    f"Release {version} does not fit this tablet"
                    if version is not None
                    else self.why_no_update(tablet_id) or "No update is available"
                )
            installed = self.installed_version(tablet_id)
            if installed is not None:
                if semver_key(offer.version) == semver_key(installed):
                    raise UpdateError(f"Version {installed} is already installed")
                if not is_newer(offer.version, installed) and not allow_downgrade:
                    raise UpdateError(
                        f"{offer.version} is older than the installed {installed}; "
                        "a downgrade needs allow_downgrade"
                    )
            if not self.runtime.ota_offered(tablet_id):
                raise UpdateError("The tablet is not connected or does not support installs")
            try:
                image = await ensure_image(self.catalog.fetcher, self.cache, offer)
            except (FetchError, ManifestError) as err:
                raise UpdateError(f"The image could not be fetched and verified: {err}") from err
            try:
                base = self._base_url().rstrip("/")
            except Exception as err:  # HA raises NoURLAvailableError
                raise UpdateError("Home Assistant has no internal URL to serve the image") from err
            token = self.registry.issue(tablet_id, image, self._clock() + INSTALL_TTL)
            try:
                install_id = await self.runtime.async_send_install(
                    tablet_id,
                    version=offer.version,
                    channel=offer.release.channel,
                    size=image.size,
                    sha256=image.sha256,
                    url=f"{base}{DOWNLOAD_PATH}/{offer.version}/{image.name}",
                    token=token,
                )
            except RuntimeError as err:
                self.registry.revoke(tablet_id)
                raise UpdateError(str(err)) from err
            status = self.runtime.installs.get(tablet_id)
            if status is not None:
                # The grant expires with the command, not a moment before it.
                self.registry.set_expiry(token, status.expires_at)
            return install_id

    # --- policy --------------------------------------------------------------------------

    async def async_apply_policy(self, minutes_now: int) -> list[str]:
        """Install what `update_policy` allows now; returns the tablets it started.

        Only `automatic` tablets, only within two hours after their `update_time`, only a
        release newer than the running version, and each version once per tablet: a failed
        automatic install is not repeated, an administrator decides then.
        """
        started = []
        for tablet_id in tuple(self.runtime.configuration.tablets):
            try:
                settings = self.settings(tablet_id)
            except Exception:  # an invalid stored configuration is not this module's business
                continue
            if settings["update_policy"] != "automatic":
                continue
            if not in_update_window(minutes_now, settings["update_time"]):
                continue
            offer = self.latest(tablet_id)
            installed = self.installed_version(tablet_id)
            if (
                offer is None
                or installed is None
                or not is_newer(offer.version, installed)
                or self._attempted.get(tablet_id) == offer.version
                or not self.runtime.ota_offered(tablet_id)
                or self.install_open(tablet_id)
            ):
                continue
            self._attempted[tablet_id] = offer.version
            try:
                await self.async_install(tablet_id, offer.version)
            except UpdateError as err:
                _LOGGER.warning("Automatic update of %s to %s failed: %s", tablet_id, offer.version, err)
            else:
                started.append(tablet_id)
        return started
