# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Native HA switches for boolean Njörun settings."""

from __future__ import annotations

from homeassistant.components.switch import SwitchEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .entity import NjorunSettingEntity
from .runtime import NjorunRuntime

_SETTING_KEYS = ("increased_contrast", "reduced_motion")


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    """Expose a native switch for each boolean preference on every tablet."""
    runtime: NjorunRuntime = hass.data[DOMAIN]["entries"][entry.entry_id]
    runtime.register_platform("switch", async_add_entities)


def create_entities(runtime: NjorunRuntime, tablet_id: str) -> list[SwitchEntity]:
    """Build stable switch entities for the tablet's boolean settings."""
    return [NjorunSettingSwitch(runtime, tablet_id, key) for key in _SETTING_KEYS]


class NjorunSettingSwitch(NjorunSettingEntity, SwitchEntity):
    """A switch whose state follows the tablet's last acknowledged setting."""

    _attr_entity_category = EntityCategory.CONFIG

    def __init__(self, runtime: NjorunRuntime, tablet_id: str, key: str) -> None:
        self.runtime = runtime
        self.tablet_id = tablet_id
        self.key = key
        self._attr_unique_id = f"{tablet_id}_{key}"
        self._attr_translation_key = key

    @property
    def is_on(self) -> bool | None:
        record = self.runtime.applied.get(self.tablet_id)
        if record is None:
            return None
        value = record["settings"].get(self.key)
        return value if type(value) is bool else None

    async def async_turn_on(self, **kwargs: object) -> None:
        """Persist the desired value; wait for the tablet ACK before publishing it."""
        await self.runtime.async_set_setting(self.tablet_id, self.key, True)

    async def async_turn_off(self, **kwargs: object) -> None:
        """Persist the desired value; wait for the tablet ACK before publishing it."""
        await self.runtime.async_set_setting(self.tablet_id, self.key, False)
