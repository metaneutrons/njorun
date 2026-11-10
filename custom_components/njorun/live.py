# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Live commands and telemetry of the management session (decision D6).

Pure checks without Home Assistant imports. See docs/protocols/002-ha-management.md.
"""

from __future__ import annotations

from typing import Any

COMMAND_TTL = 10
RESULT_TIMEOUT = 15
MAX_PENDING_COMMANDS = 4
MAX_TELEMETRY_BYTES = 512
MAX_FIRMWARE_VERSION_CHARS = 32
COMMAND_REASONS = ("expired", "duplicate", "unsupported", "invalid", "failed")
RESET_REASONS = (
    "power_on",
    "software",
    "panic",
    "watchdog",
    "brownout",
    "deep_sleep",
    "external",
    "unknown",
)
_U32 = 4294967295


def _is_int(value: Any) -> bool:
    return type(value) is int


def valid_backlight(value: Any, *, partial: bool = False) -> bool:
    """`{on, brightness}` with brightness 0 to 100; a command may omit the brightness."""
    if not isinstance(value, dict) or "on" not in value or type(value["on"]) is not bool:
        return False
    expected = {"on", "brightness"}
    if set(value) - expected or (not partial and set(value) != expected):
        return False
    if "brightness" in value:
        return _is_int(value["brightness"]) and 0 <= value["brightness"] <= 100
    return True


def parse_command_result(payload: Any) -> tuple[str, str | None] | None:
    """Return (command_id, reason) for a valid result, reason None for success."""
    if not isinstance(payload, dict) or set(payload) != {"type", "command_id", "ok", "reason"}:
        return None
    command_id, ok, reason = payload["command_id"], payload["ok"], payload["reason"]
    if payload["type"] != "command_result" or not isinstance(command_id, str):
        return None
    if not 0 < len(command_id.encode()) <= 128 or any(c < " " or c == "\x7f" for c in command_id):
        return None
    if ok is True and reason is None:
        return command_id, None
    if ok is False and reason in COMMAND_REASONS:
        return command_id, reason
    return None


def parse_telemetry(payload: Any) -> dict[str, Any] | None:
    """Return the telemetry fields of a valid message, else None."""
    expected = {
        "type",
        "firmware_version",
        "uptime_s",
        "rssi_dbm",
        "free_heap",
        "reset_reason",
        "backlight",
    }
    if not isinstance(payload, dict) or set(payload) != expected or payload["type"] != "telemetry":
        return None
    version = payload["firmware_version"]
    if (
        not isinstance(version, str)
        or not 0 < len(version) <= MAX_FIRMWARE_VERSION_CHARS
        or any(c < " " or c == "\x7f" for c in version)
    ):
        return None
    for key in ("uptime_s", "free_heap"):
        if not _is_int(payload[key]) or not 0 <= payload[key] <= _U32:
            return None
    rssi = payload["rssi_dbm"]
    if rssi is not None and not (_is_int(rssi) and -127 <= rssi <= 0):
        return None
    if payload["reset_reason"] not in RESET_REASONS or not valid_backlight(payload["backlight"]):
        return None
    result = {key: payload[key] for key in expected - {"type"}}
    result["backlight"] = dict(payload["backlight"])
    return result
