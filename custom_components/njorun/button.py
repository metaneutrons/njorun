# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Native HA button that returns a tablet to the settings of its profile."""

from __future__ import annotations

from homeassistant.components.button import ButtonEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
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
    """Expose the button on every tablet, now and when one is approved later."""
    runtime: NjorunRuntime = hass.data[DOMAIN]["entries"][entry.entry_id]
    runtime.register_platform("button", async_add_entities)


def create_entities(runtime: NjorunRuntime, tablet_id: str) -> list[ButtonEntity]:
    """Build the stable button entities for a logical tablet."""
    entities: list[ButtonEntity] = [NjorunUseProfileValuesButton(runtime, tablet_id)]
    if "wake" in runtime.configuration.tablets[tablet_id]["capabilities"].get("live", ()):
        entities.append(NjorunWakeButton(runtime, tablet_id))
    return entities


class NjorunUseProfileValuesButton(NjorunTabletEntity, ButtonEntity):
    """Clears the tablet's own settings; the tablet then follows its profile."""

    _attr_entity_category = EntityCategory.CONFIG
    _attr_translation_key = "use_profile_values"

    def __init__(self, runtime: NjorunRuntime, tablet_id: str) -> None:
        self.runtime = runtime
        self.tablet_id = tablet_id
        self._attr_unique_id = f"{tablet_id}_use_profile_values"

    @property
    def available(self) -> bool:
        # A change on the Home Assistant side; the tablet catches up when it is online.
        return self.tablet_id in self.runtime.configuration.tablets

    async def async_press(self) -> None:
        await self.runtime.async_use_profile_values(self.tablet_id)


class NjorunWakeButton(NjorunTabletEntity, ButtonEntity):
    """Wakes the tablet's display now. A live command: never queued, never repeated."""

    _attr_translation_key = "wake"

    def __init__(self, runtime: NjorunRuntime, tablet_id: str) -> None:
        self.runtime = runtime
        self.tablet_id = tablet_id
        self._attr_unique_id = f"{tablet_id}_wake"

    @property
    def available(self) -> bool:
        return self.runtime.live_part_offered(self.tablet_id, "wake")

    async def async_press(self) -> None:
        try:
            await self.runtime.async_send_command(self.tablet_id, "wake")
        except RuntimeError as err:
            raise HomeAssistantError(str(err)) from err
