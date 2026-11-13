# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Native HA select entities for finite Njörun settings and the tablet's profile."""

from __future__ import annotations

from collections import Counter

from homeassistant.components.select import SelectEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .entity import NjorunSettingEntity, NjorunTabletEntity
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
    entities: list[SelectEntity] = [
        NjorunSettingSelect(runtime, tablet_id, key)
        for key in _SETTING_KEYS
        if key in runtime.configuration.tablets[tablet_id]["overrides"]
        or key in runtime.configuration.snapshot(tablet_id).settings
    ]
    entities.append(NjorunProfileSelect(runtime, tablet_id))
    return entities


def profile_labels(runtime: NjorunRuntime) -> dict[str, str]:
    """Profile ID to the name shown in HA; a name used twice gets its ID appended."""
    names = {
        profile_id: record["name"]
        for profile_id, record in runtime.configuration.profiles.items()
    }
    counts = Counter(names.values())
    return {
        profile_id: name if counts[name] == 1 else f"{name} ({profile_id})"
        for profile_id, name in names.items()
    }


class NjorunSettingSelect(NjorunSettingEntity, SelectEntity):
    """A select whose state follows durable device-reported values only."""

    _attr_entity_category = EntityCategory.CONFIG

    def __init__(self, runtime: NjorunRuntime, tablet_id: str, key: str) -> None:
        self.runtime = runtime
        self.tablet_id = tablet_id
        self.key = key
        self._attr_unique_id = f"{tablet_id}_{key}"
        self._attr_translation_key = key

    @property
    def options(self) -> list[str]:
        choices = list(CHOICES[self.key])
        if self.key == "orientation":
            # Only the rotations this tablet reported as qualified.
            tablet = self.runtime.configuration.tablets.get(self.tablet_id, {})
            supported = tablet.get("capabilities", {}).get("orientations")
            if supported:
                choices = [choice for choice in choices if choice in supported]
        return choices

    @property
    def current_option(self) -> str | None:
        record = self.runtime.applied.get(self.tablet_id)
        if record is None:
            return None
        value = record["settings"].get(self.key)
        return value if isinstance(value, str) else None

    async def async_select_option(self, option: str) -> None:
        """Persist a desired override and dispatch it without optimistic state."""
        if option not in CHOICES[self.key]:
            raise ValueError("Unsupported Njörun select option")
        await self.runtime.async_set_setting(self.tablet_id, self.key, option)


class NjorunProfileSelect(NjorunTabletEntity, SelectEntity):
    """The profile a tablet follows; choosing another keeps the tablet's own settings."""

    _attr_entity_category = EntityCategory.CONFIG
    _attr_translation_key = "profile"

    def __init__(self, runtime: NjorunRuntime, tablet_id: str) -> None:
        self.runtime = runtime
        self.tablet_id = tablet_id
        self._attr_unique_id = f"{tablet_id}_profile"

    @property
    def options(self) -> list[str]:
        return list(profile_labels(self.runtime).values())

    @property
    def current_option(self) -> str | None:
        tablet = self.runtime.configuration.tablets.get(self.tablet_id)
        if tablet is None:
            return None
        return profile_labels(self.runtime).get(tablet["profile_id"])

    @property
    def available(self) -> bool:
        # An assignment is configuration kept in Home Assistant, not device state.
        return self.tablet_id in self.runtime.configuration.tablets

    async def async_select_option(self, option: str) -> None:
        for profile_id, label in profile_labels(self.runtime).items():
            if label == option:
                await self.runtime.async_set_profile(self.tablet_id, profile_id)
                return
        raise ValueError("Unknown Njörun profile")
