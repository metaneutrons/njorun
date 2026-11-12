# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Native HA number entities for bounded Njörun settings."""

from __future__ import annotations

import math

from typing import Any

from homeassistant.components.number import NumberEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .entity import NjorunSettingEntity
from .model import RANGES
from .runtime import NjorunRuntime

_SETTING_KEYS = ("brightness", "idle_brightness", "idle_timeout", "ui_scale")
_UNITS = {"ui_scale": "%"}


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    """Expose native bounded number entities on capable tablets."""
    runtime: NjorunRuntime = hass.data[DOMAIN]["entries"][entry.entry_id]
    runtime.register_platform("number", async_add_entities)


def create_entities(runtime: NjorunRuntime, tablet_id: str) -> list[NumberEntity]:
    """Build stable number entities, omitting brightness controls if unsupported."""
    capabilities = runtime.configuration.tablets[tablet_id]["capabilities"]
    return [
        NjorunSettingNumber(runtime, tablet_id, key)
        for key in _SETTING_KEYS
        if key not in {"brightness", "idle_brightness"} or capabilities["brightness"]
    ]


class NjorunSettingNumber(NjorunSettingEntity, NumberEntity):
    """A number entity whose state is the last persisted device ACK."""

    _attr_entity_category = EntityCategory.CONFIG

    def __init__(self, runtime: NjorunRuntime, tablet_id: str, key: str) -> None:
        self.runtime = runtime
        self.tablet_id = tablet_id
        self.key = key
        self._attr_unique_id = f"{tablet_id}_{key}"
        self._attr_name = key.replace("_", " ").capitalize()
        self._attr_translation_key = key
        low, high = RANGES[key]
        self._attr_native_min_value = low
        self._attr_native_max_value = high
        self._attr_native_step = 1
        self._attr_native_unit_of_measurement = _UNITS.get(key)

    def supported_by(self, capabilities: dict[str, Any]) -> bool:
        return self.key not in {"brightness", "idle_brightness"} or capabilities["brightness"]

    @property
    def native_value(self) -> float | None:
        record = self.runtime.applied.get(self.tablet_id)
        if record is None:
            return None
        value = record["settings"].get(self.key)
        return float(value) if type(value) is int else None

    async def async_set_native_value(self, value: float) -> None:
        """Persist a desired integer override; wait for the device ACK to publish."""
        if not isinstance(value, (int, float)) or not math.isfinite(value):
            raise ValueError("Invalid Njörun number")
        integer = int(value)
        if integer != value:
            raise ValueError("Njörun setting requires an integer")
        await self.runtime.async_set_setting(self.tablet_id, self.key, integer)
