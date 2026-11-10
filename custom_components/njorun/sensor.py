# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Diagnostic sensors fed by a tablet's live telemetry (decision D6)."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from homeassistant.components.sensor import (
    SensorDeviceClass,
    SensorEntity,
    SensorEntityDescription,
    SensorStateClass,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import (
    SIGNAL_STRENGTH_DECIBELS_MILLIWATT,
    EntityCategory,
    UnitOfInformation,
    UnitOfTime,
)
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .entity import NjorunTabletEntity
from .live import RESET_REASONS
from .runtime import NjorunRuntime


@dataclass(frozen=True, kw_only=True)
class NjorunSensorDescription(SensorEntityDescription):
    """A sensor that reads one telemetry field."""

    field: str
    convert: Callable[[Any], Any] = lambda value: value


SENSORS = (
    NjorunSensorDescription(
        key="firmware_version",
        field="firmware_version",
        translation_key="firmware_version",
        entity_category=EntityCategory.DIAGNOSTIC,
    ),
    NjorunSensorDescription(
        key="uptime",
        field="uptime_s",
        translation_key="uptime",
        device_class=SensorDeviceClass.DURATION,
        native_unit_of_measurement=UnitOfTime.SECONDS,
        entity_category=EntityCategory.DIAGNOSTIC,
    ),
    NjorunSensorDescription(
        key="wifi_signal",
        field="rssi_dbm",
        translation_key="wifi_signal",
        device_class=SensorDeviceClass.SIGNAL_STRENGTH,
        native_unit_of_measurement=SIGNAL_STRENGTH_DECIBELS_MILLIWATT,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
    ),
    NjorunSensorDescription(
        key="free_heap",
        field="free_heap",
        translation_key="free_heap",
        device_class=SensorDeviceClass.DATA_SIZE,
        native_unit_of_measurement=UnitOfInformation.BYTES,
        suggested_unit_of_measurement=UnitOfInformation.KILOBYTES,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
    ),
    NjorunSensorDescription(
        key="reset_reason",
        field="reset_reason",
        translation_key="reset_reason",
        device_class=SensorDeviceClass.ENUM,
        options=list(RESET_REASONS),
        entity_category=EntityCategory.DIAGNOSTIC,
    ),
)


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    """Expose the diagnostic sensors on tablets that report telemetry."""
    runtime: NjorunRuntime = hass.data[DOMAIN]["entries"][entry.entry_id]
    runtime.register_platform("sensor", async_add_entities)


def create_entities(runtime: NjorunRuntime, tablet_id: str) -> list[SensorEntity]:
    """Build the telemetry sensors for a tablet that reports the live part."""
    live = runtime.configuration.tablets[tablet_id]["capabilities"].get("live", ())
    if "telemetry" not in live:
        return []
    return [NjorunTelemetrySensor(runtime, tablet_id, description) for description in SENSORS]


class NjorunTelemetrySensor(NjorunTabletEntity, SensorEntity):
    """One telemetry field; unavailable while no live session reports it."""

    entity_description: NjorunSensorDescription

    def __init__(
        self, runtime: NjorunRuntime, tablet_id: str, description: NjorunSensorDescription
    ) -> None:
        self.runtime = runtime
        self.tablet_id = tablet_id
        self.entity_description = description
        self._attr_unique_id = f"{tablet_id}_{description.key}"

    def supported_by(self, capabilities: dict[str, Any]) -> bool:
        return "telemetry" in capabilities.get("live", ())

    @property
    def available(self) -> bool:
        return self.runtime.telemetry(self.tablet_id) is not None

    @property
    def native_value(self) -> Any:
        telemetry = self.runtime.telemetry(self.tablet_id)
        if telemetry is None:
            return None
        return self.entity_description.convert(telemetry[self.entity_description.field])
