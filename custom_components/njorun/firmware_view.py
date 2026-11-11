# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""The HTTP endpoint a tablet downloads its firmware from (decision D7)."""

from __future__ import annotations

from aiohttp import web

from homeassistant.components.http import HomeAssistantView
from homeassistant.core import HomeAssistant

from .const import DOMAIN
from .firmware_serve import serve


class NjorunFirmwareView(HomeAssistantView):
    """`GET /api/njorun/firmware/{version}/{name}` with the install's bearer token.

    Home Assistant's own authentication is off because the tablet presents the single install
    token, not a user token; `serve` rejects everything else with 401.
    """

    url = "/api/njorun/firmware/{version}/{name}"
    name = "api:njorun:firmware"
    requires_auth = False

    async def get(self, request: web.Request, version: str, name: str) -> web.StreamResponse:
        hass: HomeAssistant = request.app["hass"]
        data = hass.data.get(DOMAIN, {})
        manager = data.get("firmware")
        if manager is None:
            return web.Response(status=401, headers={"WWW-Authenticate": "Bearer"})
        return await serve(request, version, name, manager.registry, manager.install_open)

    head = get
