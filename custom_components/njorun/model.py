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
    "theme": "dark",
    # Kept for old clients and backups. The HA UI exposes the two palettes below.
    "color_scheme": "njorun",
    "light_color_scheme": "njorun",
    "dark_color_scheme": "njorun",
    "increased_contrast": False,
    "reduced_motion": False,
    "language": "en",
    "idle_timeout": 300,
    "idle_brightness": 10,
    "ui_scale": 100,
    "screensaver": "display_off",
    "home_view": "mixed",
    "details": "adaptive",
    "update_policy": "automatic",
    "update_time": "02:00",
    "update_channel": "stable",
    "favorites": [],
    "room_order": [],
    "entity_order": {},
    "excluded_entities": [],
    "entity_names": {},
    "room_names": {},
    "room_icons": {},
}
CHOICES = {
    "orientation": ("portrait", "landscape", "portrait_flipped", "landscape_flipped"),
    "theme": ("automatic", "light", "dark"),
    "color_scheme": ("njorun", "graphite", "warm", "porcelain", "high_contrast"),
    "light_color_scheme": ("njorun", "graphite", "warm", "porcelain"),
    "dark_color_scheme": ("njorun", "graphite", "warm", "porcelain"),
    "language": ("en", "de"),
    "screensaver": ("display_off", "clock", "disabled"),
    "home_view": ("rooms", "favorites", "mixed"),
    "details": ("adaptive", "fullscreen"),
    "update_policy": ("automatic", "manual"),
    "update_channel": ("stable", "beta"),
}
BOOLEAN_KEYS = ("increased_contrast", "reduced_motion")
ROOM_ICONS = ("home", "sofa", "kitchen", "bed", "desk", "bath", "garage", "garden")
RANGES = {
    "brightness": (1, 100),
    "idle_brightness": (0, 100),
    "idle_timeout": (10, 86400),
    # The size of everything on the dashboard in percent.
    "ui_scale": (75, 200),
}
MAX_PROFILES = 64
MAX_TABLETS = 128
MAX_SELECTIONS = 256
MAX_AREAS = 128
MAX_LOG_SINK = 256
_ADDITIVE_DEFAULTS = (
    "ui_scale",
    "color_scheme",
    "light_color_scheme",
    "dark_color_scheme",
    "increased_contrast",
    "reduced_motion",
    "entity_names",
    "room_names",
    "room_icons",
)


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


def validate_log_sink(value: Any) -> str:
    """The remote logging sink: empty for off, else an http(s) URL the tablet accepts.

    Mirrors the tablet's check: no credentials, query or fragment, at most 256 characters.
    """
    if not isinstance(value, str):
        raise ConfigurationError("Invalid log sink")
    text = value.strip()
    if not text:
        return ""
    scheme_end = text.find("://")
    scheme = text[:scheme_end] if scheme_end > 0 else ""
    rest = text[scheme_end + 3:] if scheme_end > 0 else ""
    authority = rest.split("/", 1)[0]
    if (
        scheme not in ("http", "https")
        or len(text) > MAX_LOG_SINK
        or not authority
        or any(char in text for char in "?# \t\r\n@")
        or not all(char.isascii() and (char.isalnum() or char in ".-:[]") for char in authority)
        or any(unicodedata.category(char) == "Cc" for char in text)
    ):
        raise ConfigurationError("The log sink must be an http(s) URL such as https://loki.example.org")
    return text


def options_menu(profiles: int, network_tablets: int = 0) -> list[str]:
    """Options-flow menu entries that apply now; a step that could only abort is not offered.

    The options manage shared profiles and installation-wide settings. Tablets are approved on
    their Discovered card and changed on their device page. Deleting a profile needs another
    one to keep. The network entries need a tablet that reports network interfaces.
    """
    entries = ["edit_profile", "add_profile"] if profiles else ["add_profile"]
    if profiles:
        entries.extend(["edit_entity_names", "edit_room_display"])
    if profiles > 1:
        entries.append("delete_profile")
    if network_tablets:
        entries.extend(["network_profile", "network_tablet"])
    entries.append("remote_logging")
    return entries


def validate_settings(settings: Any, *, partial: bool = False) -> dict[str, Any]:
    if not isinstance(settings, dict) or set(settings) - DEFAULTS.keys():
        raise ConfigurationError("Unknown configuration field")
    if not partial and set(settings) != DEFAULTS.keys():
        raise ConfigurationError("Incomplete configuration")
    for key, value in settings.items():
        if key in CHOICES and value not in CHOICES[key]:
            raise ConfigurationError(f"Unsupported {key}")
        if key in BOOLEAN_KEYS and type(value) is not bool:
            raise ConfigurationError(f"Invalid {key}")
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
        if key in ("entity_names", "room_names", "room_icons"):
            limit = MAX_SELECTIONS if key == "entity_names" else MAX_AREAS
            if not isinstance(value, dict) or len(value) > limit:
                raise ConfigurationError(f"Invalid {key}")
            for item_id, label in value.items():
                identifier(item_id)
                if not isinstance(label, str) or not label.strip() or len(label) > 128:
                    raise ConfigurationError(f"Invalid {key}")
                if any(unicodedata.category(char) == "Cc" for char in label):
                    raise ConfigurationError(f"Invalid {key}")
                if key == "room_icons" and label not in ROOM_ICONS:
                    raise ConfigurationError("Unsupported room icon")
    if len(json.dumps(settings, ensure_ascii=False, separators=(",", ":")).encode()) > 3072:
        raise ConfigurationError("Configuration exceeds the transport size limit")
    return deepcopy(settings)


def migrate_settings(settings: Any, *, complete: bool = False) -> Any:
    """Migrate stored settings while preserving tablet-level partial overrides.

    Before independent light and dark palettes existed, `color_scheme` selected both. The
    legacy high-contrast preset was the Njörun palette plus increased contrast. Modern
    settings already carry their explicit palette fields, so their values always win.
    """
    if not isinstance(settings, dict):
        return settings
    migrated = deepcopy(settings)
    legacy_scheme = migrated.get("color_scheme")
    if legacy_scheme in CHOICES["color_scheme"]:
        scheme = "njorun" if legacy_scheme == "high_contrast" else legacy_scheme
        migrated["color_scheme"] = scheme
        migrated.setdefault("light_color_scheme", scheme)
        migrated.setdefault("dark_color_scheme", scheme)
        if legacy_scheme == "high_contrast":
            migrated.setdefault("increased_contrast", True)
    if complete:
        for key in _ADDITIVE_DEFAULTS:
            migrated.setdefault(key, deepcopy(DEFAULTS[key]))
    return migrated


LIVE_PARTS = ("wake", "backlight", "telemetry")
NETWORK_INTERFACES = ("wifi", "ethernet")


def validate_capabilities(value: Any) -> dict[str, Any]:
    base = {"orientations", "brightness"}
    # `live` lists the live parts of decision D6, `ota` carries the board of decision D7 and
    # `network` the interfaces for the network messages of plan 006; tablets approved before
    # them have none.
    if (
        not isinstance(value, dict)
        or not base <= set(value)
        or set(value) - (base | {"live", "ota", "network"})
    ):
        raise ConfigurationError("Unsupported capability schema")
    orientations = selection(value["orientations"])
    if not orientations or any(item not in CHOICES["orientation"] for item in orientations):
        raise ConfigurationError("Invalid orientations")
    if type(value["brightness"]) is not bool:
        raise ConfigurationError("Invalid brightness capability")
    if "live" in value:
        live = selection(value["live"])
        if any(item not in LIVE_PARTS for item in live):
            raise ConfigurationError("Invalid live capabilities")
    if "network" in value:
        interfaces = selection(value["network"])
        if not interfaces or any(item not in NETWORK_INTERFACES for item in interfaces):
            raise ConfigurationError("Invalid network capabilities")
    if "ota" in value:
        ota = value["ota"]
        # `board` is required. `hardware_revision` and `partition_layout` are what the tablet
        # runs on; HA offers an update only when it knows both (docs/protocols/002).
        if (
            not isinstance(ota, dict)
            or "board" not in ota
            or set(ota) - {"board", "hardware_revision", "partition_layout"}
        ):
            raise ConfigurationError("Invalid ota capability")
        for field in ota:
            identifier(ota[field])
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
    log_sink: str = ""


class Configuration:
    """Profile/assignment state with optimistic concurrency and atomic validation."""

    def __init__(self) -> None:
        self.revision = 0
        self.profiles: dict[str, dict[str, Any]] = {}
        self.tablets: dict[str, dict[str, Any]] = {}
        # Installation-wide remote logging sink, empty for off; every tablet receives it.
        self.log_sink = ""

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

    def set_capabilities(self, tablet_id: str, capabilities: dict[str, Any], expected: int) -> None:
        """Replace a tablet's capabilities (decision D8) if all its settings stay valid."""
        self._expect(expected)
        tablet = deepcopy(self._tablet(tablet_id))
        tablet["capabilities"] = validate_capabilities(capabilities)
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

    def set_log_sink(self, url: Any, expected: int) -> None:
        self._expect(expected)
        sink = validate_log_sink(url)
        if sink == self.log_sink:
            return
        self.log_sink = sink
        self._commit(list(self.tablets))

    def remove_tablet(self, tablet_id: str, expected: int) -> None:
        self._expect(expected)
        self._tablet(tablet_id)
        del self.tablets[tablet_id]
        self._commit([])

    def snapshot(self, tablet_id: str) -> ConfigurationSnapshot:
        tablet = self._tablet(tablet_id)
        return ConfigurationSnapshot(tablet_id, tablet["profile_id"], tablet["revision"],
                                     self._effective(tablet), sorted(tablet["overrides"]),
                                     self.log_sink)

    def dump(self) -> dict[str, Any]:
        return deepcopy({"version": SCHEMA_VERSION, "revision": self.revision,
                         "profiles": self.profiles, "tablets": self.tablets,
                         "log_sink": self.log_sink})

    @classmethod
    def restore(cls, data: dict[str, Any]) -> Configuration:
        # State stored before the log sink existed has no such key: remote logging is off.
        if not isinstance(data, dict) or set(data) - {"log_sink"} != {"version", "revision", "profiles", "tablets"}:
            raise ConfigurationError("Invalid stored configuration")
        if type(data["version"]) is not int or data["version"] != SCHEMA_VERSION or type(data["revision"]) is not int or data["revision"] < 0:
            raise ConfigurationError("Unsupported stored configuration")
        result = cls()
        if not isinstance(data["profiles"], dict) or not isinstance(data["tablets"], dict):
            raise ConfigurationError("Invalid stored records")
        for profile_id, profile in data["profiles"].items():
            if not isinstance(profile, dict) or set(profile) != {"name", "settings"}:
                raise ConfigurationError("Invalid stored profile")
            settings = migrate_settings(profile["settings"], complete=True)
            validate_settings(settings)
            result.add_profile(profile_id, profile["name"], settings, result.revision)
        for tablet_id, tablet in data["tablets"].items():
            if not isinstance(tablet, dict) or set(tablet) != {"name", "profile_id", "overrides", "capabilities", "revision"}:
                raise ConfigurationError("Invalid stored tablet")
            revision = tablet["revision"]
            if type(revision) is not int or not 1 <= revision <= data["revision"]:
                raise ConfigurationError("Invalid stored tablet revision")
            result.add_tablet(tablet_id, tablet["name"], tablet["profile_id"], tablet["capabilities"], result.revision)
            overrides = migrate_settings(tablet["overrides"])
            result.set_overrides(tablet_id, overrides, result.revision)
            result.tablets[tablet_id]["revision"] = revision
        result.log_sink = validate_log_sink(data.get("log_sink", ""))
        result.revision = data["revision"]
        return result
