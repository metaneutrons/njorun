# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Native HA light for a tablet's live backlight (decision D6).

The light shows and changes what the display does now. It is not the persistent `brightness`
setting of the profile, which keeps its own number entity; after a restart or a new configuration
the display returns to that setting.
"""

from __future__ import annotations

from typing import Any

from homeassistant.components.light import ATTR_BRIGHTNESS, ColorMode, LightEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant
from homeassistant.exceptions import HomeAssistantError
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .entity import NjorunTabletEntity
from .runtime import NjorunRuntime


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    """Expose the backlight light on every tablet that reports it."""
    runtime: NjorunRuntime = hass.data[DOMAIN]["entries"][entry.entry_id]
    runtime.register_platform("light", async_add_entities)


def create_entities(runtime: NjorunRuntime, tablet_id: str) -> list[LightEntity]:
    """Build the backlight light for a tablet that reports the live part."""
    live = runtime.configuration.tablets[tablet_id]["capabilities"].get("live", ())
    return [NjorunBacklightLight(runtime, tablet_id)] if "backlight" in live else []


def to_ha_brightness(percent: int) -> int:
    """Device brightness 0 to 100 as HA's 0 to 255."""
    return round(percent * 255 / 100)


def to_device_brightness(value: int) -> int:
    """HA brightness 1 to 255 as the device's 1 to 100."""
    return max(1, round(value * 100 / 255))


class NjorunBacklightLight(NjorunTabletEntity, LightEntity):
    """The tablet's live backlight; unavailable while no session reports it."""

    _attr_translation_key = "backlight"
    _attr_supported_color_modes = {ColorMode.BRIGHTNESS}
    _attr_color_mode = ColorMode.BRIGHTNESS

    def __init__(self, runtime: NjorunRuntime, tablet_id: str) -> None:
        self.runtime = runtime
        self.tablet_id = tablet_id
        self._attr_unique_id = f"{tablet_id}_backlight"

    def supported_by(self, capabilities: dict[str, Any]) -> bool:
        return "backlight" in capabilities.get("live", ())

    def _backlight(self) -> dict[str, Any] | None:
        telemetry = self.runtime.telemetry(self.tablet_id)
        return telemetry["backlight"] if telemetry else None

    @property
    def available(self) -> bool:
        return (
            self.runtime.live_part_offered(self.tablet_id, "backlight")
            and self._backlight() is not None
        )

    @property
    def is_on(self) -> bool | None:
        backlight = self._backlight()
        return None if backlight is None else backlight["on"]

    @property
    def brightness(self) -> int | None:
        backlight = self._backlight()
        return None if backlight is None else to_ha_brightness(backlight["brightness"])

    async def async_turn_on(self, **kwargs: Any) -> None:
        value: dict[str, Any] = {"on": True}
        if ATTR_BRIGHTNESS in kwargs:
            value["brightness"] = to_device_brightness(kwargs[ATTR_BRIGHTNESS])
        await self._send(value)

    async def async_turn_off(self, **kwargs: Any) -> None:
        await self._send({"on": False})

    async def _send(self, value: dict[str, Any]) -> None:
        try:
            await self.runtime.async_send_command(self.tablet_id, "backlight", value)
        except RuntimeError as err:
            raise HomeAssistantError(str(err)) from err
