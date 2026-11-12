# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Versioned authenticated Home Assistant WebSocket commands."""

from __future__ import annotations

import logging
from typing import Any

import probatio

from homeassistant.components import websocket_api
from homeassistant.core import HomeAssistant

from .const import (
    DOMAIN,
    PROTOCOL_VERSION,
    WS_ANNOUNCE,
    WS_CONFIGURE,
    WS_ENROLL,
    WS_INSTALL,
    WS_INSTALL_STATUS,
    WS_LIST,
    WS_SESSION_FRAME,
    WS_SESSION_HANDSHAKE,
    WS_SESSION_START,
    WS_SESSION_SUBSCRIBE,
)
from .runtime import NjorunRuntime, RuntimeError


def async_register_commands(hass: HomeAssistant) -> None:
    """Register each integration-wide command once during component setup."""
    for handler in (
        websocket_list,
        websocket_configure,
        websocket_enroll,
        websocket_announce,
        websocket_session_start,
        websocket_session_handshake,
        websocket_session_subscribe,
        websocket_session_frame,
        websocket_install,
        websocket_install_status,
    ):
        websocket_api.async_register_command(hass, handler)


@websocket_api.websocket_command({probatio.Required("type"): WS_LIST})
@websocket_api.require_admin
def websocket_list(
    hass: HomeAssistant, connection: websocket_api.ActiveConnection, msg: dict[str, Any]
) -> None:
    """List profiles and logical tablets without trust or session data."""
    try:
        runtime = _runtime(hass)
    except RuntimeError as err:
        connection.send_error(msg["id"], websocket_api.ERR_HOME_ASSISTANT_ERROR, str(err))
        return
    connection.send_result(msg["id"], runtime.configuration.dump())


@websocket_api.websocket_command(
    {
        probatio.Required("type"): WS_CONFIGURE,
        probatio.Required("operation"): str,
        probatio.Required("expected_revision"): int,
        probatio.Optional("profile_id"): str,
        probatio.Optional("tablet_id"): str,
        probatio.Optional("name"): str,
        probatio.Optional("settings"): dict,
        probatio.Optional("patch"): dict,
        probatio.Optional("overrides"): dict,
        probatio.Optional("clear_overrides"): bool,
        probatio.Optional("url"): str,
    }
)
@websocket_api.require_admin
@websocket_api.async_response
async def websocket_configure(
    hass: HomeAssistant, connection: websocket_api.ActiveConnection, msg: dict[str, Any]
) -> None:
    """Apply an administrator-authorized configuration mutation."""
    try:
        runtime = _runtime(hass)
    except RuntimeError as err:
        connection.send_error(msg["id"], websocket_api.ERR_HOME_ASSISTANT_ERROR, str(err))
        return
    args = {key: value for key, value in msg.items() if key not in {"id", "type", "expected_revision"}}
    try:
        revision = await runtime.async_configure(
            args, expected_revision=msg["expected_revision"]
        )
    except (RuntimeError, ValueError) as err:
        connection.send_error(msg["id"], websocket_api.ERR_INVALID_FORMAT, str(err))
        return
    connection.send_result(msg["id"], {"revision": revision})


@websocket_api.websocket_command(
    {
        probatio.Required("type"): WS_ENROLL,
        probatio.Required("tablet_id"): str,
        probatio.Required("name"): str,
        probatio.Required("profile_id"): str,
        probatio.Required("tablet_public_key"): str,
        probatio.Required("capabilities"): dict,
        probatio.Required("expected_revision"): int,
    }
)
@websocket_api.require_admin
@websocket_api.async_response
async def websocket_enroll(
    hass: HomeAssistant, connection: websocket_api.ActiveConnection, msg: dict[str, Any]
) -> None:
    """Approve a pending binding; the tablet must still prove its private key."""
    try:
        result = await _runtime(hass).async_enroll(
            tablet_id=msg["tablet_id"],
            name=msg["name"],
            profile_id=msg["profile_id"],
            tablet_public_key=msg["tablet_public_key"],
            capabilities=msg["capabilities"],
            expected_revision=msg["expected_revision"],
        )
    except (RuntimeError, ValueError) as err:
        connection.send_error(msg["id"], websocket_api.ERR_INVALID_FORMAT, str(err))
        return
    connection.send_result(msg["id"], result)


@websocket_api.websocket_command(
    {
        probatio.Required("type"): WS_ANNOUNCE,
        probatio.Required("protocol"): int,
        probatio.Required("tablet_id"): str,
        probatio.Required("name"): str,
        probatio.Required("tablet_public_key"): str,
        probatio.Required("capabilities"): dict,
    }
)
def websocket_announce(
    hass: HomeAssistant, connection: websocket_api.ActiveConnection, msg: dict[str, Any]
) -> None:
    """Let a signed-in tablet ask for pairing; an administrator approves it in the UI."""
    if type(msg["protocol"]) is not int or msg["protocol"] != PROTOCOL_VERSION:
        connection.send_error(msg["id"], websocket_api.ERR_INVALID_FORMAT, "Unsupported protocol")
        return
    try:
        result = _runtime(hass).async_announce(
            connection,
            tablet_id=msg["tablet_id"],
            name=msg["name"],
            tablet_public_key=msg["tablet_public_key"],
            capabilities=msg["capabilities"],
        )
    except RuntimeError as err:
        connection.send_error(msg["id"], websocket_api.ERR_INVALID_FORMAT, str(err))
        return
    connection.send_result(msg["id"], result)


@websocket_api.websocket_command(
    {
        probatio.Required("type"): WS_SESSION_START,
        probatio.Required("protocol"): int,
        probatio.Required("binding_id"): str,
    }
)
def websocket_session_start(
    hass: HomeAssistant, connection: websocket_api.ActiveConnection, msg: dict[str, Any]
) -> None:
    """Start an authenticated session; later commands also require Noise proof."""
    if type(msg["protocol"]) is not int or msg["protocol"] != PROTOCOL_VERSION:
        connection.send_error(msg["id"], websocket_api.ERR_INVALID_FORMAT, "Unsupported protocol")
        return
    try:
        result = _runtime(hass).async_start_session(connection, msg["binding_id"])
    except RuntimeError as err:
        connection.send_error(msg["id"], websocket_api.ERR_UNAUTHORIZED, str(err))
        return
    connection.send_result(msg["id"], result)


@websocket_api.websocket_command(
    {
        probatio.Required("type"): WS_SESSION_HANDSHAKE,
        probatio.Required("session_id"): str,
        probatio.Required("frame"): str,
    }
)
def websocket_session_handshake(
    hass: HomeAssistant, connection: websocket_api.ActiveConnection, msg: dict[str, Any]
) -> None:
    """Complete the responder flight only after the stored tablet pin matches."""
    try:
        frame = _runtime(hass).async_handshake(
            connection, msg["session_id"], msg["frame"]
        )
    except RuntimeError as err:
        connection.send_error(msg["id"], websocket_api.ERR_UNAUTHORIZED, str(err))
        return
    connection.send_result(msg["id"], {"frame": frame})


@websocket_api.websocket_command(
    {
        probatio.Required("type"): WS_SESSION_SUBSCRIBE,
        probatio.Required("session_id"): str,
        probatio.Required("frame"): str,
    }
)
@websocket_api.async_response
async def websocket_session_subscribe(
    hass: HomeAssistant, connection: websocket_api.ActiveConnection, msg: dict[str, Any]
) -> None:
    """Confirm the Noise peer, activate pending trust and start config events."""
    try:
        runtime = _runtime(hass)
        session = await runtime.async_confirm_and_subscribe(
            connection,
            msg["session_id"],
            msg["frame"],
            msg["id"],
        )
    except (RuntimeError, ValueError) as err:
        connection.send_error(msg["id"], websocket_api.ERR_UNAUTHORIZED, str(err))
        return
    # `live` lists the live parts of decision D6 that the tablet reported and HA implements.
    # `ota` is true when the tablet reported the install capability of decision D7, `network`
    # when it reported interfaces for the network messages of plan 006.
    connection.send_result(
        msg["id"],
        {
            "session_id": session.session_id,
            "live": list(session.offered),
            "ota": session.ota,
            "network": session.network,
        },
    )
    try:
        await runtime.async_send_initial_configuration(session)
    except RuntimeError as err:
        # The request already succeeded and HA has installed the subscription.
        # Sending another response with the same id would violate WS semantics.
        _LOGGER.warning("Could not send initial Njörun configuration: %s", err)


@websocket_api.websocket_command(
    {
        probatio.Required("type"): WS_SESSION_FRAME,
        probatio.Required("session_id"): str,
        probatio.Required("frame"): str,
    }
)
@websocket_api.async_response
async def websocket_session_frame(
    hass: HomeAssistant, connection: websocket_api.ActiveConnection, msg: dict[str, Any]
) -> None:
    """Consume one encrypted applied-state acknowledgement."""
    try:
        await _runtime(hass).async_receive_ack(
            connection, msg["session_id"], msg["frame"]
        )
    except (RuntimeError, ValueError) as err:
        connection.send_error(msg["id"], websocket_api.ERR_INVALID_FORMAT, str(err))
        return
    connection.send_result(msg["id"], {"accepted": True})


@websocket_api.websocket_command(
    {
        probatio.Required("type"): WS_INSTALL,
        probatio.Required("tablet_id"): str,
        probatio.Required("version"): str,
        probatio.Required("channel"): str,
        probatio.Required("size"): int,
        probatio.Required("sha256"): str,
        probatio.Required("url"): str,
        probatio.Required("token"): str,
        probatio.Optional("downgrade"): bool,
    }
)
@websocket_api.require_admin
@websocket_api.async_response
async def websocket_install(
    hass: HomeAssistant, connection: websocket_api.ActiveConnection, msg: dict[str, Any]
) -> None:
    """Send one install (decision D7) and wait for the tablet's answer.

    Low-level and administrator-only: the caller supplies the download URL and token. The update
    entity (item 6) uses the runtime directly.
    """
    try:
        install_id = await _runtime(hass).async_send_install(
            msg["tablet_id"],
            **{key: msg[key] for key in ("version", "channel", "size", "sha256", "url", "token")},
            downgrade=msg.get("downgrade", False),
        )
    except (RuntimeError, ValueError) as err:
        connection.send_error(msg["id"], websocket_api.ERR_INVALID_FORMAT, str(err))
        return
    connection.send_result(msg["id"], {"install_id": install_id})


@websocket_api.websocket_command(
    {probatio.Required("type"): WS_INSTALL_STATUS, probatio.Required("tablet_id"): str}
)
@websocket_api.require_admin
def websocket_install_status(
    hass: HomeAssistant, connection: websocket_api.ActiveConnection, msg: dict[str, Any]
) -> None:
    """The latest install of a tablet (decision D7), without its token or URL."""
    try:
        status = _runtime(hass).install_status(msg["tablet_id"])
    except RuntimeError as err:
        connection.send_error(msg["id"], websocket_api.ERR_HOME_ASSISTANT_ERROR, str(err))
        return
    connection.send_result(msg["id"], {"install": status})


def _runtime(hass: HomeAssistant) -> NjorunRuntime:
    """Resolve the single loaded Njörun entry without exposing its stored trust."""
    entries = hass.data.get(DOMAIN, {}).get("entries", {})
    if len(entries) != 1:
        raise RuntimeError("Njörun is not configured")
    return next(iter(entries.values()))
_LOGGER = logging.getLogger(__name__)
