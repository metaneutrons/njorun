# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Stable entity IDs: the internal key, not the translated name.

Home Assistant builds an entity ID from the entity name in the language it ran in when
the entity was first registered. The same entity could end up as ``..._brightness`` on one
installation and ``..._helligkeit`` on another. Njörun therefore names the ID itself.
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Callable

if TYPE_CHECKING:
    from homeassistant.core import HomeAssistant

_LOGGER = logging.getLogger(__name__)


def object_key(unique_id: str, tablet_id: str) -> str:
    """The part of an entity's unique ID after the tablet ID, for example ``ui_scale``."""
    return unique_id.removeprefix(f"{tablet_id}_")


def canonical_entity_id(
    entity_id: str,
    unique_id: str,
    tablet_id: str,
    original_name: str | None,
    slugify: Callable[[str], str],
) -> str | None:
    """The entity ID this entity should have, or None if it already has it or cannot be told.

    The device prefix of the existing ID is kept, so a renamed device or a different
    naming setting in Home Assistant is respected. It is found by removing the slug of
    the name the ID was once built from.
    """
    domain, _, object_id = entity_id.partition(".")
    key = object_key(unique_id, tablet_id)
    if not key or object_id.endswith(f"_{key}") or object_id == key:
        return None
    if not original_name:
        return None
    tail = slugify(original_name)
    if not tail or not object_id.endswith(tail):
        return None
    return f"{domain}.{object_id[: len(object_id) - len(tail)]}{key}"


def migrate_entity_ids(hass: HomeAssistant, config_entry_id: str) -> None:
    """Give the entities of a config entry the IDs derived from their keys (once).

    An entity whose wanted ID is taken by another entity keeps its current ID.
    """
    from homeassistant.helpers import entity_registry as er
    from homeassistant.util import slugify

    from .const import DOMAIN

    registry = er.async_get(hass)
    for entry in er.async_entries_for_config_entry(registry, config_entry_id):
        if entry.platform != DOMAIN or not entry.unique_id.startswith("njorun-"):
            continue
        tablet_id = entry.unique_id.split("_", 1)[0]
        wanted = canonical_entity_id(
            entry.entity_id, entry.unique_id, tablet_id, entry.original_name, slugify
        )
        if wanted is None:
            continue
        if registry.async_get(wanted) is not None:
            _LOGGER.warning("Cannot rename %s to %s: the name is taken", entry.entity_id, wanted)
            continue
        _LOGGER.info("Renaming %s to %s", entry.entity_id, wanted)
        registry.async_update_entity(entry.entity_id, new_entity_id=wanted)
