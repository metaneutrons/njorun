# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0
"""What Home Assistant keeps about the network of its tablets (plan 006, decisions N4, N6, N9).

Wi-Fi credentials are set per profile and can be replaced per tablet. A static address is set
per tablet and interface only, because a profile would give the same address to every tablet in
it. Each tablet has a revision that rises whenever the values HA would send change (`sync`
compares them with the ones it last saw, so profile edits, reassignment and new capabilities
all count), and the result the tablet reported for the newest revision it answered. A revision
is at least the Unix time of its change, so that it still exceeds what a tablet finished after
HA lost its storage (a tablet refuses an older revision). HA sends a revision once: a reverted
revision is not sent again until the administrator changes the values (N6).

Nothing here imports Home Assistant. Values contain a password: use `network.redacted` before
logging or showing them.
"""

from __future__ import annotations

from copy import deepcopy
import json
from typing import Any

from .network import MAX_REVISION, NetworkError, validate_network

UNSET: Any = object()
MAX_PROFILES = 32
MAX_TABLETS = 64


def _fail(problem: str, message: str) -> NetworkError:
    return NetworkError(problem, message)


def _credentials(wifi: Any, *, where: str) -> dict[str, str]:
    """Validate Wi-Fi credentials (a name and an optional password) and return a clean copy."""
    if not isinstance(wifi, dict) or set(wifi) - {"ssid", "password"}:
        raise _fail("parse", f"The {where} Wi-Fi setting has an unexpected shape")
    validate_network({"wifi": wifi}, ["wifi"])
    result = {"ssid": wifi["ssid"]}
    if wifi.get("password"):
        result["password"] = wifi["password"]
    return result


def _ip_settings(ip: Any, interfaces: list[str] | tuple[str, ...]) -> dict[str, dict[str, Any]]:
    """Validate the IP setups per interface and return a clean copy."""
    if not isinstance(ip, dict) or set(ip) - {"wifi", "ethernet"}:
        raise _fail("parse", "The IP settings have an unexpected shape")
    result: dict[str, dict[str, Any]] = {}
    for interface, setup in ip.items():
        if interface not in interfaces:
            raise _fail("no_such_interface", f"This tablet has no {interface} interface")
        config = {"wifi": {"ssid": "x", "ip": setup}} if interface == "wifi" else {
            "ethernet": {"ip": setup}
        }
        validate_network(config, [interface])
        result[interface] = deepcopy(setup)
    return result


class NetworkSettings:
    """Desired network values, their revisions and the tablets' latest results."""

    def __init__(self) -> None:
        self.profiles: dict[str, dict[str, Any]] = {}
        self.tablets: dict[str, dict[str, Any]] = {}
        self.state: dict[str, dict[str, Any]] = {}

    def copy(self) -> NetworkSettings:
        other = NetworkSettings()
        other.profiles = deepcopy(self.profiles)
        other.tablets = deepcopy(self.tablets)
        other.state = deepcopy(self.state)
        return other

    def is_empty(self) -> bool:
        return not (self.profiles or self.tablets or self.state)

    # Changes. Each one validates first and changes nothing when it raises.

    def set_profile_wifi(self, profile_id: str, wifi: Any) -> None:
        """Set the Wi-Fi credentials of a profile, or clear them with `None`."""
        if wifi is None:
            self.profiles.pop(profile_id, None)
            return
        if isinstance(wifi, dict) and "ip" in wifi:
            raise _fail("profile_ip", "A profile cannot carry an IP setup; set it per tablet")
        if profile_id not in self.profiles and len(self.profiles) >= MAX_PROFILES:
            raise _fail("limit", "Too many profiles have network settings")
        self.profiles[profile_id] = {"wifi": _credentials(wifi, where="profile")}

    def set_tablet(
        self,
        tablet_id: str,
        interfaces: list[str] | tuple[str, ...],
        *,
        wifi: Any = UNSET,
        ip: Any = UNSET,
    ) -> None:
        """Set a tablet's own Wi-Fi credentials and IP setups; `None` clears, unset keeps."""
        entry = deepcopy(self.tablets.get(tablet_id, {}))
        if wifi is not UNSET:
            if wifi is None:
                entry.pop("wifi", None)
            else:
                entry["wifi"] = _credentials(wifi, where="tablet")
        if ip is not UNSET:
            if ip is None:
                entry.pop("ip", None)
            else:
                entry["ip"] = _ip_settings(ip, interfaces)
        if tablet_id not in self.tablets and len(self.tablets) >= MAX_TABLETS:
            raise _fail("limit", "Too many tablets have network settings")
        if entry.get("ip") == {}:
            entry.pop("ip")
        if entry:
            self.tablets[tablet_id] = entry
        else:
            self.tablets.pop(tablet_id, None)

    def forget_profile(self, profile_id: str) -> None:
        self.profiles.pop(profile_id, None)

    def forget_tablet(self, tablet_id: str) -> None:
        self.tablets.pop(tablet_id, None)
        self.state.pop(tablet_id, None)

    # What would be sent.

    def desired(
        self, tablet_id: str, profile_id: str, interfaces: list[str] | tuple[str, ...]
    ) -> dict[str, Any]:
        """The interface settings HA would send to the tablet; empty when it has nothing to say.

        Wi-Fi is sent only with credentials, because the message names the network. A static
        address alone does not change a tablet whose Wi-Fi was set up on the tablet.
        """
        tablet = self.tablets.get(tablet_id, {})
        credentials = tablet.get("wifi") or self.profiles.get(profile_id, {}).get("wifi")
        ip = tablet.get("ip", {})
        config: dict[str, Any] = {}
        if "wifi" in interfaces and credentials:
            wifi = dict(credentials)
            if "wifi" in ip:
                wifi["ip"] = deepcopy(ip["wifi"])
            config["wifi"] = wifi
        if "ethernet" in interfaces and "ethernet" in ip:
            config["ethernet"] = {"ip": deepcopy(ip["ethernet"])}
        if config:
            validate_network(config, interfaces)
        return config

    def revision(self, tablet_id: str) -> int:
        return self.state.get(tablet_id, {}).get("revision", 0)

    def sync(self, tablet_id: str, desired: dict[str, Any], now: float) -> bool:
        """Raise the revision when `desired` differs from what was last seen; says whether it did.

        The new revision is one more than the old, or the Unix time in seconds if that is more.
        """
        entry = self.state.get(tablet_id)
        if entry is None:
            if not desired:
                return False
            entry = self.state[tablet_id] = {"revision": 0, "desired": {}, "result": None}
        if entry["desired"] == desired:
            return False
        entry["revision"] = min(max(entry["revision"] + 1, int(now)), MAX_REVISION)
        entry["desired"] = deepcopy(desired)
        return True

    def result(self, tablet_id: str) -> dict[str, Any] | None:
        result = self.state.get(tablet_id, {}).get("result")
        return deepcopy(result) if result else None

    def due(self, tablet_id: str, desired: dict[str, Any]) -> int | None:
        """The revision to send, or `None`: nothing to say, or this revision already has its answer."""
        if not desired:
            return None
        revision = self.revision(tablet_id)
        if revision < 1 or self.state[tablet_id]["desired"] != desired:
            return None
        result = self.state[tablet_id]["result"]
        if result is not None and result["revision"] >= revision:
            return None
        return revision

    def record_result(self, tablet_id: str, payload: dict[str, Any], now: float) -> str:
        """Take a validated `network_result`; returns `applied`, `reverted`, `stale` or `unknown`.

        A result for an older revision is `stale` and a result for a revision HA has not issued
        is `unknown` (HA lost its storage, or the tablet was paired elsewhere); neither changes
        anything.
        """
        entry = self.state.get(tablet_id)
        revision = payload["revision"]
        if entry is None or revision > entry["revision"]:
            return "unknown"
        if revision < entry["revision"]:
            return "stale"
        detail = "proof" if payload["result"] == "applied" else "reason"
        entry["result"] = {
            "revision": revision,
            "result": payload["result"],
            detail: payload[detail],
            "state": deepcopy(payload["state"]),
            "at": now,
        }
        return payload["result"]

    # Storage.

    def dump(self) -> dict[str, Any]:
        return {
            "profiles": deepcopy(self.profiles),
            "tablets": deepcopy(self.tablets),
            "state": deepcopy(self.state),
        }

    @classmethod
    def restore(cls, data: Any) -> NetworkSettings:
        """Rebuild from storage; raises `NetworkError` for anything that is not valid."""
        if not isinstance(data, dict) or set(data) != {"profiles", "tablets", "state"}:
            raise _fail("parse", "The stored network settings are invalid")
        settings = cls()
        for key in ("profiles", "tablets", "state"):
            if not isinstance(data[key], dict):
                raise _fail("parse", "The stored network settings are invalid")
        for profile_id, profile in data["profiles"].items():
            if not isinstance(profile, dict) or set(profile) != {"wifi"}:
                raise _fail("parse", "The stored network settings are invalid")
            settings.profiles[profile_id] = {"wifi": _credentials(profile["wifi"], where="profile")}
        for tablet_id, entry in data["tablets"].items():
            if not isinstance(entry, dict) or set(entry) - {"wifi", "ip"}:
                raise _fail("parse", "The stored network settings are invalid")
            clean: dict[str, Any] = {}
            if "wifi" in entry:
                clean["wifi"] = _credentials(entry["wifi"], where="tablet")
            if "ip" in entry:
                clean["ip"] = _ip_settings(entry["ip"], ("wifi", "ethernet"))
            settings.tablets[tablet_id] = clean
        for tablet_id, entry in data["state"].items():
            settings.state[tablet_id] = _restore_state(entry)
        return settings


def _restore_state(entry: Any) -> dict[str, Any]:
    if (
        not isinstance(entry, dict)
        or set(entry) != {"revision", "desired", "result"}
        or type(entry["revision"]) is not int
        or entry["revision"] < 0
        or not isinstance(entry["desired"], dict)
    ):
        raise _fail("parse", "The stored network state is invalid")
    if entry["desired"]:
        validate_network(entry["desired"], ("wifi", "ethernet"))
    result = entry["result"]
    if result is not None:
        extra = {"applied": "proof", "reverted": "reason"}.get(
            result.get("result") if isinstance(result, dict) else None
        )
        if (
            extra is None
            or set(result) != {"revision", "result", "state", "at", extra}
            or type(result["revision"]) is not int
            or not 1 <= result["revision"] <= entry["revision"]
            or not isinstance(result["state"], dict)
            or type(result["at"]) not in (int, float)
        ):
            raise _fail("parse", "The stored network result is invalid")
    return json.loads(json.dumps(entry))
