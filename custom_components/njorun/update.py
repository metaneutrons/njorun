# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Native HA update entity per tablet for its firmware (decision D7)."""

from __future__ import annotations

from typing import Any

import probatio

from homeassistant.components.update import (
    UpdateDeviceClass,
    UpdateEntity,
    UpdateEntityFeature,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant
from homeassistant.exceptions import HomeAssistantError
from homeassistant.helpers import entity_platform
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .entity import NjorunTabletEntity
from .runtime import NjorunRuntime
from .updates import UpdateError, progress_percentage

SERVICE_INSTALL_FIRMWARE = "install_firmware"


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    """Expose the update entity on every tablet that can install firmware."""
    runtime: NjorunRuntime = hass.data[DOMAIN]["entries"][entry.entry_id]
    runtime.register_platform("update", async_add_entities)
    entity_platform.async_get_current_platform().async_register_entity_service(
        SERVICE_INSTALL_FIRMWARE,
        {
            probatio.Optional("version"): str,
            probatio.Optional("allow_downgrade", default=False): bool,
        },
        "async_install_firmware",
    )


def create_entities(runtime: NjorunRuntime, tablet_id: str) -> list[UpdateEntity]:
    if runtime.firmware is None or "ota" not in runtime.configuration.tablets[tablet_id][
        "capabilities"
    ]:
        return []
    return [NjorunFirmwareUpdate(runtime, tablet_id)]


class NjorunFirmwareUpdate(NjorunTabletEntity, UpdateEntity):
    """The firmware of one tablet: what runs, what the tablet's channel offers, the install."""

    _attr_device_class = UpdateDeviceClass.FIRMWARE
    _attr_translation_key = "firmware"
    _attr_supported_features = (
        UpdateEntityFeature.INSTALL
        | UpdateEntityFeature.PROGRESS
        | UpdateEntityFeature.SPECIFIC_VERSION
    )

    def __init__(self, runtime: NjorunRuntime, tablet_id: str) -> None:
        self.runtime = runtime
        self.tablet_id = tablet_id
        self._attr_unique_id = f"{tablet_id}_firmware_update"

    def supported_by(self, capabilities: dict[str, Any]) -> bool:
        return "ota" in capabilities

    @property
    def _manager(self):
        return self.runtime.firmware

    @property
    def available(self) -> bool:
        return self.tablet_id in self.runtime.configuration.tablets

    @property
    def installed_version(self) -> str | None:
        return self._manager.installed_version(self.tablet_id)

    @property
    def latest_version(self) -> str | None:
        offer = self._manager.latest(self.tablet_id)
        # With nothing on offer the entity reports "up to date" instead of "unknown".
        return offer.version if offer is not None else self.installed_version

    @property
    def release_url(self) -> str | None:
        offer = self._manager.latest(self.tablet_id)
        return (offer.release.url or None) if offer is not None else None

    @property
    def in_progress(self) -> bool:
        return self._manager.install_open(self.tablet_id)

    @property
    def update_percentage(self) -> int | None:
        return progress_percentage(self.runtime.install_status(self.tablet_id))

    @property
    def extra_state_attributes(self) -> dict[str, Any]:
        attributes: dict[str, Any] = {}
        install = self.runtime.install_status(self.tablet_id)
        if install is not None:
            attributes["install_state"] = install["state"]
            attributes["install_stage"] = install["stage"]
            if install["reason"]:
                attributes["install_reason"] = install["reason"]
        if (problem := self._manager.why_no_update(self.tablet_id)) is not None:
            attributes["no_update_reason"] = problem
        if self._manager.last_error:
            attributes["release_check_error"] = self._manager.last_error
        return attributes

    async def async_update(self) -> None:
        await self._manager.async_refresh()

    async def async_install(self, version: str | None, backup: bool, **kwargs: Any) -> None:
        """Install the offered release or a chosen newer one; never downgrades."""
        await self.async_install_firmware(version=version, allow_downgrade=False)

    async def async_install_firmware(
        self, version: str | None = None, allow_downgrade: bool = False
    ) -> None:
        try:
            await self._manager.async_install(
                self.tablet_id, version, allow_downgrade=allow_downgrade
            )
        except UpdateError as err:
            raise HomeAssistantError(str(err)) from err
