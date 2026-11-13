# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Stable entity IDs: the internal key, not the translated name.

Home Assistant builds an entity ID from the entity name in the language it ran in when
the entity was first registered. The same entity could end up as ``..._brightness`` on one
installation and ``..._helligkeit`` on another. Njörun therefore names the ID itself.
"""

from __future__ import annotations


def object_key(unique_id: str, tablet_id: str) -> str:
    """The part of an entity's unique ID after the tablet ID, for example ``ui_scale``."""
    return unique_id.removeprefix(f"{tablet_id}_")
