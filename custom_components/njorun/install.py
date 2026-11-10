# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Firmware install messages of the management session (decision D7).

Pure checks without Home Assistant imports. See docs/protocols/002-ha-management.md.
"""

from __future__ import annotations

import re
from typing import Any

INSTALL_TTL = 300
RESULT_TIMEOUT = 15
OUTCOME_WINDOW = 1800
MAX_URL_CHARS = 512
MAX_VERSION_CHARS = 32
SLOT_BYTES = 8 * 1024 * 1024
REJECT_REASONS = ("expired", "duplicate", "busy", "incompatible", "too_large", "malformed")
STAGES = ("download", "verify", "write", "reboot")
OUTCOME_STATES = ("installed", "rolled_back")
CHANNELS = ("stable", "beta")
_U32 = 4294967295
_SHA256 = re.compile(r"[0-9a-f]{64}")
_TOKEN = re.compile(r"[A-Za-z0-9_-]{32,128}")
_URL = re.compile(r"https?://[^\s/?#@]+(?:/[^\s#]*)?")

INSTALL_FIELDS = {
    "type",
    "install_id",
    "version",
    "channel",
    "size",
    "sha256",
    "url",
    "token",
    "expires_at",
}


def _is_int(value: Any) -> bool:
    return type(value) is int


def _plain(text: Any, limit: int) -> bool:
    """A nonempty string of at most `limit` characters without control characters."""
    return (
        isinstance(text, str)
        and 0 < len(text) <= limit
        and not any(c < " " or c == "\x7f" for c in text)
    )


def valid_install_id(value: Any) -> bool:
    """Same rule as operation and command IDs: at most 128 UTF-8 bytes."""
    return _plain(value, 128) and len(value.encode()) <= 128


def valid_version(value: Any) -> bool:
    return _plain(value, MAX_VERSION_CHARS)


def valid_install(payload: Any) -> bool:
    """Whether `payload` is a well-formed `install` message (shape only)."""
    if not isinstance(payload, dict) or set(payload) != INSTALL_FIELDS:
        return False
    if payload["type"] != "install" or not valid_install_id(payload["install_id"]):
        return False
    if not valid_version(payload["version"]) or payload["channel"] not in CHANNELS:
        return False
    if not _is_int(payload["size"]) or not 0 < payload["size"] <= _U32:
        return False
    if not isinstance(payload["sha256"], str) or not _SHA256.fullmatch(payload["sha256"]):
        return False
    url = payload["url"]
    if not _plain(url, MAX_URL_CHARS) or not _URL.fullmatch(url):
        return False
    token = payload["token"]
    if not isinstance(token, str) or not _TOKEN.fullmatch(token):
        return False
    return _is_int(payload["expires_at"]) and payload["expires_at"] >= 0


def parse_install_result(payload: Any) -> tuple[str, str | None] | None:
    """Return (install_id, reason) for a valid result; reason None for `accepted`."""
    if not isinstance(payload, dict) or set(payload) != {"type", "install_id", "state", "reason"}:
        return None
    if payload["type"] != "install_result" or not valid_install_id(payload["install_id"]):
        return None
    if payload["state"] == "accepted" and payload["reason"] is None:
        return payload["install_id"], None
    if payload["state"] == "rejected" and payload["reason"] in REJECT_REASONS:
        return payload["install_id"], payload["reason"]
    return None


def parse_install_progress(payload: Any) -> tuple[str, str, int, int] | None:
    """Return (install_id, stage, done, total) for a valid progress message."""
    if not isinstance(payload, dict) or set(payload) != {
        "type",
        "install_id",
        "stage",
        "done",
        "total",
    }:
        return None
    if payload["type"] != "install_progress" or not valid_install_id(payload["install_id"]):
        return None
    done, total = payload["done"], payload["total"]
    if payload["stage"] not in STAGES or not _is_int(done) or not _is_int(total):
        return None
    if not 0 < total <= _U32 or not 0 <= done <= total:
        return None
    return payload["install_id"], payload["stage"], done, total


def parse_install_outcome(payload: Any) -> tuple[str, str, str] | None:
    """Return (install_id, state, version) for a valid final report."""
    if not isinstance(payload, dict) or set(payload) != {
        "type",
        "install_id",
        "state",
        "version",
    }:
        return None
    if payload["type"] != "install_outcome" or not valid_install_id(payload["install_id"]):
        return None
    if payload["state"] not in OUTCOME_STATES or not valid_version(payload["version"]):
        return None
    return payload["install_id"], payload["state"], payload["version"]
