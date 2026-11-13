# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Shared behavior of the entities on a tablet's device page."""

from __future__ import annotations

from typing import Any

from .const import DOMAIN
from .entity_ids import object_key
from .runtime import NjorunRuntime


class NjorunTabletEntity:
    """Mixin: one device per tablet, tracked by the runtime for state writes."""

    _attr_has_entity_name = True
    _attr_should_poll = False

    runtime: NjorunRuntime
    tablet_id: str

    def supported_by(self, capabilities: dict[str, Any]) -> bool:
        """Whether the tablet's capabilities (decision D8) still justify this entity."""
        return True

    @property
    def suggested_object_id(self) -> str:
        """The entity ID follows the internal key, never the translated name.

        The name is shown to people and may change with the language or its wording;
        the ID is what automations and dashboards hold on to.
        """
        return object_key(self.unique_id or "", self.tablet_id)

    @property
    def device_info(self) -> dict[str, Any]:
        tablet = self.runtime.configuration.tablets.get(self.tablet_id, {})
        return {
            "identifiers": {(DOMAIN, self.tablet_id)},
            "name": tablet.get("name", self.tablet_id),
            "manufacturer": "Njörun",
            "model": "Touchscreen",
        }

    async def async_added_to_hass(self) -> None:
        await super().async_added_to_hass()  # type: ignore[misc]
        self.runtime.add_entity(self.tablet_id, self)

    async def async_will_remove_from_hass(self) -> None:
        self.runtime.remove_entity(self.tablet_id, self)
        await super().async_will_remove_from_hass()  # type: ignore[misc]


class NjorunSettingEntity(NjorunTabletEntity):
    """A tablet setting: its state is what the tablet last acknowledged as applied."""

    key: str

    @property
    def available(self) -> bool:
        return self.runtime.is_available(self.tablet_id)

    @property
    def extra_state_attributes(self) -> dict[str, str]:
        """Where the value comes from: this tablet's own override or its profile."""
        tablet = self.runtime.configuration.tablets.get(self.tablet_id)
        if tablet is None:
            return {}
        return {"source": "tablet" if self.key in tablet["overrides"] else "profile"}
