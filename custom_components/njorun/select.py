# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Native HA select entities for finite Njörun settings."""

from __future__ import annotations

from typing import Any

from homeassistant.components.select import SelectEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .model import CHOICES
from .runtime import NjorunRuntime

_SETTING_KEYS = (
    "orientation",
    "theme",
    "language",
    "screensaver",
    "home_view",
    "details",
    "update_policy",
    "update_channel",
)


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    """Expose one native select for each finite preference on each tablet."""
    runtime: NjorunRuntime = hass.data[DOMAIN]["entries"][entry.entry_id]
    runtime.register_platform("select", async_add_entities)


def create_entities(runtime: NjorunRuntime, tablet_id: str) -> list[SelectEntity]:
    """Build stable select entities for a logical tablet."""
    return [
        NjorunSettingSelect(runtime, tablet_id, key)
        for key in _SETTING_KEYS
        if key in runtime.configuration.tablets[tablet_id]["overrides"]
        or key in runtime.configuration.snapshot(tablet_id).settings
    ]


class NjorunSettingSelect(SelectEntity):
    """A select whose state follows durable device-reported values only."""

    _attr_has_entity_name = True
    _attr_entity_category = EntityCategory.CONFIG
    _attr_should_poll = False

    def __init__(self, runtime: NjorunRuntime, tablet_id: str, key: str) -> None:
        self.runtime = runtime
        self.tablet_id = tablet_id
        self.key = key
        self._attr_unique_id = f"{tablet_id}_{key}"
        self._attr_name = key.replace("_", " ").capitalize()
        self._attr_translation_key = key

    @property
    def device_info(self) -> dict[str, Any]:
        tablet = self.runtime.configuration.tablets.get(self.tablet_id, {})
        return {
            "identifiers": {(DOMAIN, self.tablet_id)},
            "name": tablet.get("name", self.tablet_id),
            "manufacturer": "Njörun",
            "model": "Touchscreen",
        }

    @property
    def options(self) -> list[str]:
        return list(CHOICES[self.key])

    @property
    def current_option(self) -> str | None:
        record = self.runtime.applied.get(self.tablet_id)
        if record is None:
            return None
        value = record["settings"].get(self.key)
        return value if isinstance(value, str) else None

    @property
    def available(self) -> bool:
        return self.runtime.is_available(self.tablet_id)

    async def async_select_option(self, option: str) -> None:
        """Persist a desired override and dispatch it without optimistic state."""
        if option not in CHOICES[self.key]:
            raise ValueError("Unsupported Njörun select option")
        await self.runtime.async_set_setting(self.tablet_id, self.key, option)

    async def async_added_to_hass(self) -> None:
        await super().async_added_to_hass()
        self.runtime.add_entity(self.tablet_id, self)

    async def async_will_remove_from_hass(self) -> None:
        self.runtime.remove_entity(self.tablet_id, self)
        await super().async_will_remove_from_hass()
