# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""The authenticated download of a cached firmware release (decision D7).

A tablet that accepted an `install` fetches the manifest, its signature and the image from
Home Assistant with the bearer token of that command. One token belongs to one install: it
expires with the command, stops working as soon as the install is no longer open and is
revoked when sending failed. It works for repeated and ranged requests of the install's three
files, since a resume needs more than one request. Pure aiohttp logic without Home Assistant
imports; `firmware_view.py` mounts it.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
import hashlib
import re
import secrets
import time

from aiohttp import web

from .firmware import MANIFEST_NAME, SIGNATURE_NAME, CachedImage

_BEARER = re.compile(r"Bearer ([A-Za-z0-9_-]{32,128})")


def new_token() -> str:
    """32 random bytes as 43 characters of unpadded URL-safe base64."""
    return secrets.token_urlsafe(32)


def _digest(token: str) -> str:
    return hashlib.sha256(token.encode()).hexdigest()


@dataclass
class Grant:
    tablet_id: str
    image: CachedImage
    expires_at: float


class GrantRegistry:
    """Download grants by token digest; the token itself is never stored."""

    def __init__(self, clock: Callable[[], float] = time.time) -> None:
        self._grants: dict[str, Grant] = {}
        self._clock = clock

    def issue(self, tablet_id: str, image: CachedImage, expires_at: float) -> str:
        """A new token for `tablet_id`; earlier grants of that tablet end."""
        self.revoke(tablet_id)
        token = new_token()
        self._grants[_digest(token)] = Grant(tablet_id, image, expires_at)
        return token

    def set_expiry(self, token: str, expires_at: float) -> None:
        if (grant := self._grants.get(_digest(token))) is not None:
            grant.expires_at = expires_at

    def revoke(self, tablet_id: str) -> None:
        for key in [k for k, g in self._grants.items() if g.tablet_id == tablet_id]:
            del self._grants[key]

    def lookup(self, token: str) -> Grant | None:
        now = self._clock()
        for key in [k for k, g in self._grants.items() if g.expires_at <= now]:
            del self._grants[key]
        # The key is a SHA-256 of a 256-bit random token, so a lookup leaks nothing useful.
        return self._grants.get(_digest(token))

    def tablets_with_grants(self) -> set[str]:
        return {grant.tablet_id for grant in self._grants.values()}

    def versions_in_use(self) -> set[str]:
        return {grant.image.version for grant in self._grants.values()}


def _unauthorized() -> web.Response:
    return web.Response(
        status=401,
        headers={"WWW-Authenticate": "Bearer", "Cache-Control": "no-store"},
    )


async def serve(
    request: web.Request,
    version: str,
    name: str,
    registry: GrantRegistry,
    install_open: Callable[[str], bool],
) -> web.StreamResponse:
    """Answer one GET or HEAD for `name` of `version`; the image supports Range requests."""
    match = _BEARER.fullmatch(request.headers.get("Authorization", ""))
    grant = registry.lookup(match.group(1)) if match else None
    if grant is None or not install_open(grant.tablet_id):
        return _unauthorized()
    image = grant.image
    if version != image.version:
        return web.Response(status=404, headers={"Cache-Control": "no-store"})
    headers = {"Cache-Control": "no-store"}
    if name == MANIFEST_NAME:
        return web.Response(
            body=image.manifest_bytes, content_type="application/json", headers=headers
        )
    if name == SIGNATURE_NAME:
        return web.Response(body=image.signature, content_type="text/plain", headers=headers)
    if name == image.name:
        return web.FileResponse(
            image.image_path, headers={**headers, "Content-Type": "application/octet-stream"}
        )
    return web.Response(status=404, headers=headers)
