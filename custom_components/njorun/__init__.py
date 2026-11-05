"""Njörun Home Assistant integration entry point."""

from __future__ import annotations

from typing import Any

from homeassistant.config_entries import ConfigEntry
from homeassistant.const import Platform
from homeassistant.core import HomeAssistant
from homeassistant.helpers.storage import Store

from .const import DOMAIN, STORE_VERSION
from .runtime import NjorunRuntime
from .websocket import async_register_commands

PLATFORMS = (Platform.SELECT, Platform.NUMBER)


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
    await hass.config_entries.async_forward_entry_setups(entry, PLATFORMS)
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
