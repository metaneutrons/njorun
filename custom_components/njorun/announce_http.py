# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0
"""The announcement endpoint without sign-in (plan 004), as plain functions.

`announce_view.py` is the thin Home Assistant view around `handle_announce`. Nothing a caller
sends here is trusted: a tablet only becomes a tablet when an administrator compares its code
and approves it. The limits keep a faulty or hostile device from filling the Discovered list:
the body size, a rate per client address and, in the runtime, the number of waiting tablets.
"""

from __future__ import annotations

from collections import deque
import json
import time
from typing import Any, Callable

MAX_BODY_BYTES = 16 * 1024
FIELDS = {"protocol", "tablet_id", "name", "tablet_public_key", "capabilities", "need_grant"}
REQUIRED = FIELDS - {"need_grant"}
PROTOCOL_VERSION = 1


class RateLimiter:
    """At most `limit` requests per `window` seconds for each client address."""

    def __init__(
        self,
        limit: int = 60,
        window: float = 60.0,
        max_clients: int = 1024,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self._limit = limit
        self._window = window
        self._max_clients = max_clients
        self._clock = clock
        self._seen: dict[str, deque[float]] = {}

    def allow(self, client: str) -> tuple[bool, int]:
        """Whether the request may go on, and else the seconds to wait."""
        now = self._clock()
        if client not in self._seen and len(self._seen) >= self._max_clients:
            self._forget_idle(now)
            if len(self._seen) >= self._max_clients:
                return False, int(self._window)
        times = self._seen.setdefault(client, deque())
        while times and now - times[0] >= self._window:
            times.popleft()
        if len(times) >= self._limit:
            return False, max(1, int(self._window - (now - times[0])) + 1)
        times.append(now)
        return True, 0

    def _forget_idle(self, now: float) -> None:
        for client in [
            key
            for key, times in self._seen.items()
            if not times or now - times[-1] >= self._window
        ]:
            del self._seen[client]


Response = tuple[int, "dict[str, Any] | None", "dict[str, str]"]


def _invalid(detail: str) -> Response:
    return 400, {"error": "invalid", "detail": detail}, {}


async def handle_announce(
    runtime: Any,
    limiter: RateLimiter,
    remote: str | None,
    content_type: str | None,
    body: bytes,
) -> Response:
    """Answer one announcement: (status, JSON body or None, extra headers)."""
    if runtime is None:
        return 404, None, {}
    allowed, retry_after = limiter.allow(remote or "unknown")
    if not allowed:
        return 429, {"error": "rate_limited"}, {"Retry-After": str(retry_after)}
    if (content_type or "").split(";")[0].strip().lower() != "application/json":
        return _invalid("The body must be application/json")
    if len(body) > MAX_BODY_BYTES:
        return _invalid("The body is too large")
    try:
        message = json.loads(body)
    except (UnicodeDecodeError, ValueError):
        return _invalid("The body is not valid JSON")
    if not isinstance(message, dict):
        return _invalid("The body must be a JSON object")
    if set(message) - FIELDS or REQUIRED - set(message):
        return _invalid("Unexpected or missing fields")
    if type(message["protocol"]) is not int or message["protocol"] != PROTOCOL_VERSION:
        return _invalid("Unsupported protocol")
    try:
        result = await runtime.async_announce_public(
            tablet_id=message["tablet_id"],
            name=message["name"],
            tablet_public_key=message["tablet_public_key"],
            capabilities=message["capabilities"],
            need_grant=message.get("need_grant", False),
        )
    except ValueError as err:
        code = getattr(err, "announce_code", None)
        if code is not None:
            return 503, {"error": code}, {}
        return _invalid(str(err))
    return 200, result, {}
