# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Validated desired configuration; independent of HA and transport state.

Mutations are synchronous: the HA adapter must serialize them on its event loop
and persist the returned state before publishing a new configuration revision.
"""
from __future__ import annotations

from copy import deepcopy
from dataclasses import dataclass
import json
import unicodedata
from typing import Any

SCHEMA_VERSION = 1
DEFAULTS = {
    "orientation": "portrait",
    "brightness": 80,
    "theme": "automatic",
    "language": "en",
    "idle_timeout": 300,
    "idle_brightness": 10,
    "screensaver": "display_off",
    "home_view": "rooms",
    "details": "adaptive",
    "update_policy": "automatic",
    "update_time": "02:00",
    "update_channel": "stable",
    "favorites": [],
    "room_order": [],
    "entity_order": {},
    "excluded_entities": [],
}
CHOICES = {
    "orientation": ("portrait", "landscape"),
    "theme": ("automatic", "light", "dark"),
    "language": ("en", "de"),
    "screensaver": ("display_off", "clock", "disabled"),
    "home_view": ("rooms", "favorites", "mixed"),
    "details": ("adaptive", "fullscreen"),
    "update_policy": ("automatic", "manual"),
    "update_channel": ("stable", "beta"),
}
RANGES = {"brightness": (1, 100), "idle_brightness": (0, 100), "idle_timeout": (10, 86400)}
MAX_PROFILES = 64
MAX_TABLETS = 128
MAX_SELECTIONS = 256
MAX_AREAS = 128


class ConfigurationError(ValueError):
    """Rejected configuration; message contains no credentials."""


def identifier(value: Any) -> str:
    if not isinstance(value, str) or not 1 <= len(value) <= 128:
        raise ConfigurationError("Invalid identifier")
    if any(unicodedata.category(char) == "Cc" for char in value):
        raise ConfigurationError("Invalid identifier")
    return value


def name(value: Any) -> str:
    if not isinstance(value, str) or not value.strip() or len(value) > 128:
        raise ConfigurationError("Name must contain 1–128 characters")
    return value


def selection(value: Any) -> list[str]:
    if not isinstance(value, list) or len(value) > MAX_SELECTIONS:
        raise ConfigurationError("Selection exceeds its limit")
    result = [identifier(item) for item in value]
    if len(set(result)) != len(result):
        raise ConfigurationError("Selection contains duplicates")
    return result


def validate_settings(settings: Any, *, partial: bool = False) -> dict[str, Any]:
    if not isinstance(settings, dict) or set(settings) - DEFAULTS.keys():
        raise ConfigurationError("Unknown configuration field")
    if not partial and set(settings) != DEFAULTS.keys():
        raise ConfigurationError("Incomplete configuration")
    for key, value in settings.items():
        if key in CHOICES and value not in CHOICES[key]:
            raise ConfigurationError(f"Unsupported {key}")
        if key in RANGES:
            low, high = RANGES[key]
            if type(value) is not int or not low <= value <= high:
                raise ConfigurationError(f"Invalid {key}")
        if key == "update_time":
            if (not isinstance(value, str) or len(value) != 5
                    or value[2] != ":" or not value[:2].isascii()
                    or not value[3:].isascii() or not value[:2].isdigit()
                    or not value[3:].isdigit() or int(value[:2]) > 23
                    or int(value[3:]) > 59):
                raise ConfigurationError("Invalid update time")
        if key in ("favorites", "room_order", "excluded_entities"):
            selection(value)
        if key == "room_order" and len(value) > MAX_AREAS:
            raise ConfigurationError("Room order exceeds its limit")
        if key == "entity_order":
            if not isinstance(value, dict) or len(value) > MAX_AREAS + 1:
                raise ConfigurationError("Invalid entity order")
            for room, entities in value.items():
                identifier(room)
                selection(entities)
    if len(json.dumps(settings, ensure_ascii=False, separators=(",", ":")).encode()) > 3072:
        raise ConfigurationError("Configuration exceeds the transport size limit")
    return deepcopy(settings)


def validate_capabilities(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != {"orientations", "brightness"}:
        raise ConfigurationError("Unsupported capability schema")
    orientations = selection(value["orientations"])
    if not orientations or any(item not in CHOICES["orientation"] for item in orientations):
        raise ConfigurationError("Invalid orientations")
    if type(value["brightness"]) is not bool:
        raise ConfigurationError("Invalid brightness capability")
    return deepcopy(value)


def validate_effective(settings: dict[str, Any], capabilities: dict[str, Any]) -> None:
    if settings["orientation"] not in capabilities["orientations"]:
        raise ConfigurationError("Orientation is not supported by an assigned tablet")
    # A tablet without a brightness actuator still stores the profile preference;
    # its adapter must not advertise or create a brightness control.


@dataclass(frozen=True)
class ConfigurationSnapshot:
    """Desired settings and source, never an assertion of successful application."""

    tablet_id: str
    profile_id: str
    revision: int
    settings: dict[str, Any]
    overrides: list[str]


class Configuration:
    """Profile/assignment state with optimistic concurrency and atomic validation."""

    def __init__(self) -> None:
        self.revision = 0
        self.profiles: dict[str, dict[str, Any]] = {}
        self.tablets: dict[str, dict[str, Any]] = {}

    def _expect(self, revision: int) -> None:
        if type(revision) is not int or revision != self.revision:
            raise ConfigurationError("Configuration changed; refresh and retry")

    def _profile(self, profile_id: str) -> dict[str, Any]:
        try:
            return self.profiles[profile_id]
        except KeyError as exc:
            raise ConfigurationError("Unknown profile") from exc

    def _tablet(self, tablet_id: str) -> dict[str, Any]:
        try:
            return self.tablets[tablet_id]
        except KeyError as exc:
            raise ConfigurationError("Unknown tablet") from exc

    def _effective(self, tablet: dict[str, Any], profile: dict[str, Any] | None = None) -> dict[str, Any]:
        if profile is None:
            profile = self._profile(tablet["profile_id"])
        settings = profile["settings"] | tablet["overrides"]
        validate_settings(settings)
        validate_effective(settings, tablet["capabilities"])
        return deepcopy(settings)

    def _commit(self, affected: list[str]) -> None:
        self.revision += 1
        for tablet_id in affected:
            self.tablets[tablet_id]["revision"] = self.revision

    def add_profile(self, profile_id: str, label: str, settings: dict[str, Any], expected: int) -> None:
        self._expect(expected)
        identifier(profile_id)
        name(label)
        if profile_id in self.profiles or len(self.profiles) >= MAX_PROFILES:
            raise ConfigurationError("Profile already exists or profile limit reached")
        complete = validate_settings(deepcopy(DEFAULTS) | validate_settings(settings, partial=True))
        self.profiles[profile_id] = {"name": label, "settings": complete}
        self._commit([])

    def edit_profile(self, profile_id: str, patch: dict[str, Any], expected: int) -> None:
        self._expect(expected)
        profile = deepcopy(self._profile(profile_id))
        profile["settings"].update(validate_settings(patch, partial=True))
        affected = [key for key, value in self.tablets.items() if value["profile_id"] == profile_id]
        for tablet_id in affected:
            self._effective(self.tablets[tablet_id], profile)
        self.profiles[profile_id] = profile
        self._commit(affected)

    def delete_profile(self, profile_id: str, expected: int) -> None:
        self._expect(expected)
        self._profile(profile_id)
        if any(tablet["profile_id"] == profile_id for tablet in self.tablets.values()):
            raise ConfigurationError("Profile is assigned; reassign its tablets first")
        del self.profiles[profile_id]
        self._commit([])

    def add_tablet(self, tablet_id: str, label: str, profile_id: str,
                   capabilities: dict[str, Any], expected: int) -> None:
        self._expect(expected)
        identifier(tablet_id)
        name(label)
        if tablet_id in self.tablets or len(self.tablets) >= MAX_TABLETS:
            raise ConfigurationError("Tablet already exists or tablet limit reached")
        tablet = {"name": label, "profile_id": profile_id, "overrides": {},
                  "capabilities": validate_capabilities(capabilities), "revision": 0}
        self._effective(tablet)
        self.tablets[tablet_id] = tablet
        self._commit([tablet_id])

    def set_overrides(self, tablet_id: str, overrides: dict[str, Any], expected: int) -> None:
        self._expect(expected)
        tablet = deepcopy(self._tablet(tablet_id))
        tablet["overrides"] = validate_settings(overrides, partial=True)
        self._effective(tablet)
        self.tablets[tablet_id] = tablet
        self._commit([tablet_id])

    def preview_assignment(self, tablet_id: str, profile_id: str, *, clear: bool = False) -> dict[str, Any]:
        tablet = deepcopy(self._tablet(tablet_id))
        tablet["profile_id"] = profile_id
        if clear:
            tablet["overrides"] = {}
        settings = self._effective(tablet)
        before = self.snapshot(tablet_id).settings
        return {"settings": settings, "changes": {key: {"before": before[key], "after": value}
                for key, value in settings.items() if before[key] != value},
                "automatic_updates_enabled": before["update_policy"] == "manual"
                and settings["update_policy"] == "automatic"}

    def assign(self, tablet_id: str, profile_id: str, expected: int, *, clear: bool = False) -> None:
        self._expect(expected)
        self.preview_assignment(tablet_id, profile_id, clear=clear)
        tablet = deepcopy(self._tablet(tablet_id))
        tablet["profile_id"] = profile_id
        if clear:
            tablet["overrides"] = {}
        self.tablets[tablet_id] = tablet
        self._commit([tablet_id])

    def remove_tablet(self, tablet_id: str, expected: int) -> None:
        self._expect(expected)
        self._tablet(tablet_id)
        del self.tablets[tablet_id]
        self._commit([])

    def snapshot(self, tablet_id: str) -> ConfigurationSnapshot:
        tablet = self._tablet(tablet_id)
        return ConfigurationSnapshot(tablet_id, tablet["profile_id"], tablet["revision"],
                                     self._effective(tablet), sorted(tablet["overrides"]))

    def dump(self) -> dict[str, Any]:
        return deepcopy({"version": SCHEMA_VERSION, "revision": self.revision,
                         "profiles": self.profiles, "tablets": self.tablets})

    @classmethod
    def restore(cls, data: dict[str, Any]) -> Configuration:
        if not isinstance(data, dict) or set(data) != {"version", "revision", "profiles", "tablets"}:
            raise ConfigurationError("Invalid stored configuration")
        if type(data["version"]) is not int or data["version"] != SCHEMA_VERSION or type(data["revision"]) is not int or data["revision"] < 0:
            raise ConfigurationError("Unsupported stored configuration")
        result = cls()
        if not isinstance(data["profiles"], dict) or not isinstance(data["tablets"], dict):
            raise ConfigurationError("Invalid stored records")
        for profile_id, profile in data["profiles"].items():
            if not isinstance(profile, dict) or set(profile) != {"name", "settings"}:
                raise ConfigurationError("Invalid stored profile")
            validate_settings(profile["settings"])
            result.add_profile(profile_id, profile["name"], profile["settings"], result.revision)
        for tablet_id, tablet in data["tablets"].items():
            if not isinstance(tablet, dict) or set(tablet) != {"name", "profile_id", "overrides", "capabilities", "revision"}:
                raise ConfigurationError("Invalid stored tablet")
            revision = tablet["revision"]
            if type(revision) is not int or not 1 <= revision <= data["revision"]:
                raise ConfigurationError("Invalid stored tablet revision")
            result.add_tablet(tablet_id, tablet["name"], tablet["profile_id"], tablet["capabilities"], result.revision)
            result.set_overrides(tablet_id, tablet["overrides"], result.revision)
            result.tablets[tablet_id]["revision"] = revision
        result.revision = data["revision"]
        return result
