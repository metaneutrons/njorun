# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0
"""The grant: a tablet's Home Assistant access, sealed to the tablet's key.

The announcement endpoint is public, so the refresh token that an approved tablet receives
must be unreadable for everybody else. It travels as one Noise message of the pattern
`Noise_N_25519_ChaChaPoly_SHA256`: Home Assistant is the initiator with a fresh ephemeral key,
the tablet is the responder with its static key (plan 004, message layout and test vector in
protocol/fixtures/grant.json).
"""

from __future__ import annotations

import base64
import json

from noise.connection import Keypair, NoiseConnection

PROTOCOL_NAME = b"Noise_N_25519_ChaChaPoly_SHA256"
PROLOGUE_PREFIX = b"njorun/grant\x01"


def _prologue(binding_id: str) -> bytes:
    return PROLOGUE_PREFIX + binding_id.encode("utf-8")


def seal_grant(
    tablet_public_key: bytes,
    binding_id: str,
    refresh_token: str,
    client_id: str,
    *,
    ephemeral_private_key: bytes | None = None,
) -> str:
    """Return the base64 of the Noise message that carries the tablet's access.

    `ephemeral_private_key` is only for the test vector; a real grant uses a fresh key.
    """
    if len(tablet_public_key) != 32:
        raise ValueError("The tablet key must be 32 bytes")
    payload = json.dumps(
        {"refresh_token": refresh_token, "client_id": client_id},
        separators=(",", ":"),
        ensure_ascii=False,
    ).encode("utf-8")
    noise = NoiseConnection.from_name(PROTOCOL_NAME)
    noise.set_as_initiator()
    noise.set_prologue(_prologue(binding_id))
    if ephemeral_private_key is not None:
        noise.set_keypair_from_private_bytes(Keypair.EPHEMERAL, ephemeral_private_key)
    noise.set_keypair_from_public_bytes(Keypair.REMOTE_STATIC, tablet_public_key)
    noise.start_handshake()
    return base64.b64encode(noise.write_message(payload)).decode("ascii")


def open_grant(tablet_private_key: bytes, binding_id: str, grant: str) -> dict[str, str]:
    """What the tablet does with a grant; here for the tests of both directions."""
    noise = NoiseConnection.from_name(PROTOCOL_NAME)
    noise.set_as_responder()
    noise.set_prologue(_prologue(binding_id))
    noise.set_keypair_from_private_bytes(Keypair.STATIC, tablet_private_key)
    noise.start_handshake()
    payload = json.loads(noise.read_message(base64.b64decode(grant, validate=True)))
    if (
        not isinstance(payload, dict)
        or set(payload) != {"refresh_token", "client_id"}
        or not all(isinstance(value, str) for value in payload.values())
    ):
        raise ValueError("The grant payload is malformed")
    return payload
