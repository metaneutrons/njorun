# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Persistent configuration, per-binding Noise sessions and applied state."""

from __future__ import annotations

import asyncio
import base64
from collections.abc import Callable
from dataclasses import dataclass
import json
import logging
import math
import secrets
import time
from typing import Any
import uuid

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant
from homeassistant.helpers.storage import Store
from noise.connection import Keypair, NoiseConnection

from .const import (
    DOMAIN,
    MAX_HANDSHAKE_REQUEST_BYTES,
    MAX_HANDSHAKE_RESPONSE_BYTES,
    MAX_PLAINTEXT_BYTES,
    NOISE_NAME,
    NOISE_PROLOGUE_DOMAIN,
    OPERATION_TTL,
    PROTOCOL_VERSION,
    SESSION_ACTIVE_TTL,
    SESSION_HANDSHAKE_TTL,
    STORE_VERSION,
)
from .model import (
    Configuration,
    ConfigurationError,
    validate_capabilities,
    validate_effective,
    validate_settings,
)

_MAX_STORED_BYTES = 65536
_MAX_FRAME_BYTES = MAX_PLAINTEXT_BYTES + 16
_B64_KEY_LEN = 44
_LOGGER = logging.getLogger(__name__)


class RuntimeError(ValueError):
    """A safe, user-facing Njörun configuration or transport error."""


@dataclass
class Session:
    """One ephemeral Noise state bound to one HA WebSocket connection."""

    session_id: str
    binding_id: str
    tablet_id: str
    context: bytes
    connection: Any
    expires_at: float
    phase: str = "await_handshake"
    noise: NoiseConnection | None = None
    subscription_id: int | None = None
    expiry_handle: asyncio.TimerHandle | None = None


@dataclass
class PendingOperation:
    """An unexpired configuration request awaiting an authenticated ACK."""

    operation_id: str
    session_id: str
    binding_id: str
    tablet_id: str
    revision: int
    settings: dict[str, Any]
    expires_at: float


class NjorunRuntime:
    """Runtime adapter; configuration and trust are durably stored together."""

    def __init__(self, hass: HomeAssistant, entry: ConfigEntry) -> None:
        self.hass = hass
        self.entry = entry
        self.store: Store[dict[str, Any]] = Store(
            hass,
            STORE_VERSION,
            f"{DOMAIN}.{entry.entry_id}",
            private=True,
            atomic_writes=True,
        )
        self.configuration = Configuration()
        self.bindings: dict[str, dict[str, Any]] = {}
        self.applied: dict[str, dict[str, Any]] = {}
        self.sessions: dict[str, Session] = {}
        self.session_by_tablet: dict[str, str] = {}
        self.pending_operations: dict[str, PendingOperation] = {}
        self.completed_operations: set[str] = set()
        self.entity_adders: dict[str, Callable[..., Any]] = {}
        self.entities: dict[str, set[Any]] = {}
        self._registered_platforms: set[str] = set()
        self._entities_created: set[tuple[str, str]] = set()
        self._lock = asyncio.Lock()
        self._closed = False

    @classmethod
    async def async_load(cls, hass: HomeAssistant, entry: ConfigEntry) -> NjorunRuntime:
        """Load and validate durable configuration/trust, creating a default profile."""
        runtime = cls(hass, entry)
        stored = await runtime.store.async_load()
        if stored is None:
            profile_name = entry.data.get("initial_profile_name", "Home")
            runtime.configuration.add_profile("default", profile_name, {}, 0)
            await runtime._async_save()
            return runtime
        runtime._restore(stored)
        return runtime

    def _restore(self, stored: dict[str, Any]) -> None:
        """Reject corrupt persisted data before exposing entities or commands."""
        if (
            not isinstance(stored, dict)
            or set(stored) != {"version", "configuration", "bindings", "applied"}
            or type(stored["version"]) is not int
            or stored["version"] != 1
            or not isinstance(stored["bindings"], dict)
            or not isinstance(stored["applied"], dict)
        ):
            raise RuntimeError("Stored Njörun state is invalid; remove and reconfigure the integration")
        data = json.dumps(stored, separators=(",", ":"), ensure_ascii=False).encode()
        if len(data) > _MAX_STORED_BYTES:
            raise RuntimeError("Stored Njörun state exceeds its size limit")
        configuration = Configuration.restore(stored["configuration"])
        bindings: dict[str, dict[str, Any]] = {}
        for binding_id, binding in stored["bindings"].items():
            if (
                not isinstance(binding_id, str)
                or len(binding_id) > 128
                or not isinstance(binding, dict)
                or binding.get("status") not in {"pending", "active", "revoked"}
                or not isinstance(binding.get("tablet_id"), str)
                or binding["tablet_id"] not in configuration.tablets
            ):
                raise RuntimeError("Stored Njörun binding is invalid")
            if binding["status"] in {"pending", "active"}:
                _decode_key(binding.get("tablet_public_key"))
                _decode_key(binding.get("ha_private_key"))
            bindings[binding_id] = dict(binding)
        applied: dict[str, dict[str, Any]] = {}
        for tablet_id, record in stored["applied"].items():
            if tablet_id not in configuration.tablets or not isinstance(record, dict):
                raise RuntimeError("Stored Njörun applied state is invalid")
            settings = validate_settings(record.get("settings"))
            validate_effective(settings, configuration.tablets[tablet_id]["capabilities"])
            revision = record.get("revision")
            if type(revision) is not int or revision < 0:
                raise RuntimeError("Stored Njörun applied revision is invalid")
            applied[tablet_id] = {
                "revision": revision,
                "settings": settings,
                "applied_at": _safe_timestamp(record.get("applied_at")),
            }
        self.configuration = configuration
        self.bindings = bindings
        self.applied = applied

    def _stored_data(
        self,
        *,
        configuration: Configuration | None = None,
        bindings: dict[str, dict[str, Any]] | None = None,
        applied: dict[str, dict[str, Any]] | None = None,
    ) -> dict[str, Any]:
        data = {
            "version": 1,
            "configuration": (configuration or self.configuration).dump(),
            "bindings": bindings if bindings is not None else self.bindings,
            "applied": applied if applied is not None else self.applied,
        }
        encoded = json.dumps(data, separators=(",", ":"), ensure_ascii=False).encode()
        if len(encoded) > _MAX_STORED_BYTES:
            raise RuntimeError("Njörun state exceeds its storage limit")
        return data

    async def _async_save(
        self,
        *,
        configuration: Configuration | None = None,
        bindings: dict[str, dict[str, Any]] | None = None,
        applied: dict[str, dict[str, Any]] | None = None,
    ) -> None:
        """Atomically store a complete candidate before changing observable state."""
        await self.store.async_save(
            self._stored_data(
                configuration=configuration, bindings=bindings, applied=applied
            )
        )

    def register_platform(self, platform: str, add_entities: Callable[..., Any]) -> None:
        """Remember HA's platform adder for tablets enrolled after setup."""
        self.entity_adders[platform] = add_entities
        if platform in self._registered_platforms:
            return
        self._registered_platforms.add(platform)
        for tablet_id in self.configuration.tablets:
            self._add_tablet_entities(platform, tablet_id)

    def _add_tablet_entities(self, platform: str, tablet_id: str) -> None:
        """Create a tablet's native HA entities once per loaded platform."""
        adder = self.entity_adders.get(platform)
        if adder is None:
            return
        key = (platform, tablet_id)
        if key in self._entities_created:
            return
        from . import number as number_platform
        from . import select as select_platform

        if platform == "select":
            entities = select_platform.create_entities(self, tablet_id)
        elif platform == "number":
            entities = number_platform.create_entities(self, tablet_id)
        else:
            return
        if entities:
            self._entities_created.add(key)
            adder(entities)

    def async_add_tablet_entities(self, tablet_id: str) -> None:
        """Expose newly enrolled tablets on every already-loaded platform."""
        for platform in self._registered_platforms:
            self._add_tablet_entities(platform, tablet_id)

    def add_entity(self, tablet_id: str, entity: Any) -> None:
        """Track an entity so confirmed state and availability can be written."""
        self.entities.setdefault(tablet_id, set()).add(entity)

    def remove_entity(self, tablet_id: str, entity: Any) -> None:
        """Stop tracking an entity after its platform unloads."""
        if entities := self.entities.get(tablet_id):
            entities.discard(entity)
            if not entities:
                self.entities.pop(tablet_id, None)

    def async_write_tablet_state(self, tablet_id: str) -> None:
        """Publish only the last persisted applied state to native entities."""
        for entity in tuple(self.entities.get(tablet_id, ())):
            entity.async_write_ha_state()

    def is_available(self, tablet_id: str) -> bool:
        """Availability means this logical tablet has a live proven session."""
        session_id = self.session_by_tablet.get(tablet_id)
        session = self.sessions.get(session_id or "")
        return bool(
            not self._closed
            and session
            and session.phase == "active"
            and session.expires_at > time.time()
        )

    async def async_enroll(
        self,
        *,
        tablet_id: str,
        name: str,
        profile_id: str,
        tablet_public_key: str,
        capabilities: Any,
        expected_revision: int,
    ) -> dict[str, Any]:
        """Create an admin-approved pending binding without claiming device proof."""
        if type(expected_revision) is not int:
            raise RuntimeError("expected_revision must be an integer")
        tablet_public = _decode_key(tablet_public_key)
        valid_capabilities = validate_capabilities(capabilities)
        async with self._lock:
            if expected_revision != self.configuration.revision:
                raise RuntimeError("Configuration changed; refresh and retry")
            if any(
                b["tablet_id"] == tablet_id and b["status"] in {"pending", "active"}
                for b in self.bindings.values()
            ):
                raise RuntimeError("Tablet already has a pending or active binding")
            candidate = Configuration.restore(self.configuration.dump())
            existing = candidate.tablets.get(tablet_id)
            if existing is None:
                candidate.add_tablet(
                    tablet_id,
                    name,
                    profile_id,
                    valid_capabilities,
                    expected_revision,
                )
            else:
                if profile_id != existing["profile_id"]:
                    raise RuntimeError("Assign the logical tablet to its profile before replacement")
                if existing["capabilities"] != valid_capabilities:
                    raise RuntimeError("Replacement capabilities differ; resolve them before pairing")
            ha_private, ha_public = _generate_static_keypair()
            binding_id = uuid.uuid4().hex
            now = time.time()
            candidate_bindings = {
                key: dict(value) for key, value in self.bindings.items()
            }
            candidate_bindings[binding_id] = {
                "tablet_id": tablet_id,
                "tablet_public_key": base64.b64encode(tablet_public).decode("ascii"),
                "ha_private_key": base64.b64encode(ha_private).decode("ascii"),
                "status": "pending",
                "created_at": now,
                "expires_at": now + 24 * 60 * 60,
            }
            await self._async_save(
                configuration=candidate, bindings=candidate_bindings
            )
            self.configuration = candidate
            self.bindings = candidate_bindings
        self.async_add_tablet_entities(tablet_id)
        self.async_write_tablet_state(tablet_id)
        return {
            "binding_id": binding_id,
            "ha_public_key": base64.b64encode(ha_public).decode("ascii"),
            "status": "pending",
            "revision": self.configuration.revision,
        }

    def async_start_session(self, connection: Any, binding_id: str) -> dict[str, Any]:
        """Issue a fresh context for an authenticated connection and pinned binding."""
        self._expire_sessions()
        user = getattr(connection, "user", None)
        if user is None:
            raise RuntimeError("Authenticated Home Assistant session required")
        binding = self.bindings.get(binding_id)
        if binding is None or binding["status"] not in {"pending", "active"}:
            raise RuntimeError("Unknown or revoked tablet binding")
        if binding["status"] == "pending" and binding["expires_at"] <= time.time():
            raise RuntimeError("Enrollment has expired; approve it again")
        tablet_id = binding["tablet_id"]
        existing_session_id = self.session_by_tablet.get(tablet_id)
        if existing_session_id and existing_session_id in self.sessions:
            raise RuntimeError("Tablet already has a live management session")
        session_id = secrets.token_urlsafe(24)
        context = secrets.token_bytes(32)
        session = Session(
            session_id=session_id,
            binding_id=binding_id,
            tablet_id=tablet_id,
            context=context,
            connection=connection,
            expires_at=time.time() + SESSION_HANDSHAKE_TTL,
        )
        self.sessions[session_id] = session
        self.session_by_tablet[tablet_id] = session_id
        session.expiry_handle = self.hass.loop.call_later(
            SESSION_HANDSHAKE_TTL, self._expire_one_session, session_id
        )
        return {
            "session_id": session_id,
            "context": base64.b64encode(context).decode("ascii"),
            "expires_at": int(session.expires_at),
        }

    def async_handshake(self, connection: Any, session_id: str, frame_text: str) -> str:
        """Verify the IK initiator pin before producing responder flight two."""
        session = self._require_session(connection, session_id, "await_handshake")
        try:
            frame = _decode_frame(frame_text, maximum=MAX_HANDSHAKE_REQUEST_BYTES)
        except RuntimeError:
            self._close_session(session_id)
            raise
        if len(frame) != MAX_HANDSHAKE_REQUEST_BYTES:
            self._close_session(session_id)
            raise RuntimeError("Invalid Noise handshake frame")
        binding = self.bindings.get(session.binding_id)
        if not binding or binding["status"] not in {"pending", "active"}:
            self._close_session(session_id)
            raise RuntimeError("Tablet binding is no longer active")
        try:
            noise = NoiseConnection.from_name(NOISE_NAME)
            noise.set_as_responder()
            noise.set_keypair_from_private_bytes(
                Keypair.STATIC, _decode_key(binding.get("ha_private_key"))
            )
            prologue = NOISE_PROLOGUE_DOMAIN + bytes([PROTOCOL_VERSION]) + session.context
            noise.set_prologue(prologue)
            noise.start_handshake()
            payload = bytes(noise.read_message(frame))
            handshake = noise.noise_protocol.handshake_state
            peer_static = bytes(handshake.rs.public_bytes)
            expected_static = _decode_key(binding.get("tablet_public_key"))
            if payload or peer_static != expected_static:
                self._close_session(session_id)
                raise RuntimeError("Tablet identity proof failed")
            response = bytes(noise.write_message(b""))
        except RuntimeError:
            raise
        except Exception as err:
            self._close_session(session_id)
            raise RuntimeError("Noise handshake failed") from err
        if len(response) != MAX_HANDSHAKE_RESPONSE_BYTES or not noise.handshake_finished:
            self._close_session(session_id)
            raise RuntimeError("Noise handshake failed")
        session.context = b""
        session.noise = noise
        session.phase = "await_confirm"
        return base64.b64encode(response).decode("ascii")

    async def async_confirm_and_subscribe(
        self,
        connection: Any,
        session_id: str,
        frame_text: str,
        subscription_id: int,
    ) -> Session:
        """Decrypt device confirmation, persist activation and subscribe to config."""
        session = self._require_session(connection, session_id, "await_confirm")
        if session.noise is None:
            self._close_session(session_id)
            raise RuntimeError("Noise handshake is incomplete")
        try:
            frame = _decode_frame(frame_text, maximum=_MAX_FRAME_BYTES)
        except RuntimeError:
            self._close_session(session_id)
            raise
        try:
            payload = json.loads(session.noise.decrypt(frame))
        except Exception as err:
            self._close_session(session_id)
            raise RuntimeError("Encrypted tablet confirmation failed") from err
        expected = {
            "type": "confirm",
            "protocol": PROTOCOL_VERSION,
            "binding_id": session.binding_id,
            "session_id": session.session_id,
        }
        if payload != expected:
            self._close_session(session_id)
            raise RuntimeError("Tablet confirmation does not match this binding")
        if subscription_id in session.connection.subscriptions:
            self._close_session(session_id)
            raise RuntimeError("Subscription identifier is already in use")

        async with self._lock:
            binding = self.bindings.get(session.binding_id)
            if binding is None or binding["status"] not in {"pending", "active"}:
                self._close_session(session_id)
                raise RuntimeError("Tablet binding is no longer active")
            if binding["status"] == "pending":
                candidate_bindings = {
                    key: dict(value) for key, value in self.bindings.items()
                }
                candidate_bindings[session.binding_id]["status"] = "active"
                candidate_bindings[session.binding_id].pop("expires_at", None)
                await self._async_save(bindings=candidate_bindings)
                self.bindings = candidate_bindings
        session.phase = "active"
        session.expires_at = time.time() + SESSION_ACTIVE_TTL
        session.subscription_id = subscription_id
        if session.expiry_handle is not None:
            session.expiry_handle.cancel()
        session.expiry_handle = self.hass.loop.call_later(
            SESSION_ACTIVE_TTL, self._expire_one_session, session_id
        )

        def unsubscribe() -> None:
            """Close the session when HA or the tablet unsubscribes."""
            self._close_session(session_id, detach_subscription=False)

        session.connection.subscriptions[subscription_id] = unsubscribe
        self.async_write_tablet_state(session.tablet_id)
        return session

    async def async_send_initial_configuration(self, session: Session) -> None:
        """Send a fresh snapshot after the HA command's subscription result."""
        await self._send_configuration(session)

    async def async_receive_ack(
        self, connection: Any, session_id: str, frame_text: str
    ) -> None:
        """Persist a matching, authenticated device ACK before publishing state."""
        session = self._require_session(connection, session_id, "active")
        if session.noise is None:
            raise RuntimeError("Management session is unavailable")
        try:
            frame = _decode_frame(frame_text, maximum=_MAX_FRAME_BYTES)
        except RuntimeError:
            self._close_session(session_id)
            raise
        try:
            payload = json.loads(session.noise.decrypt(frame))
        except Exception as err:
            self._close_session(session_id)
            raise RuntimeError("Encrypted tablet acknowledgement failed") from err
        if not isinstance(payload, dict) or set(payload) != {
            "type", "operation_id", "revision", "settings"
        } or payload.get("type") != "applied":
            self._close_session(session_id)
            raise RuntimeError("Invalid tablet acknowledgement")
        operation_id = payload.get("operation_id")
        if not isinstance(operation_id, str) or len(operation_id) > 128:
            raise RuntimeError("Invalid operation identifier")
        if operation_id in self.completed_operations:
            raise RuntimeError("Operation was already acknowledged")
        operation = self.pending_operations.get(operation_id)
        if (
            operation is None
            or operation.expires_at <= time.time()
            or operation.session_id != session_id
            or operation.binding_id != session.binding_id
            or operation.tablet_id != session.tablet_id
            or payload.get("revision") != operation.revision
        ):
            raise RuntimeError("No matching configuration operation is pending")
        revision = payload.get("revision")
        if type(revision) is not int or revision != operation.revision:
            raise RuntimeError("No matching configuration operation is pending")
        actual = validate_settings(payload.get("settings"))
        async with self._lock:
            # Configuration changes and revocation also hold this lock. Repeat
            # every state-dependent check here so an ACK queued behind removal
            # or revocation cannot commit a stale copy of applied state.
            current_session = self.sessions.get(session_id)
            current_binding = self.bindings.get(session.binding_id)
            current_operation = self.pending_operations.get(operation_id)
            tablet = self.configuration.tablets.get(session.tablet_id)
            if (
                current_session is not session
                or session.connection is not connection
                or session.phase != "active"
                or session.expires_at <= time.time()
                or current_binding is None
                or current_binding.get("status") != "active"
                or current_binding.get("tablet_id") != session.tablet_id
                or current_operation is not operation
                or operation.expires_at <= time.time()
                or tablet is None
                or tablet["revision"] != operation.revision
                or _effective_settings(self.configuration, session.tablet_id)
                != operation.settings
            ):
                raise RuntimeError("No matching configuration operation is pending")
            validate_effective(actual, tablet["capabilities"])
            if actual != operation.settings:
                raise RuntimeError("Tablet acknowledgement differs from its configuration")
            previous = self.applied.get(session.tablet_id)
            if previous and operation.revision < previous["revision"]:
                raise RuntimeError("Stale tablet acknowledgement")
            candidate_applied = dict(self.applied)
            candidate_applied[session.tablet_id] = {
                "revision": operation.revision,
                "settings": actual,
                "applied_at": time.time(),
            }
            await self._async_save(applied=candidate_applied)
            self.applied = candidate_applied
            self.pending_operations.pop(operation_id, None)
            self.completed_operations.add(operation_id)
            if len(self.completed_operations) > 1024:
                self.completed_operations = set(list(self.completed_operations)[-512:])
        self.async_write_tablet_state(session.tablet_id)

    async def async_configure(
        self, args: dict[str, Any], *, expected_revision: int
    ) -> int:
        """Validate and atomically persist one configuration or binding mutation."""
        allowed_fields = {
            "add_profile": {"profile_id", "name", "settings"},
            "edit_profile": {"profile_id", "patch"},
            "delete_profile": {"profile_id"},
            "set_overrides": {"tablet_id", "overrides"},
            "assign": {"tablet_id", "profile_id", "clear_overrides"},
            "remove_tablet": {"tablet_id"},
            "revoke": {"tablet_id"},
        }
        required_fields = {
            "add_profile": {"profile_id", "name"},
            "edit_profile": {"profile_id", "patch"},
            "delete_profile": {"profile_id"},
            "set_overrides": {"tablet_id", "overrides"},
            "assign": {"tablet_id", "profile_id"},
            "remove_tablet": {"tablet_id"},
            "revoke": {"tablet_id"},
        }
        if not isinstance(args, dict) or not isinstance(args.get("operation"), str):
            raise RuntimeError("Invalid configuration operation")
        operation = args["operation"]
        if operation not in allowed_fields:
            raise RuntimeError("Unsupported configuration operation")
        supplied_fields = set(args) - {"operation"}
        if supplied_fields - allowed_fields[operation] or required_fields[operation] - supplied_fields:
            raise RuntimeError("Invalid fields for configuration operation")

        async with self._lock:
            if type(expected_revision) is not int or expected_revision != self.configuration.revision:
                raise RuntimeError("Configuration changed; refresh and retry")
            candidate = Configuration.restore(self.configuration.dump())
            candidate_bindings = {key: dict(value) for key, value in self.bindings.items()}
            before_tablets = set(candidate.tablets)
            before_tablet_revisions = {
                tablet_id: tablet["revision"]
                for tablet_id, tablet in candidate.tablets.items()
            }
            candidate_applied = dict(self.applied)
            try:
                if operation == "add_profile":
                    candidate.add_profile(
                        args["profile_id"],
                        args["name"],
                        args.get("settings", {}),
                        expected_revision,
                    )
                elif operation == "edit_profile":
                    candidate.edit_profile(
                        args["profile_id"], args["patch"], expected_revision
                    )
                elif operation == "delete_profile":
                    candidate.delete_profile(args["profile_id"], expected_revision)
                elif operation == "set_overrides":
                    candidate.set_overrides(
                        args["tablet_id"], args["overrides"], expected_revision
                    )
                elif operation == "assign":
                    clear = args.get("clear_overrides", False)
                    if type(clear) is not bool:
                        raise RuntimeError("clear_overrides must be a boolean")
                    candidate.assign(
                        args["tablet_id"],
                        args["profile_id"],
                        expected_revision,
                        clear=clear,
                    )
                elif operation == "remove_tablet":
                    candidate.remove_tablet(args["tablet_id"], expected_revision)
                    candidate_applied.pop(args["tablet_id"], None)
                    for binding_id, binding in list(candidate_bindings.items()):
                        if binding["tablet_id"] == args["tablet_id"]:
                            del candidate_bindings[binding_id]
                elif operation == "revoke":
                    tablet_id = args["tablet_id"]
                    if tablet_id not in candidate.tablets:
                        raise RuntimeError("Unknown tablet")
                    found = False
                    for binding in candidate_bindings.values():
                        if binding["tablet_id"] == tablet_id and binding["status"] != "revoked":
                            binding["status"] = "revoked"
                            binding.pop("tablet_public_key", None)
                            binding.pop("ha_private_key", None)
                            binding.pop("expires_at", None)
                            found = True
                    if not found:
                        raise RuntimeError("Tablet has no active binding")
            except ConfigurationError as err:
                raise RuntimeError(str(err)) from err

            await self._async_save(
                configuration=candidate,
                bindings=candidate_bindings,
                applied=candidate_applied,
            )
            self.configuration = candidate
            self.bindings = candidate_bindings
            self.applied = candidate_applied
            affected = {
                tablet_id for tablet_id in candidate.tablets
                if before_tablet_revisions.get(tablet_id)
                != candidate.tablets[tablet_id]["revision"]
            }
            removed = before_tablets - set(candidate.tablets)
        for tablet_id in removed:
            self._close_tablet_sessions(tablet_id, close_reason="removed")
            self.async_write_tablet_state(tablet_id)
        for tablet_id in affected:
            if tablet_id in self.configuration.tablets:
                self.async_add_tablet_entities(tablet_id)
                try:
                    await self._send_latest_if_connected(tablet_id)
                except RuntimeError as err:
                    _LOGGER.warning(
                        "Could not send updated Njörun settings for tablet %s: %s",
                        tablet_id,
                        err,
                    )
                self.async_write_tablet_state(tablet_id)
        if operation == "revoke":
            self._close_tablet_sessions(args["tablet_id"], close_reason="revoked")
            self.async_write_tablet_state(args["tablet_id"])
        return self.configuration.revision

    async def async_set_setting(self, tablet_id: str, key: str, value: Any) -> None:
        """Use a standard HA entity service to create a validated tablet override."""
        tablet = self.configuration.tablets.get(tablet_id)
        if tablet is None:
            raise RuntimeError("Unknown tablet")
        overrides = dict(tablet["overrides"])
        overrides[key] = value
        await self.async_configure(
            {"operation": "set_overrides", "tablet_id": tablet_id, "overrides": overrides},
            expected_revision=self.configuration.revision,
        )

    async def async_unload(self) -> None:
        """Close per-entry subscriptions and ephemeral state after unload."""
        self._closed = True
        for session_id in tuple(self.sessions):
            self._close_session(session_id, close_reason="unloaded")
        self.pending_operations.clear()
        self.entity_adders.clear()
        self._registered_platforms.clear()
        self._entities_created.clear()

    async def async_remove(self) -> None:
        """Revoke trust and remove the integration's persistent store."""
        await self.async_unload()
        await self.store.async_remove()

    async def _send_latest_if_connected(self, tablet_id: str) -> None:
        session = self._active_session_for_tablet(tablet_id)
        if session is not None:
            await self._send_configuration(session)

    async def _send_configuration(self, session: Session) -> None:
        """Send an expiring desired snapshot; reconnect creates a new operation ID."""
        if session.noise is None or session.subscription_id is None:
            raise RuntimeError("Management session is unavailable")
        if session.phase != "active":
            raise RuntimeError("Management session is not confirmed")
        snapshot = self.configuration.snapshot(session.tablet_id)
        operation_id = uuid.uuid4().hex
        expires_at = int(time.time() + OPERATION_TTL)
        payload = {
            "type": "configuration",
            "operation_id": operation_id,
            "revision": snapshot.revision,
            "settings": snapshot.settings,
            "expires_at": expires_at,
        }
        encoded = json.dumps(payload, separators=(",", ":"), ensure_ascii=False).encode()
        if len(encoded) > MAX_PLAINTEXT_BYTES:
            raise RuntimeError("Configuration exceeds the transport limit")
        frame = session.noise.encrypt(encoded)
        self.pending_operations[operation_id] = PendingOperation(
            operation_id=operation_id,
            session_id=session.session_id,
            binding_id=session.binding_id,
            tablet_id=session.tablet_id,
            revision=snapshot.revision,
            settings=snapshot.settings,
            expires_at=float(expires_at),
        )
        try:
            session.connection.send_event(
                session.subscription_id,
                {"frame": base64.b64encode(frame).decode("ascii")},
            )
        except Exception as err:
            self.pending_operations.pop(operation_id, None)
            self._close_session(session.session_id)
            raise RuntimeError("Could not send tablet configuration") from err

    def _active_session_for_tablet(self, tablet_id: str) -> Session | None:
        session_id = self.session_by_tablet.get(tablet_id)
        session = self.sessions.get(session_id or "")
        if session and session.phase == "active" and session.expires_at > time.time():
            return session
        return None

    def _require_session(
        self, connection: Any, session_id: str, expected_phase: str
    ) -> Session:
        self._expire_sessions()
        session = self.sessions.get(session_id)
        if (
            session is None
            or session.connection is not connection
            or session.phase != expected_phase
            or session.expires_at <= time.time()
        ):
            self._close_session(session_id)
            raise RuntimeError("Management session is missing, expired or in the wrong phase")
        binding = self.bindings.get(session.binding_id)
        if binding is None or binding["tablet_id"] != session.tablet_id or binding["status"] not in {"pending", "active"}:
            self._close_session(session_id)
            raise RuntimeError("Tablet binding is no longer active")
        return session

    def _expire_sessions(self) -> None:
        now = time.time()
        for session_id, session in tuple(self.sessions.items()):
            if session.expires_at <= now:
                self._close_session(session_id)
        for operation_id, operation in tuple(self.pending_operations.items()):
            if operation.expires_at <= now:
                self.pending_operations.pop(operation_id, None)

    def _expire_one_session(self, session_id: str) -> None:
        """Make availability false at the exact session deadline."""
        session = self.sessions.get(session_id)
        if session is not None and session.expires_at <= time.time():
            self._close_session(session_id, close_reason="expired")

    def _close_tablet_sessions(
        self, tablet_id: str, *, close_reason: str | None = None
    ) -> None:
        for session_id, session in tuple(self.sessions.items()):
            if session.tablet_id == tablet_id:
                self._close_session(session_id, close_reason=close_reason)

    def _close_session(
        self,
        session_id: str,
        *,
        detach_subscription: bool = True,
        close_reason: str | None = None,
    ) -> None:
        session = self.sessions.pop(session_id, None)
        if session is None:
            return
        if (
            close_reason is not None
            and session.phase == "active"
            and session.noise is not None
            and session.subscription_id is not None
        ):
            self._send_close_event(session, close_reason)
        if session.expiry_handle is not None:
            session.expiry_handle.cancel()
            session.expiry_handle = None
        session.phase = "closed"
        if self.session_by_tablet.get(session.tablet_id) == session_id:
            self.session_by_tablet.pop(session.tablet_id, None)
        for operation_id, operation in tuple(self.pending_operations.items()):
            if operation.session_id == session_id:
                self.pending_operations.pop(operation_id, None)
        if detach_subscription and session.subscription_id is not None:
            unsubscribe = session.connection.subscriptions.pop(
                session.subscription_id, None
            )
            if unsubscribe is not None:
                try:
                    unsubscribe()
                except Exception:
                    pass
        self.async_write_tablet_state(session.tablet_id)

    def _send_close_event(self, session: Session, reason: str) -> None:
        """Tell the authenticated peer why its management channel is ending."""
        if session.noise is None or session.subscription_id is None:
            return
        payload = json.dumps(
            {
                "type": "closed",
                "protocol": PROTOCOL_VERSION,
                "session_id": session.session_id,
                "reason": reason,
            },
            separators=(",", ":"),
        ).encode()
        try:
            frame = session.noise.encrypt(payload)
            session.connection.send_event(
                session.subscription_id,
                {"frame": base64.b64encode(frame).decode("ascii")},
            )
        except Exception:
            _LOGGER.debug(
                "Could not notify tablet %s that its session closed",
                session.tablet_id,
            )


def _generate_static_keypair() -> tuple[bytes, bytes]:
    private = X25519PrivateKey.generate()
    private_raw = private.private_bytes(
        serialization.Encoding.Raw,
        serialization.PrivateFormat.Raw,
        serialization.NoEncryption(),
    )
    public_raw = private.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    return private_raw, public_raw


def _decode_key(value: Any) -> bytes:
    if not isinstance(value, str) or len(value) != _B64_KEY_LEN:
        raise RuntimeError("Invalid Noise static key")
    try:
        decoded = base64.b64decode(value, validate=True)
    except Exception as err:
        raise RuntimeError("Invalid Noise static key") from err
    if len(decoded) != 32:
        raise RuntimeError("Invalid Noise static key")
    return decoded


def _decode_frame(value: Any, *, maximum: int) -> bytes:
    if not isinstance(value, str) or len(value) > ((maximum + 2) // 3) * 4:
        raise RuntimeError("Transport frame exceeds its limit")
    try:
        decoded = base64.b64decode(value, validate=True)
    except Exception as err:
        raise RuntimeError("Invalid transport frame") from err
    if len(decoded) > maximum:
        raise RuntimeError("Transport frame exceeds its limit")
    return decoded


def _safe_timestamp(value: Any) -> float:
    if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
        raise RuntimeError("Stored Njörun timestamp is invalid")
    return float(value)


def _effective_settings(configuration: Configuration, tablet_id: str) -> dict[str, Any]:
    """Compute settings from an arbitrary configuration candidate."""
    tablet = configuration.tablets[tablet_id]
    profile = configuration.profiles[tablet["profile_id"]]
    return validate_settings(profile["settings"] | tablet["overrides"])
