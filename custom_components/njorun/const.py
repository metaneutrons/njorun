# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Integration constants."""

from __future__ import annotations

DOMAIN = "njorun"
PROTOCOL_VERSION = 1
NOISE_NAME = b"Noise_IK_25519_ChaChaPoly_SHA256"
NOISE_PROLOGUE_DOMAIN = b"njorun/pairing"
MAX_PLAINTEXT_BYTES = 4096
MAX_HANDSHAKE_REQUEST_BYTES = 96
MAX_HANDSHAKE_RESPONSE_BYTES = 48
SESSION_HANDSHAKE_TTL = 60
SESSION_ACTIVE_TTL = 24 * 60 * 60
OPERATION_TTL = 60
STORE_VERSION = 1

WS_LIST = "njorun/list"
WS_CONFIGURE = "njorun/configure"
WS_ENROLL = "njorun/enroll"
WS_SESSION_START = "njorun/session_start"
WS_SESSION_HANDSHAKE = "njorun/session_handshake"
WS_SESSION_SUBSCRIBE = "njorun/session_subscribe"
WS_SESSION_FRAME = "njorun/session_frame"
WS_ANNOUNCE = "njorun/announce"

# Tablets waiting for an administrator: kept in memory, re-announced by the tablet.
ANNOUNCE_TTL = 10 * 60
MAX_ANNOUNCEMENTS = 16
# Sessions between session_start and the tablet's proven confirmation, per tablet.
MAX_HANDSHAKES_PER_TABLET = 3
COMPARE_DOMAIN = b"njorun/compare"
