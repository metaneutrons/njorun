# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Njörun Home Assistant integration entry point."""

from __future__ import annotations

import time
from typing import Any

from homeassistant import config_entries
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import Platform
from homeassistant.core import CALLBACK_TYPE, HomeAssistant, callback
from homeassistant.helpers import discovery_flow
from homeassistant.helpers.device_registry import DeviceEntry
from homeassistant.helpers.event import async_call_later
from homeassistant.helpers.storage import Store

from .const import DOMAIN, STORE_VERSION
from .runtime import NjorunRuntime, RuntimeError
from .websocket import async_register_commands

PLATFORMS = (Platform.SELECT, Platform.NUMBER, Platform.BUTTON)


async def async_setup(hass: HomeAssistant, config: dict[str, Any]) -> bool:
    """Register versioned WebSocket commands once for this HA process."""
    hass.data.setdefault(DOMAIN, {"entries": {}})
    async_register_commands(hass)
    return True


async def async_setup_entry(hass: HomeAssistant, entry: ConfigEntry) -> bool:
    """Load durable settings and native entity platforms."""
    domain_data = hass.data.setdefault(DOMAIN, {"entries": {}})
    runtime = await NjorunRuntime.async_load(hass, entry)
    domain_data["entries"][entry.entry_id] = runtime
    entry.async_on_unload(runtime.async_unload)
    entry.async_on_unload(_offer_waiting_tablets(hass, runtime))
    await hass.config_entries.async_forward_entry_setups(entry, PLATFORMS)
    return True


def _offer_waiting_tablets(hass: HomeAssistant, runtime: NjorunRuntime) -> CALLBACK_TYPE:
    """Show a Discovered card for every tablet that waits for approval.

    The card is withdrawn when the tablet stops announcing (the runtime forgets it after
    ten minutes) or has been approved some other way.
    """
    timers: dict[str, CALLBACK_TYPE] = {}

    @callback
    def check(tablet_id: str) -> None:
        timers.pop(tablet_id, None)
        expires_at = runtime.announcement_expires_at(tablet_id)
        if expires_at is not None and expires_at > time.time():
            schedule(tablet_id, expires_at - time.time() + 1)
            return
        runtime.announcement(tablet_id)  # forgets it if expired
        for flow in hass.config_entries.flow.async_progress_by_handler(
            DOMAIN, match_context={"unique_id": tablet_id}
        ):
            hass.config_entries.flow.async_abort(flow["flow_id"])

    @callback
    def schedule(tablet_id: str, delay: float) -> None:
        timers[tablet_id] = async_call_later(
            hass, delay, lambda _now: check(tablet_id)
        )

    @callback
    def offer(tablet_id: str) -> None:
        discovery_flow.async_create_flow(
            hass,
            DOMAIN,
            {"source": config_entries.SOURCE_INTEGRATION_DISCOVERY},
            {"tablet_id": tablet_id},
        )
        if tablet_id not in timers:
            schedule(tablet_id, 60)

    @callback
    def stop() -> None:
        runtime.on_announcement = None
        for cancel in timers.values():
            cancel()
        timers.clear()

    runtime.on_announcement = offer
    return stop


async def async_remove_config_entry_device(
    hass: HomeAssistant, entry: ConfigEntry, device: DeviceEntry
) -> bool:
    """Deleting a tablet's device removes the tablet, its trust and its settings."""
    runtime = hass.data.get(DOMAIN, {}).get("entries", {}).get(entry.entry_id)
    if runtime is None:
        return True
    for domain, tablet_id in device.identifiers:
        if domain != DOMAIN or tablet_id not in runtime.configuration.tablets:
            continue
        try:
            await runtime.async_remove_tablet(tablet_id)
        except RuntimeError:
            return False
    return True


async def async_unload_entry(hass: HomeAssistant, entry: ConfigEntry) -> bool:
    """Unload platforms; the config-entry unload callback closes sessions."""
    unloaded = await hass.config_entries.async_unload_platforms(entry, PLATFORMS)
    if unloaded:
        hass.data.get(DOMAIN, {}).get("entries", {}).pop(entry.entry_id, None)
    return unloaded


async def async_remove_entry(hass: HomeAssistant, entry: ConfigEntry) -> None:
    """Delete the sensitive store when an administrator removes the entry."""
    runtime = hass.data.get(DOMAIN, {}).get("entries", {}).pop(entry.entry_id, None)
    if runtime is not None:
        await runtime.async_remove()
        return
    store = Store(
        hass,
        STORE_VERSION,
        f"{DOMAIN}.{entry.entry_id}",
        private=True,
        atomic_writes=True,
    )
    await store.async_remove()
