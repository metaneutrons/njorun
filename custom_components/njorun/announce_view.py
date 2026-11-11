# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""`POST /api/njorun/announce`, the way a tablet asks to be paired (plan 004)."""

from __future__ import annotations

from aiohttp import web

from homeassistant.components.http import HomeAssistantView
from homeassistant.core import HomeAssistant

from .announce_http import MAX_BODY_BYTES, RateLimiter, handle_announce
from .const import DOMAIN


class NjorunAnnounceView(HomeAssistantView):
    """Home Assistant's own authentication is off: the tablet has no account yet.

    What `handle_announce` accepts is limited by size, by rate and by the runtime, and nothing
    in it is trusted until an administrator approves the tablet.
    """

    url = "/api/njorun/announce"
    name = "api:njorun:announce"
    requires_auth = False
    cors_allowed = False
    max_body_size = MAX_BODY_BYTES

    def __init__(self) -> None:
        self._limiter = RateLimiter()

    async def post(self, request: web.Request) -> web.Response:
        hass: HomeAssistant = request.app["hass"]
        runtimes = hass.data.get(DOMAIN, {}).get("entries", {})
        runtime = next(iter(runtimes.values()), None)
        body = await request.content.read(MAX_BODY_BYTES + 1)
        status, payload, headers = await handle_announce(
            runtime, self._limiter, request.remote, request.content_type, body
        )
        if payload is None:
            return web.Response(status=status, headers=headers)
        return web.json_response(payload, status=status, headers=headers)
