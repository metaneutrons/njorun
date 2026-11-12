# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Persistent configuration, per-binding Noise sessions and applied state."""

from __future__ import annotations

import asyncio
import base64
import hashlib
from collections.abc import Callable
from dataclasses import dataclass, field
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
    ANNOUNCE_TTL,
    COMPARE_DOMAIN,
    DOMAIN,
    MAX_ANNOUNCEMENTS,
    MAX_HANDSHAKES_PER_TABLET,
    MAX_HANDSHAKE_REQUEST_BYTES,
    MAX_HANDSHAKE_RESPONSE_BYTES,
    MAX_PLAINTEXT_BYTES,
    CONFIGURATION_REFUSALS,
    NETWORK_RESULT_WINDOW,
    NOISE_NAME,
    NOISE_PROLOGUE_DOMAIN,
    OPERATION_TTL,
    PROTOCOL_VERSION,
    SESSION_ACTIVE_TTL,
    SESSION_HANDSHAKE_TTL,
    STORE_VERSION,
)
from .grant import seal_grant
from .install import (
    INSTALL_TTL,
    InstallError,
    InstallRecord,
    parse_install_outcome,
    parse_install_progress,
    parse_install_result,
    valid_install,
)
from .install import RESULT_TIMEOUT as INSTALL_RESULT_TIMEOUT
from .live import (
    COMMAND_TTL,
    MAX_PENDING_COMMANDS,
    RESULT_TIMEOUT,
    parse_command_result,
    parse_telemetry,
    valid_backlight,
)
from .network import NetworkError, build_network_message, parse_network_result
from .network_store import UNSET, NetworkSettings
from .model import (
    CHOICES,
    DEFAULTS,
    LIVE_PARTS,
    Configuration,
    ConfigurationError,
    identifier,
    name as display_name,
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


class AnnounceLimitError(RuntimeError):
    """Too many tablets are waiting for approval."""

    announce_code = "too_many_waiting"


class AnnounceUnloadingError(RuntimeError):
    """The integration is unloading and takes no announcements."""

    announce_code = "unloading"


# Plan 004: the access Home Assistant creates for a tablet at approval. A normal user (a system
# user cannot carry a client ID), limited to the local network, in the ordinary users group.
GROUP_ID_USER = "system-users"
GRANT_CLIENT_ID = "https://github.com/metaneutrons/njorun"


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
    # Live parts (decision D6) this session negotiated; they end with the session.
    offered: tuple[str, ...] = ()
    pending_commands: dict[str, tuple[str, Any, asyncio.Future[None]]] = field(
        default_factory=dict
    )
    telemetry: dict[str, Any] | None = None
    # Install protocol (decision D7): negotiated on subscribe, and the install waiting for its
    # `install_result` (only one per session).
    ota: bool = False
    pending_install: tuple[str, asyncio.Future[None]] | None = None
    # Network messages (plan 006): negotiated on subscribe.
    network: bool = False


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

    on_announcement: Callable[[str], None] | None = None
    # Wi-Fi credentials, IP setups, revisions and results (plan 006); set in `__init__`.
    network_settings: NetworkSettings | None = None

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
        self.network_settings = NetworkSettings()
        self.bindings: dict[str, dict[str, Any]] = {}
        self.applied: dict[str, dict[str, Any]] = {}
        self.sessions: dict[str, Session] = {}
        self.session_by_tablet: dict[str, str] = {}
        self.pending_operations: dict[str, PendingOperation] = {}
        # The latest install per tablet (decision D7); in memory only, across sessions.
        self.installs: dict[str, InstallRecord] = {}
        # The firmware manager (decision D7), set by the integration's setup.
        self.firmware: Any = None
        self.completed_operations: set[str] = set()
        # Tablets asking to be paired, by tablet ID; memory only, the tablet re-announces.
        self.announcements: dict[str, dict[str, Any]] = {}
        # Called with the tablet ID when a tablet starts waiting; the integration uses it to
        # offer a Discovered card. Not called again while the tablet keeps announcing.
        self.on_announcement: Callable[[str], None] | None = None
        self.entity_adders: dict[str, Callable[..., Any]] = {}
        self.entities: dict[str, set[Any]] = {}
        self._registered_platforms: set[str] = set()
        # (platform, tablet, unique ID): capabilities can add entities to a platform that
        # already has some of the tablet's entities (decision D8).
        self._entities_created: set[tuple[str, str, str]] = set()
        # Tablets with an open repair issue about refused capabilities.
        self._capability_issues: set[str] = set()
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
        base_keys = {"version", "configuration", "bindings", "applied"}
        if (
            not isinstance(stored, dict)
            or not base_keys <= set(stored) <= base_keys | {"network"}
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
            stored_settings = record.get("settings")
            if isinstance(stored_settings, dict):
                # An applied record from before the interface size existed has no `ui_scale`.
                stored_settings = {"ui_scale": DEFAULTS["ui_scale"]} | stored_settings
            settings = validate_settings(stored_settings)
            validate_effective(settings, configuration.tablets[tablet_id]["capabilities"])
            revision = record.get("revision")
            if type(revision) is not int or revision < 0:
                raise RuntimeError("Stored Njörun applied revision is invalid")
            applied[tablet_id] = {
                "revision": revision,
                "settings": settings,
                "applied_at": _safe_timestamp(record.get("applied_at")),
            }
        try:
            network = NetworkSettings.restore(stored["network"]) if "network" in stored else NetworkSettings()
        except NetworkError as err:
            raise RuntimeError("Stored Njörun network settings are invalid") from err
        self.configuration = configuration
        self.bindings = bindings
        self.applied = applied
        self.network_settings = network

    def _stored_data(
        self,
        *,
        configuration: Configuration | None = None,
        bindings: dict[str, dict[str, Any]] | None = None,
        applied: dict[str, dict[str, Any]] | None = None,
        network: NetworkSettings | None = None,
    ) -> dict[str, Any]:
        data = {
            "version": 1,
            "configuration": (configuration or self.configuration).dump(),
            "bindings": bindings if bindings is not None else self.bindings,
            "applied": applied if applied is not None else self.applied,
        }
        network = network if network is not None else self.network_settings
        if network is not None and not network.is_empty():
            data["network"] = network.dump()
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
        network: NetworkSettings | None = None,
    ) -> None:
        """Atomically store a complete candidate before changing observable state."""
        await self.store.async_save(
            self._stored_data(
                configuration=configuration,
                bindings=bindings,
                applied=applied,
                network=network,
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
        from . import button as button_platform
        from . import light as light_platform
        from . import number as number_platform
        from . import select as select_platform
        from . import sensor as sensor_platform
        from . import update as update_platform

        if platform == "select":
            entities = select_platform.create_entities(self, tablet_id)
        elif platform == "number":
            entities = number_platform.create_entities(self, tablet_id)
        elif platform == "button":
            entities = button_platform.create_entities(self, tablet_id)
        elif platform == "light":
            entities = light_platform.create_entities(self, tablet_id)
        elif platform == "sensor":
            entities = sensor_platform.create_entities(self, tablet_id)
        elif platform == "update":
            entities = update_platform.create_entities(self, tablet_id)
        else:
            return
        fresh = [
            entity
            for entity in entities
            if (platform, tablet_id, entity.unique_id) not in self._entities_created
        ]
        if fresh:
            self._entities_created.update(
                (platform, tablet_id, entity.unique_id) for entity in fresh
            )
            adder(fresh)

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
        # A removed entity (device deleted, or capability gone) must be creatable again.
        self._entities_created = {
            key
            for key in self._entities_created
            if key[1:] != (tablet_id, getattr(entity, "unique_id", None))
        }

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
        profile_id: str | None,
        tablet_public_key: str,
        capabilities: Any,
        expected_revision: int,
        access: dict[str, str] | None = None,
        new_profile_name: str | None = None,
        theme: str | None = None,
    ) -> dict[str, Any]:
        """Create an admin-approved pending binding without claiming device proof.

        `access` is the user and refresh token Home Assistant created for the tablet (plan 004);
        `new_profile_name` creates a profile for this tablet in the same atomic step.
        """
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
                revision = expected_revision
                if new_profile_name is not None:
                    profile_id = uuid.uuid4().hex
                    candidate.add_profile(profile_id, new_profile_name, {}, revision)
                    revision = candidate.revision
                if profile_id is None:
                    raise RuntimeError("Choose a profile for the tablet")
                candidate.add_tablet(
                    tablet_id,
                    name,
                    profile_id,
                    valid_capabilities,
                    revision,
                )
                # The theme the user chose on the tablet is its starting value (plan 006 N1).
                if theme is not None and theme != candidate._effective(
                    candidate.tablets[tablet_id]
                )["theme"]:
                    candidate.set_overrides(
                        tablet_id, {"theme": theme}, candidate.revision
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
                **(access or {}),
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

    def async_announce(
        self,
        connection: Any,
        *,
        tablet_id: Any,
        name: Any,
        tablet_public_key: Any,
        capabilities: Any,
    ) -> dict[str, Any]:
        """Record a tablet that asks to be paired over a signed-in WebSocket connection.

        Any signed-in user may announce; nothing is trusted until an administrator compares
        the code shown on the tablet with the one shown in Home Assistant and approves it.
        Knowing a binding ID gives no access: the session still needs the tablet's key.
        """
        if getattr(connection, "user", None) is None:
            raise RuntimeError("Authenticated Home Assistant session required")
        return self._announce(
            tablet_id=tablet_id,
            name=name,
            tablet_public_key=tablet_public_key,
            capabilities=capabilities,
        )

    async def async_announce_public(
        self,
        *,
        tablet_id: Any,
        name: Any,
        tablet_public_key: Any,
        capabilities: Any,
        need_grant: Any = False,
        theme: Any = None,
    ) -> dict[str, Any]:
        """The announcement endpoint without sign-in (plan 004).

        Same results as the WebSocket command. An approved tablet whose access Home Assistant
        created also gets the grant, sealed to its key, until its first confirmed session and
        whenever it asks for one (`need_grant`). A tablet without valid access gets new access
        only then; a working token is never revoked because of an announcement.
        """
        if type(need_grant) is not bool:
            raise RuntimeError("need_grant must be a boolean")
        result = self._announce(
            tablet_id=tablet_id,
            name=name,
            tablet_public_key=tablet_public_key,
            capabilities=capabilities,
            theme=theme,
        )
        if result.get("status") != "approved":
            return result
        binding = self.bindings.get(result["binding_id"])
        if binding is None or "user_id" not in binding:
            return result
        if binding["status"] != "pending" and not need_grant:
            return result
        grant = await self._async_grant_for(result["binding_id"], need_grant=need_grant)
        if grant is not None:
            result = dict(result, grant=grant)
        return result

    def _announce(
        self,
        *,
        tablet_id: Any,
        name: Any,
        tablet_public_key: Any,
        capabilities: Any,
        theme: Any = None,
    ) -> dict[str, Any]:
        if self._closed:
            raise AnnounceUnloadingError("Njörun is unloading")
        try:
            tablet_id = identifier(tablet_id)
            label = display_name(name)
            valid_capabilities = validate_capabilities(capabilities)
        except ConfigurationError as err:
            raise RuntimeError(str(err)) from err
        if theme is not None and (type(theme) is not str or theme not in CHOICES["theme"]):
            raise RuntimeError("theme must be one of the known themes")
        tablet_public = _decode_key(tablet_public_key)
        encoded = base64.b64encode(tablet_public).decode("ascii")
        for binding_id, binding in self.bindings.items():
            if binding["tablet_id"] != tablet_id or binding["status"] not in {"pending", "active"}:
                continue
            if binding["tablet_public_key"] != encoded:
                # Another key is paired under this ID: replace or remove that tablet first.
                return {"status": "conflict"}
            return {
                "status": "approved",
                "binding_id": binding_id,
                "ha_public_key": _public_key_of(binding["ha_private_key"]),
            }
        now = time.time()
        self._expire_announcements(now)
        if tablet_id not in self.announcements and len(self.announcements) >= MAX_ANNOUNCEMENTS:
            raise AnnounceLimitError("Too many tablets are waiting for approval")
        code = comparison_code(tablet_public)
        newly_waiting = tablet_id not in self.announcements
        self.announcements[tablet_id] = {
            "tablet_id": tablet_id,
            "name": label,
            "tablet_public_key": encoded,
            "capabilities": valid_capabilities,
            "theme": theme,
            "code": code,
            "expires_at": now + ANNOUNCE_TTL,
        }
        if newly_waiting and self.on_announcement is not None:
            self.on_announcement(tablet_id)
        return {"status": "pending", "code": code}

    async def _async_grant_for(self, binding_id: str, *, need_grant: bool) -> str | None:
        """The sealed access of a binding, creating new access only when none is valid."""
        async with self._lock:
            binding = self.bindings.get(binding_id)
            if binding is None or binding["status"] not in {"pending", "active"}:
                return None
            token = self._valid_refresh_token(binding)
            if token is None:
                if binding["status"] != "pending" and not need_grant:
                    return None
                access = await self._async_create_access(
                    self.configuration.tablets[binding["tablet_id"]]["name"]
                )
                candidate = {key: dict(value) for key, value in self.bindings.items()}
                candidate[binding_id].update(access)
                await self._async_save(bindings=candidate)
                self.bindings = candidate
                token = self._valid_refresh_token(self.bindings[binding_id])
                if token is None:
                    return None
            return seal_grant(
                base64.b64decode(binding["tablet_public_key"]),
                binding_id,
                token.token,
                GRANT_CLIENT_ID,
            )

    def _valid_refresh_token(self, binding: dict[str, Any]) -> Any:
        """The binding's refresh token while it and its user still exist."""
        token_id = binding.get("refresh_token_id")
        if not isinstance(token_id, str):
            return None
        return self.hass.auth.async_get_refresh_token(token_id)

    async def _async_create_access(self, label: str) -> dict[str, str]:
        """A user for the tablet and one refresh token for it (plan 004, decision P3)."""
        user = await self.hass.auth.async_create_user(
            f"Tablet {label}", group_ids=[GROUP_ID_USER], local_only=True
        )
        try:
            token = await self.hass.auth.async_create_refresh_token(
                user, client_id=GRANT_CLIENT_ID, client_name=f"Tablet {label}"
            )
        except Exception:
            await self.hass.auth.async_remove_user(user)
            raise
        return {"user_id": user.id, "refresh_token_id": token.id}

    async def _async_remove_access(self, user_ids: list[str]) -> None:
        """Remove the users created for tablets; their refresh tokens go with them."""
        for user_id in user_ids:
            user = await self.hass.auth.async_get_user(user_id)
            if user is None:
                continue
            try:
                await self.hass.auth.async_remove_user(user)
            except ValueError as err:
                _LOGGER.warning("Could not remove the user of a removed tablet: %s", err)

    def announcement(self, tablet_id: str) -> dict[str, Any] | None:
        """A waiting tablet's name and code, or None once it stopped waiting."""
        self._expire_announcements(time.time())
        item = self.announcements.get(tablet_id)
        if item is None:
            return None
        return {
            "tablet_id": tablet_id,
            "name": item["name"],
            "code": item["code"],
            "theme": item.get("theme"),
        }

    def announcement_expires_at(self, tablet_id: str) -> float | None:
        """When a waiting tablet is forgotten unless it announces again."""
        item = self.announcements.get(tablet_id)
        return None if item is None else item["expires_at"]

    def pending_announcements(self) -> list[dict[str, Any]]:
        """Tablets waiting for approval, without their keys, oldest name first."""
        self._expire_announcements(time.time())
        return sorted(
            (
                {"tablet_id": item["tablet_id"], "name": item["name"], "code": item["code"]}
                for item in self.announcements.values()
            ),
            key=lambda item: (item["name"], item["tablet_id"]),
        )

    async def async_approve(
        self,
        *,
        tablet_id: str,
        profile_id: str | None,
        expected_revision: int,
        new_profile: bool = False,
        name: str | None = None,
        theme: str | None = None,
    ) -> dict[str, Any]:
        """Enroll an announced tablet after the administrator compared its code.

        Home Assistant creates the tablet's own user and refresh token here; the tablet
        receives them in the grant of its next announcement. `new_profile` gives the tablet a
        profile of its own, named like the tablet, instead of a shared one. `name` is what the
        administrator chose for the tablet; it gives the device and the start of its entity IDs,
        the user, and the new profile. Without it the announced name is used.
        """
        self._expire_announcements(time.time())
        announcement = self.announcements.get(tablet_id)
        if announcement is None:
            raise RuntimeError("The tablet is no longer waiting; check that it is on and online")
        try:
            label = announcement["name"] if name is None else display_name(name.strip())
        except ConfigurationError as err:
            raise RuntimeError(str(err)) from err
        access = await self._async_create_access(label)
        try:
            result = await self.async_enroll(
                tablet_id=tablet_id,
                name=label,
                profile_id=None if new_profile else profile_id,
                tablet_public_key=announcement["tablet_public_key"],
                capabilities=announcement["capabilities"],
                expected_revision=expected_revision,
                access=access,
                new_profile_name=label if new_profile else None,
                theme=theme,
            )
        except BaseException:
            await self._async_remove_access([access["user_id"]])
            raise
        self.announcements.pop(tablet_id, None)
        return result

    def _expire_announcements(self, now: float) -> None:
        for tablet_id in [
            key for key, item in self.announcements.items() if item["expires_at"] <= now
        ]:
            del self.announcements[tablet_id]

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
        # A restarted tablet must be able to reconnect while HA still holds its old session
        # (a hard reset leaves the old socket open until it times out). Starting a session
        # grants nothing: the old session is replaced only after the new one proves the
        # tablet's key. Handshakes in flight are bounded so this cannot pile up.
        in_flight = sorted(
            (s for s in self.sessions.values()
             if s.tablet_id == tablet_id and s.phase != "active"),
            key=lambda item: item.expires_at,
        )
        for stale in in_flight[: max(0, len(in_flight) - MAX_HANDSHAKES_PER_TABLET + 1)]:
            self._close_session(stale.session_id)
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
        # The capabilities of decision D8 are optional: older firmware omits them.
        reported: dict[str, Any] | None = None
        if isinstance(payload, dict) and "capabilities" in payload:
            payload = dict(payload)
            try:
                reported = validate_capabilities(payload.pop("capabilities"))
            except ConfigurationError as err:
                self._close_session(session_id)
                raise RuntimeError("Tablet confirmation carries invalid capabilities") from err
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
            if reported is not None:
                await self._async_refresh_capabilities(session.tablet_id, reported)
        # The tablet proved its key: this session replaces any older one for the tablet.
        for other_id, other in tuple(self.sessions.items()):
            if other.tablet_id == session.tablet_id and other_id != session_id:
                self._close_session(other_id)
        self.session_by_tablet[session.tablet_id] = session_id
        session.phase = "active"
        session.expires_at = time.time() + SESSION_ACTIVE_TTL
        session.subscription_id = subscription_id
        reported = self.configuration.tablets.get(session.tablet_id, {}).get(
            "capabilities", {}
        )
        session.offered = tuple(
            part for part in LIVE_PARTS if part in reported.get("live", ())
        )
        session.ota = "ota" in reported
        session.network = "network" in reported
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

    async def _async_refresh_capabilities(
        self, tablet_id: str, reported: dict[str, Any]
    ) -> None:
        """Store capabilities a confirmed tablet reports (decision D8); the lock is held.

        They replace the stored ones only if every setting of the tablet stays valid.
        Otherwise the old capabilities and the session stay and a repair issue says so.
        """
        tablet = self.configuration.tablets.get(tablet_id)
        if tablet is None or tablet["capabilities"] == reported:
            if tablet is not None:
                self._clear_capability_issue(tablet_id)
            return
        candidate = Configuration.restore(self.configuration.dump())
        try:
            candidate.set_capabilities(tablet_id, reported, candidate.revision)
        except ConfigurationError as err:
            _LOGGER.warning(
                "Tablet %s reports capabilities that conflict with its settings (%s); "
                "keeping the stored capabilities. Change the setting to accept them.",
                tablet_id,
                err,
            )
            self._raise_capability_issue(tablet_id, str(err))
            return
        await self._async_save(configuration=candidate)
        self.configuration = candidate
        self._clear_capability_issue(tablet_id)
        _LOGGER.info("Tablet %s reported new capabilities; stored them", tablet_id)
        self._remove_unsupported_entities(tablet_id)
        self.async_add_tablet_entities(tablet_id)
        self.async_write_tablet_state(tablet_id)

    def _remove_unsupported_entities(self, tablet_id: str) -> None:
        """Remove the entities whose capability the tablet no longer reports."""
        capabilities = self.configuration.tablets[tablet_id]["capabilities"]
        gone = [
            entity
            for entity in tuple(self.entities.get(tablet_id, ()))
            if not entity.supported_by(capabilities)
        ]
        if not gone:
            return
        from homeassistant.helpers import entity_registry as er

        registry = er.async_get(self.hass)
        for entity in gone:
            if entity.entity_id and registry.async_get(entity.entity_id):
                registry.async_remove(entity.entity_id)
            else:
                self.hass.async_create_task(entity.async_remove())

    def _raise_capability_issue(self, tablet_id: str, reason: str) -> None:
        from homeassistant.helpers import issue_registry as ir

        self._capability_issues.add(tablet_id)
        ir.async_create_issue(
            self.hass,
            DOMAIN,
            f"capabilities_{tablet_id}",
            is_fixable=False,
            severity=ir.IssueSeverity.WARNING,
            translation_key="capabilities_conflict",
            translation_placeholders={
                "tablet": self.configuration.tablets[tablet_id]["name"],
                "reason": reason,
            },
        )

    def _clear_capability_issue(self, tablet_id: str) -> None:
        if tablet_id not in self._capability_issues:
            return
        from homeassistant.helpers import issue_registry as ir

        self._capability_issues.discard(tablet_id)
        ir.async_delete_issue(self.hass, DOMAIN, f"capabilities_{tablet_id}")

    async def async_send_initial_configuration(self, session: Session) -> None:
        """Send a fresh snapshot after the HA command's subscription result."""
        await self._send_configuration(session)
        await self._send_network(session)

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
        if isinstance(payload, dict) and payload.get("type") in {"command_result", "telemetry"}:
            self._receive_live(session, payload)
            return
        if isinstance(payload, dict) and payload.get("type") in {
            "install_result",
            "install_progress",
            "install_outcome",
        }:
            self._receive_install(session, payload)
            return
        if isinstance(payload, dict) and payload.get("type") == "network_result":
            await self._receive_network_result(session, payload)
            return
        if isinstance(payload, dict) and payload.get("type") == "rejected":
            await self._receive_rejection(connection, session, payload)
            return
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
        self._clear_configuration_issue(session.tablet_id)
        self.async_write_tablet_state(session.tablet_id)

    async def _receive_rejection(
        self, connection: Any, session: Session, payload: dict[str, Any]
    ) -> None:
        """Handle an authenticated `rejected`: the tablet did not apply a configuration.

        The operation must be the pending one of this session and tablet, as for an `applied`.
        The reason is shown as a repair issue, which the next `applied` of the tablet clears.
        Nothing about the applied state changes.
        """
        operation_id = payload.get("operation_id")
        revision = payload.get("revision")
        reason = payload.get("reason")
        if (
            set(payload) != {"type", "operation_id", "revision", "reason"}
            or not isinstance(operation_id, str)
            or not 0 < len(operation_id.encode()) <= 128
            or type(revision) is not int
            or reason not in CONFIGURATION_REFUSALS
        ):
            self._close_session(session.session_id)
            raise RuntimeError("Invalid tablet refusal")
        async with self._lock:
            operation = self.pending_operations.get(operation_id)
            if (
                operation is None
                or operation.expires_at <= time.time()
                or operation.session_id != session.session_id
                or operation.binding_id != session.binding_id
                or operation.tablet_id != session.tablet_id
                or operation.revision != revision
                or self.sessions.get(session.session_id) is not session
                or session.connection is not connection
                or session.tablet_id not in self.configuration.tablets
            ):
                raise RuntimeError("No matching configuration operation is pending")
            self.pending_operations.pop(operation_id, None)
            self.completed_operations.add(operation_id)
        _LOGGER.warning(
            "Tablet %s refused configuration revision %s: %s", session.tablet_id, revision, reason
        )
        self._raise_configuration_issue(session.tablet_id, reason)
        self.async_write_tablet_state(session.tablet_id)

    @property
    def _configuration_issues(self) -> set[str]:
        issues = self.__dict__.get("_configuration_issues_data")
        if issues is None:
            issues = self.__dict__["_configuration_issues_data"] = set()
        return issues

    def _raise_configuration_issue(self, tablet_id: str, reason: str) -> None:
        from homeassistant.helpers import issue_registry as ir

        self._configuration_issues.add(tablet_id)
        ir.async_create_issue(
            self.hass,
            DOMAIN,
            f"configuration_{tablet_id}",
            is_fixable=False,
            severity=ir.IssueSeverity.WARNING,
            translation_key=f"configuration_{reason}",
            translation_placeholders={"tablet": self.configuration.tablets[tablet_id]["name"]},
        )

    def _clear_configuration_issue(self, tablet_id: str) -> None:
        if tablet_id not in self._configuration_issues:
            return
        from homeassistant.helpers import issue_registry as ir

        self._configuration_issues.discard(tablet_id)
        ir.async_delete_issue(self.hass, DOMAIN, f"configuration_{tablet_id}")

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
            "set_log_sink": {"url"},
        }
        required_fields = {
            "add_profile": {"profile_id", "name"},
            "edit_profile": {"profile_id", "patch"},
            "delete_profile": {"profile_id"},
            "set_overrides": {"tablet_id", "overrides"},
            "assign": {"tablet_id", "profile_id"},
            "remove_tablet": {"tablet_id"},
            "revoke": {"tablet_id"},
            "set_log_sink": {"url"},
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
            candidate_network = (
                self.network_settings.copy() if self.network_settings is not None else None
            )
            removed_users: list[str] = []
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
                    if candidate_network is not None:
                        candidate_network.forget_profile(args["profile_id"])
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
                elif operation == "set_log_sink":
                    candidate.set_log_sink(args["url"], expected_revision)
                elif operation == "remove_tablet":
                    candidate.remove_tablet(args["tablet_id"], expected_revision)
                    candidate_applied.pop(args["tablet_id"], None)
                    if candidate_network is not None:
                        candidate_network.forget_tablet(args["tablet_id"])
                    for binding_id, binding in list(candidate_bindings.items()):
                        if binding["tablet_id"] == args["tablet_id"]:
                            if "user_id" in binding:
                                removed_users.append(binding["user_id"])
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
                            binding.pop("refresh_token_id", None)
                            if "user_id" in binding:
                                removed_users.append(binding.pop("user_id"))
                            found = True
                    if not found:
                        raise RuntimeError("Tablet has no active binding")
            except ConfigurationError as err:
                raise RuntimeError(str(err)) from err

            await self._async_save(
                configuration=candidate,
                bindings=candidate_bindings,
                applied=candidate_applied,
                network=candidate_network,
            )
            self.configuration = candidate
            self.bindings = candidate_bindings
            self.applied = candidate_applied
            if candidate_network is not None:
                self.network_settings = candidate_network
            affected = {
                tablet_id for tablet_id in candidate.tablets
                if before_tablet_revisions.get(tablet_id)
                != candidate.tablets[tablet_id]["revision"]
            }
            removed = before_tablets - set(candidate.tablets)
        await self._async_remove_access(removed_users)
        for tablet_id in removed:
            self._close_tablet_sessions(tablet_id, close_reason="removed")
            self.installs.pop(tablet_id, None)
            self._network_flights.pop(tablet_id, None)
            self._clear_network_issue(tablet_id)
            self.async_write_tablet_state(tablet_id)
        if operation in {"add_profile", "delete_profile"}:
            # Every tablet's Profile select lists the profiles.
            for tablet_id in self.configuration.tablets:
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
            self.installs.pop(args["tablet_id"], None)
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

    async def async_use_profile_values(self, tablet_id: str) -> None:
        """Clear all per-tablet overrides so the tablet follows its profile again."""
        tablet = self.configuration.tablets.get(tablet_id)
        if tablet is None:
            raise RuntimeError("Unknown tablet")
        if not tablet["overrides"]:
            return
        await self.async_configure(
            {"operation": "set_overrides", "tablet_id": tablet_id, "overrides": {}},
            expected_revision=self.configuration.revision,
        )

    async def async_set_profile(self, tablet_id: str, profile_id: str) -> None:
        """Move a tablet to another profile; its own overrides are kept."""
        tablet = self.configuration.tablets.get(tablet_id)
        if tablet is None:
            raise RuntimeError("Unknown tablet")
        if tablet["profile_id"] == profile_id:
            return
        await self.async_configure(
            {"operation": "assign", "tablet_id": tablet_id, "profile_id": profile_id},
            expected_revision=self.configuration.revision,
        )

    async def async_set_log_sink(self, url: str) -> None:
        """Set the installation-wide remote logging sink; empty switches it off."""
        await self.async_configure(
            {"operation": "set_log_sink", "url": url},
            expected_revision=self.configuration.revision,
        )

    async def async_remove_tablet(self, tablet_id: str) -> None:
        """Remove a tablet, its trust and its applied state (device deleted in HA)."""
        await self.async_configure(
            {"operation": "remove_tablet", "tablet_id": tablet_id},
            expected_revision=self.configuration.revision,
        )

    async def async_unload(self) -> None:
        """Close per-entry subscriptions and ephemeral state after unload."""
        self._closed = True
        for session_id in tuple(self.sessions):
            self._close_session(session_id, close_reason="unloaded")
        self.pending_operations.clear()
        self._network_flights.clear()
        self.entity_adders.clear()
        self._registered_platforms.clear()
        self._entities_created.clear()

    async def async_remove(self) -> None:
        """Revoke trust and remove the integration's persistent store."""
        await self._async_remove_access(
            [binding["user_id"] for binding in self.bindings.values() if "user_id" in binding]
        )
        await self.async_unload()
        await self.store.async_remove()

    async def _send_latest_if_connected(self, tablet_id: str) -> None:
        session = self._active_session_for_tablet(tablet_id)
        if session is not None:
            await self._send_configuration(session)
            await self._send_network(session)

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
            "log_sink": snapshot.log_sink,
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

    def live_part_offered(self, tablet_id: str, part: str) -> bool:
        """Whether the tablet's live session negotiated this part (decision D6)."""
        session = self._active_session_for_tablet(tablet_id)
        return session is not None and part in session.offered

    def telemetry(self, tablet_id: str) -> dict[str, Any] | None:
        """The tablet's latest telemetry, only while its session is live."""
        session = self._active_session_for_tablet(tablet_id)
        if session is None or "telemetry" not in session.offered:
            return None
        return session.telemetry

    def _receive_live(self, session: Session, payload: dict[str, Any]) -> None:
        """Handle an authenticated `command_result` or `telemetry`; anything else ends the session."""
        if payload["type"] == "telemetry":
            telemetry = parse_telemetry(payload)
            if telemetry is None or "telemetry" not in session.offered:
                self._close_session(session.session_id)
                raise RuntimeError("Invalid tablet telemetry")
            session.telemetry = telemetry
            self.async_write_tablet_state(session.tablet_id)
            return
        parsed = parse_command_result(payload)
        pending = session.pending_commands.pop(parsed[0], None) if parsed else None
        if parsed is None or pending is None:
            self._close_session(session.session_id)
            raise RuntimeError("Invalid or unexpected command result")
        name, value, future = pending
        command_id, reason = parsed
        if reason is None:
            if name == "backlight" and session.telemetry is not None:
                # The tablet carried the command out; telemetry confirms it a moment later.
                backlight = dict(session.telemetry["backlight"])
                backlight.update(value)
                session.telemetry = {**session.telemetry, "backlight": backlight}
                self.async_write_tablet_state(session.tablet_id)
            if not future.done():
                future.set_result(None)
        elif not future.done():
            future.set_exception(RuntimeError(f"The tablet refused the command: {reason}"))
        _LOGGER.debug("Command %s of tablet %s resolved: %s", command_id, session.tablet_id, reason)

    async def async_send_command(
        self, tablet_id: str, name: str, value: dict[str, Any] | None = None
    ) -> None:
        """Send one live command and wait for the tablet's result.

        Commands are never queued, stored or sent again: without a live session, or for a part the
        session did not negotiate, this fails at once. The command expires after ten seconds.
        """
        session = self._active_session_for_tablet(tablet_id)
        if session is None or session.noise is None or session.subscription_id is None:
            raise RuntimeError("The tablet is not connected")
        if name not in session.offered or name not in {"wake", "backlight"}:
            raise RuntimeError("The tablet does not support this command")
        if (name == "wake") != (value is None) or (
            name == "backlight" and not valid_backlight(value, partial=True)
        ):
            raise RuntimeError("Invalid command value")
        if len(session.pending_commands) >= MAX_PENDING_COMMANDS:
            raise RuntimeError("Too many commands are waiting for the tablet")
        command_id = uuid.uuid4().hex
        payload = {
            "type": "command",
            "command_id": command_id,
            "name": name,
            "value": value,
            "expires_at": int(time.time() + COMMAND_TTL),
        }
        frame = session.noise.encrypt(json.dumps(payload, separators=(",", ":")).encode())
        future: asyncio.Future[None] = self.hass.loop.create_future()
        session.pending_commands[command_id] = (name, value, future)
        try:
            session.connection.send_event(
                session.subscription_id, {"frame": base64.b64encode(frame).decode("ascii")}
            )
            async with asyncio.timeout(RESULT_TIMEOUT):
                await future
        except TimeoutError as err:
            raise RuntimeError("The tablet did not answer in time") from err
        except RuntimeError:
            raise
        except Exception as err:
            self._close_session(session.session_id)
            raise RuntimeError("Could not send the command") from err
        finally:
            session.pending_commands.pop(command_id, None)

    def ota_offered(self, tablet_id: str) -> bool:
        """Whether the tablet's live session negotiated the install protocol (decision D7)."""
        session = self._active_session_for_tablet(tablet_id)
        return session is not None and session.ota

    def tablet_board(self, tablet_id: str) -> str | None:
        """The board ID the tablet reported in its `ota` capability."""
        ota = self.configuration.tablets.get(tablet_id, {}).get("capabilities", {}).get("ota")
        return ota["board"] if ota else None

    def tablet_ota(self, tablet_id: str) -> dict[str, str] | None:
        """The tablet's `ota` capability: board and, when reported, revision and layout."""
        ota = self.configuration.tablets.get(tablet_id, {}).get("capabilities", {}).get("ota")
        return dict(ota) if ota else None

    def install_status(self, tablet_id: str) -> dict[str, Any] | None:
        """The latest install of the tablet, or None. A final state stays until the next install."""
        record = self.installs.get(tablet_id)
        if record is not None and record.state == "sent" and not record.is_open(time.time()):
            self.installs.pop(tablet_id, None)
            return None
        return record.snapshot() if record is not None else None

    def _receive_install(self, session: Session, payload: dict[str, Any]) -> None:
        """Handle an authenticated install message of the tablet.

        Shape errors and a message type the session did not negotiate end the session. Progress
        and outcome for an install HA does not know (HA restarted, the window passed, already
        final) are logged and dropped; the version in telemetry reconciles then.
        """
        kind = payload["type"]
        record = self.installs.get(session.tablet_id)
        now = time.time()
        try:
            if not session.ota:
                raise InstallError("Install messages without the ota capability")
            if kind == "install_result":
                parsed = parse_install_result(payload)
                pending = session.pending_install
                if (
                    parsed is None
                    or pending is None
                    or pending[0] != parsed[0]
                    or record is None
                    or record.install_id != parsed[0]
                ):
                    raise InstallError("Invalid or unexpected install result")
                record.receive_result(parsed[1])
                session.pending_install = None
                if not pending[1].done():
                    if parsed[1] is None:
                        pending[1].set_result(None)
                    else:
                        pending[1].set_exception(
                            RuntimeError(f"The tablet rejected the install: {parsed[1]}")
                        )
            elif kind == "install_progress":
                progress = parse_install_progress(payload)
                if progress is None:
                    raise InstallError("Invalid install progress")
                if record is None or record.install_id != progress[0]:
                    _LOGGER.debug("Progress of unknown install %s dropped", progress[0])
                    return
                if not record.receive_progress(
                    progress[1], progress[2], progress[3], now=now, monotonic=time.monotonic()
                ):
                    return
            else:
                outcome = parse_install_outcome(payload)
                if outcome is None:
                    raise InstallError("Invalid install outcome")
                if record is None or record.install_id != outcome[0]:
                    _LOGGER.warning("Outcome of unknown install %s ignored", outcome[0])
                    return
                if not record.receive_outcome(outcome[1], outcome[2], now=now):
                    _LOGGER.warning("Unexpected outcome of install %s ignored", outcome[0])
                    return
        except InstallError as err:
            self._close_session(session.session_id)
            raise RuntimeError(str(err)) from err
        self.async_write_tablet_state(session.tablet_id)

    async def async_send_install(
        self,
        tablet_id: str,
        *,
        version: str,
        channel: str,
        size: int,
        sha256: str,
        url: str,
        token: str,
        downgrade: bool = False,
    ) -> str:
        """Send one install for a cached image and wait for the tablet's answer.

        Returns the install ID once the tablet accepted. The download endpoint and its
        single-use token belong to the caller. An install is never queued, stored or sent again:
        without a live session that negotiated `ota`, or while another install of the tablet is
        open, this fails at once.
        """
        session = self._active_session_for_tablet(tablet_id)
        if session is None or session.noise is None or session.subscription_id is None:
            raise RuntimeError("The tablet is not connected")
        if not session.ota:
            raise RuntimeError("The tablet does not support installs")
        current = self.installs.get(tablet_id)
        if current is not None and current.is_open(time.time()):
            raise RuntimeError("An install of this tablet is already open")
        install_id = uuid.uuid4().hex
        expires_at = int(time.time() + INSTALL_TTL)
        payload = {
            "type": "install",
            "install_id": install_id,
            "version": version,
            "channel": channel,
            "size": size,
            "sha256": sha256,
            "url": url,
            "token": token,
            "expires_at": expires_at,
        }
        if downgrade:
            payload["downgrade"] = True
        if not valid_install(payload):
            raise RuntimeError("Invalid install parameters")
        frame = session.noise.encrypt(json.dumps(payload, separators=(",", ":")).encode())
        future: asyncio.Future[None] = self.hass.loop.create_future()
        record = InstallRecord(install_id, version, size, expires_at)
        self.installs[tablet_id] = record
        session.pending_install = (install_id, future)
        try:
            session.connection.send_event(
                session.subscription_id, {"frame": base64.b64encode(frame).decode("ascii")}
            )
            async with asyncio.timeout(INSTALL_RESULT_TIMEOUT):
                await future
        except TimeoutError as err:
            self._drop_install(tablet_id, record)
            raise RuntimeError("The tablet did not answer in time") from err
        except RuntimeError:
            self._drop_install(tablet_id, record, keep_rejected=True)
            raise
        except Exception as err:
            self._drop_install(tablet_id, record)
            self._close_session(session.session_id)
            raise RuntimeError("Could not send the install") from err
        finally:
            if session.pending_install is not None and session.pending_install[0] == install_id:
                session.pending_install = None
        self.async_write_tablet_state(tablet_id)
        return install_id

    def _drop_install(
        self, tablet_id: str, record: InstallRecord, *, keep_rejected: bool = False
    ) -> None:
        """Forget an install that never got accepted; a rejection stays visible as final."""
        if self.installs.get(tablet_id) is not record:
            return
        if not (keep_rejected and record.state == "rejected"):
            self.installs.pop(tablet_id, None)

    # Network settings (plan 006). Values contain a password: nothing here logs or shows them
    # without `network.redacted`, and they travel only inside the Noise session.

    @property
    def _network_flights(self) -> dict[str, tuple[str, int, float]]:
        """The network message each tablet has not answered yet: operation, revision, send time.

        Memory only. It outlives a session on purpose: a Wi-Fi trial breaks the session and the
        result arrives on the next one.
        """
        flights = self.__dict__.get("_network_flights_data")
        if flights is None:
            flights = self.__dict__["_network_flights_data"] = {}
        return flights

    def _network_interfaces(self, tablet_id: str) -> list[str]:
        tablet = self.configuration.tablets.get(tablet_id)
        return list(tablet["capabilities"].get("network", ())) if tablet else []

    def _network_desired(
        self, tablet_id: str, settings: NetworkSettings | None = None
    ) -> dict[str, Any]:
        settings = settings if settings is not None else self.network_settings
        tablet = self.configuration.tablets.get(tablet_id)
        if settings is None or tablet is None:
            return {}
        try:
            return settings.desired(
                tablet_id, tablet["profile_id"], self._network_interfaces(tablet_id)
            )
        except NetworkError as err:
            _LOGGER.warning("Network settings of tablet %s are not usable: %s", tablet_id, err)
            return {}

    def network_status(self, tablet_id: str) -> dict[str, Any] | None:
        """What is known about the tablet's network: no secrets, for entities and issues.

        `result` is the tablet's latest answer (`applied` or `reverted` with its reason and the
        state in force), `revision` is the one HA wants, and `pending` says that a message is
        out and unanswered.
        """
        settings = self.network_settings
        if settings is None or settings.revision(tablet_id) < 1:
            return None
        flight = self._network_flights.get(tablet_id)
        return {
            "revision": settings.revision(tablet_id),
            "result": settings.result(tablet_id),
            "pending": flight is not None
            and time.monotonic() - flight[2] < NETWORK_RESULT_WINDOW,
        }

    def network_tablets(self) -> list[str]:
        """The tablets that report network interfaces, so that HA may set their network."""
        return [
            tablet_id
            for tablet_id in self.configuration.tablets
            if self._network_interfaces(tablet_id)
        ]

    def network_form(self, tablet_id: str) -> dict[str, Any]:
        """What a form needs to show about a tablet's own network values, without the password.

        `interfaces` are the tablet's, `ssid` its own Wi-Fi name (empty: it follows its profile),
        `password_set` whether it has its own password, and `ip` its IP setups.
        """
        settings = self.network_settings
        entry = settings.tablets.get(tablet_id, {}) if settings is not None else {}
        wifi = entry.get("wifi") or {}
        return {
            "interfaces": self._network_interfaces(tablet_id),
            "ssid": wifi.get("ssid", ""),
            "password_set": bool(wifi.get("password")),
            "ip": dict(entry.get("ip", {})),
        }

    def network_profile_form(self, profile_id: str) -> dict[str, Any]:
        """The profile's Wi-Fi name and whether it has a password, for a form."""
        settings = self.network_settings
        wifi = settings.profiles.get(profile_id, {}).get("wifi", {}) if settings else {}
        return {"ssid": wifi.get("ssid", ""), "password_set": bool(wifi.get("password"))}

    async def _async_change_network(self, mutate: Callable[[NetworkSettings], None]) -> list[str]:
        """Apply one validated change to the network settings, store it and return the tablets
        whose values now differ from what HA last saw."""
        if self.network_settings is None:
            raise RuntimeError("Network settings are unavailable")
        async with self._lock:
            candidate = self.network_settings.copy()
            try:
                mutate(candidate)
            except NetworkError as err:
                raise RuntimeError(str(err)) from err
            changed = [
                tablet_id
                for tablet_id in self.configuration.tablets
                if candidate.sync(
                    tablet_id, self._network_desired(tablet_id, candidate), time.time()
                )
            ]
            await self._async_save(network=candidate)
            self.network_settings = candidate
        return changed

    async def async_set_profile_network(self, profile_id: str, wifi: Any) -> None:
        """Set the Wi-Fi credentials of a profile (`{"ssid", "password"}`) or clear them with None.

        A static address cannot be set here: it belongs to one tablet (decision N9).
        """
        if profile_id not in self.configuration.profiles:
            raise RuntimeError("Unknown profile")
        changed = await self._async_change_network(
            lambda settings: settings.set_profile_wifi(profile_id, wifi)
        )
        await self._async_network_changed(changed)

    async def async_set_tablet_network(
        self, tablet_id: str, *, wifi: Any = UNSET, ip: Any = UNSET
    ) -> None:
        """Set a tablet's own Wi-Fi credentials and its IP setups per interface.

        `wifi` replaces the profile's credentials for this tablet, `ip` maps an interface to
        `{"mode": "dhcp"}` or a static setup. None clears, an omitted argument keeps.
        """
        if tablet_id not in self.configuration.tablets:
            raise RuntimeError("Unknown tablet")
        interfaces = self._network_interfaces(tablet_id)
        changed = await self._async_change_network(
            lambda settings: settings.set_tablet(tablet_id, interfaces, wifi=wifi, ip=ip)
        )
        await self._async_network_changed(changed)

    async def _async_network_changed(self, tablet_ids: list[str]) -> None:
        for tablet_id in tablet_ids:
            self._clear_network_issue(tablet_id)
            self.async_write_tablet_state(tablet_id)
            try:
                await self._send_network_if_due(tablet_id)
            except RuntimeError as err:
                _LOGGER.warning("Could not send network settings to tablet %s: %s", tablet_id, err)

    async def _send_network_if_due(self, tablet_id: str) -> None:
        session = self._active_session_for_tablet(tablet_id)
        if session is not None:
            await self._send_network(session)

    async def _send_network(self, session: Session) -> None:
        """Send the tablet's network values when a revision is due (decision N6).

        A revision is sent once: after an answer, applied or reverted, nothing is sent until the
        values change. While a message is unanswered nothing else is sent either, because the
        tablet refuses a second one during its trial.
        """
        if (
            self.network_settings is None
            or not session.network
            or session.phase != "active"
            or session.noise is None
            or session.subscription_id is None
        ):
            return
        tablet_id = session.tablet_id
        flight = self._network_flights.get(tablet_id)
        if flight is not None and time.monotonic() - flight[2] < NETWORK_RESULT_WINDOW:
            return
        async with self._lock:
            settings = self.network_settings
            desired = self._network_desired(tablet_id)
            candidate = settings.copy()
            if candidate.sync(tablet_id, desired, time.time()):
                await self._async_save(network=candidate)
                self.network_settings = settings = candidate
            revision = settings.due(tablet_id, desired)
        if revision is None:
            return
        operation_id = uuid.uuid4().hex
        try:
            message = build_network_message(
                operation_id,
                revision,
                int(time.time() + OPERATION_TTL),
                desired,
                self._network_interfaces(tablet_id),
            )
        except NetworkError as err:
            _LOGGER.warning("Network settings of tablet %s are not sent: %s", tablet_id, err)
            return
        encoded = json.dumps(message, separators=(",", ":"), ensure_ascii=False).encode()
        if len(encoded) > MAX_PLAINTEXT_BYTES:
            _LOGGER.warning("Network settings of tablet %s exceed the transport limit", tablet_id)
            return
        frame = session.noise.encrypt(encoded)
        self._network_flights[tablet_id] = (operation_id, revision, time.monotonic())
        try:
            session.connection.send_event(
                session.subscription_id, {"frame": base64.b64encode(frame).decode("ascii")}
            )
        except Exception as err:
            self._network_flights.pop(tablet_id, None)
            self._close_session(session.session_id)
            raise RuntimeError("Could not send the network settings") from err
        _LOGGER.info("Network revision %s sent to tablet %s", revision, tablet_id)
        self.async_write_tablet_state(tablet_id)

    async def _receive_network_result(self, session: Session, payload: dict[str, Any]) -> None:
        """Handle an authenticated `network_result`.

        It is matched by tablet and revision, not by session or operation: after a Wi-Fi trial
        the answer comes on a new session, and after a restart HA no longer knows the operation.
        A malformed result and one without the negotiated capability end the session. A result
        for a revision HA never issued is ignored and acknowledged: it is what a tablet reports
        after HA lost its storage, and ending the session would make the tablet report it again
        on every connection.
        """
        tablet_id = session.tablet_id
        try:
            if not session.network or self.network_settings is None:
                raise NetworkError("parse", "Network results without the network capability")
            parsed = parse_network_result(payload, self._network_interfaces(tablet_id))
            async with self._lock:
                candidate = self.network_settings.copy()
                outcome = candidate.record_result(tablet_id, parsed, time.time())
                if outcome in {"applied", "reverted"}:
                    await self._async_save(network=candidate)
                    self.network_settings = candidate
        except NetworkError as err:
            self._close_session(session.session_id)
            raise RuntimeError(f"Invalid network result: {err}") from err
        flight = self._network_flights.get(tablet_id)
        if flight is not None and flight[1] <= parsed["revision"]:
            self._network_flights.pop(tablet_id, None)
        if outcome == "applied":
            self._clear_network_issue(tablet_id)
        elif outcome == "reverted":
            self._raise_network_issue(tablet_id, parsed["reason"])
        else:
            _LOGGER.debug(
                "Network result of tablet %s for revision %s ignored (%s)",
                tablet_id,
                parsed["revision"],
                outcome,
            )
        self.async_write_tablet_state(tablet_id)
        try:
            await self._send_network_if_due(tablet_id)
        except RuntimeError as err:
            _LOGGER.warning("Could not send network settings to tablet %s: %s", tablet_id, err)

    @property
    def _network_issues(self) -> set[str]:
        issues = self.__dict__.get("_network_issues_data")
        if issues is None:
            issues = self.__dict__["_network_issues_data"] = set()
        return issues

    def _raise_network_issue(self, tablet_id: str, reason: str) -> None:
        from homeassistant.helpers import issue_registry as ir

        self._network_issues.add(tablet_id)
        ir.async_create_issue(
            self.hass,
            DOMAIN,
            f"network_{tablet_id}",
            is_fixable=False,
            severity=ir.IssueSeverity.WARNING,
            translation_key=f"network_{reason}",
            translation_placeholders={"tablet": self.configuration.tablets[tablet_id]["name"]},
        )

    def _clear_network_issue(self, tablet_id: str) -> None:
        if tablet_id not in self._network_issues:
            return
        from homeassistant.helpers import issue_registry as ir

        self._network_issues.discard(tablet_id)
        ir.async_delete_issue(self.hass, DOMAIN, f"network_{tablet_id}")

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
        session.telemetry = None
        for _name, _value, future in session.pending_commands.values():
            if not future.done():
                future.set_exception(RuntimeError("The tablet session ended"))
        session.pending_commands.clear()
        if session.pending_install is not None:
            if not session.pending_install[1].done():
                session.pending_install[1].set_exception(RuntimeError("The tablet session ended"))
            session.pending_install = None
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


def comparison_code(tablet_public: bytes) -> str:
    """Six digits shown on the tablet and in HA, derived from the tablet's public key."""
    digest = hashlib.sha256(COMPARE_DOMAIN + bytes([PROTOCOL_VERSION]) + tablet_public).digest()
    return f"{int.from_bytes(digest[:4], 'big') % 1_000_000:06d}"


def _public_key_of(private_text: str) -> str:
    private = X25519PrivateKey.from_private_bytes(_decode_key(private_text))
    public = private.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    return base64.b64encode(public).decode("ascii")


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
