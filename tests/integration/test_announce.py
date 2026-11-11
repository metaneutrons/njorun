"""A tablet announces itself, an administrator approves it after comparing the code."""

from __future__ import annotations

import asyncio
import base64
from copy import deepcopy
import importlib.util
import json
from pathlib import Path
import sys
import types
import unittest
from unittest.mock import patch
import warnings

# Imported before the stubbed load: patch.dict drops modules first imported inside it, and a
# second copy of cryptography would reject the first copy's serialization classes.
from cryptography.hazmat.primitives import serialization  # noqa: F401
from cryptography.hazmat.primitives.asymmetric import x25519  # noqa: F401
import noise.connection  # noqa: F401


ROOT = Path(__file__).resolve().parents[2]
INTEGRATION = ROOT / "custom_components" / "njorun"
PACKAGE_NAME = "njorun_announce_test"
package = types.ModuleType(PACKAGE_NAME)
package.__path__ = [str(INTEGRATION)]
sys.modules[PACKAGE_NAME] = package


class _Store:
    def __class_getitem__(cls, _item):
        return cls


ha = types.ModuleType("homeassistant")
ha.__path__ = []
config_entries = types.ModuleType("homeassistant.config_entries")
config_entries.ConfigEntry = type("ConfigEntry", (), {})
core = types.ModuleType("homeassistant.core")
core.HomeAssistant = type("HomeAssistant", (), {})
storage = types.ModuleType("homeassistant.helpers.storage")
storage.Store = _Store
helpers = types.ModuleType("homeassistant.helpers")
helpers.__path__ = []
stub_modules = {
    "homeassistant": ha,
    "homeassistant.config_entries": config_entries,
    "homeassistant.core": core,
    "homeassistant.helpers": helpers,
    "homeassistant.helpers.storage": storage,
}
# The grant module needs no Home Assistant stub. It is loaded here for the same reason as the
# imports above, and the runtime then finds it under its package name.
grant_spec = importlib.util.spec_from_file_location(
    f"{PACKAGE_NAME}.grant", INTEGRATION / "grant.py"
)
grant_module = importlib.util.module_from_spec(grant_spec)
sys.modules[grant_spec.name] = grant_module
grant_spec.loader.exec_module(grant_module)

http_spec = importlib.util.spec_from_file_location(
    f"{PACKAGE_NAME}.announce_http", INTEGRATION / "announce_http.py"
)
http_module = importlib.util.module_from_spec(http_spec)
sys.modules[http_spec.name] = http_module
http_spec.loader.exec_module(http_module)

spec = importlib.util.spec_from_file_location(
    f"{PACKAGE_NAME}.runtime", INTEGRATION / "runtime.py"
)
runtime_module = importlib.util.module_from_spec(spec)
with patch.dict(sys.modules, stub_modules):
    sys.modules[spec.name] = runtime_module
    spec.loader.exec_module(runtime_module)

Configuration = runtime_module.Configuration
NjorunRuntime = runtime_module.NjorunRuntime
RuntimeError = runtime_module.RuntimeError
comparison_code = runtime_module.comparison_code

CAPABILITIES = {"orientations": ["portrait", "landscape"], "brightness": True}
TABLET_KEY = base64.b64encode(bytes(range(32))).decode()
OTHER_KEY = base64.b64encode(bytes(range(1, 33))).decode()


class _MemoryStore:
    def __init__(self):
        self.data = None

    async def async_save(self, data):
        self.data = deepcopy(data)


class _FakeAuth:
    """The part of Home Assistant's AuthManager the integration uses."""

    def __init__(self):
        self.users = {}
        self.tokens = {}
        self.removed = []
        self._count = 0

    async def async_create_user(self, name, *, group_ids=None, local_only=None):
        self._count += 1
        user = types.SimpleNamespace(
            id=f"user-{self._count}", name=name, group_ids=group_ids, local_only=local_only
        )
        self.users[user.id] = user
        return user

    async def async_create_refresh_token(self, user, client_id=None, client_name=None, **_):
        self._count += 1
        token = types.SimpleNamespace(
            id=f"token-{self._count}",
            token=f"secret-{self._count}",
            client_id=client_id,
            client_name=client_name,
            user=user,
        )
        self.tokens[token.id] = token
        return token

    def async_get_refresh_token(self, token_id):
        return self.tokens.get(token_id)

    async def async_get_user(self, user_id):
        return self.users.get(user_id)

    async def async_remove_user(self, user):
        self.users.pop(user.id)
        self.removed.append(user.id)
        for token_id in [key for key, value in self.tokens.items() if value.user.id == user.id]:
            del self.tokens[token_id]


def _runtime():
    config = Configuration()
    config.add_profile("home", "Home", {}, 0)
    runtime = NjorunRuntime.__new__(NjorunRuntime)
    runtime.configuration = config
    runtime.bindings = {}
    runtime.applied = {}
    runtime.sessions = {}
    runtime.session_by_tablet = {}
    runtime.pending_operations = {}
    runtime.installs = {}
    runtime.completed_operations = set()
    runtime.announcements = {}
    runtime.entity_adders = {}
    runtime.entities = {}
    runtime._registered_platforms = set()
    runtime._entities_created = set()
    runtime._lock = asyncio.Lock()
    runtime._closed = False
    runtime.store = _MemoryStore()
    runtime.hass = types.SimpleNamespace(auth=_FakeAuth())
    return runtime


SIGNED_IN = types.SimpleNamespace(user=types.SimpleNamespace(id="user"))


def _announce(runtime, tablet_id="njorun-1", key=TABLET_KEY, connection=SIGNED_IN, **changes):
    args = {
        "tablet_id": tablet_id,
        "name": "Kitchen",
        "tablet_public_key": key,
        "capabilities": CAPABILITIES,
    }
    args.update(changes)
    return runtime.async_announce(connection, **args)


class ComparisonCodeTests(unittest.TestCase):
    def test_matches_the_shared_fixture(self):
        fixture = json.loads((ROOT / "protocol/fixtures/comparison-code.json").read_text())
        for case in fixture["cases"]:
            key = base64.b64decode(case["tablet_public_key"])
            self.assertEqual(comparison_code(key), case["code"])


class _Unique:
    def __init__(self, unique_id):
        self.unique_id = unique_id


class AnnounceTests(unittest.IsolatedAsyncioTestCase):
    async def test_announced_tablet_waits_then_is_approved(self):
        runtime = _runtime()
        result = _announce(runtime)
        self.assertEqual(result["status"], "pending")
        self.assertEqual(result["code"], comparison_code(bytes(range(32))))
        self.assertEqual(
            runtime.pending_announcements(),
            [{"tablet_id": "njorun-1", "name": "Kitchen", "code": result["code"]}],
        )
        self.assertIsNone(runtime.store.data, "announcing persists nothing")

        enrolled = await runtime.async_approve(
            tablet_id="njorun-1",
            profile_id="home",
            expected_revision=runtime.configuration.revision,
        )
        self.assertEqual(enrolled["status"], "pending")
        self.assertEqual(runtime.pending_announcements(), [])
        self.assertIn("njorun-1", runtime.configuration.tablets)

        approved = _announce(runtime)
        self.assertEqual(approved["status"], "approved")
        self.assertEqual(approved["binding_id"], enrolled["binding_id"])
        self.assertEqual(approved["ha_public_key"], enrolled["ha_public_key"])

    async def test_another_key_under_a_paired_id_is_a_conflict(self):
        runtime = _runtime()
        _announce(runtime)
        await runtime.async_approve(
            tablet_id="njorun-1",
            profile_id="home",
            expected_revision=runtime.configuration.revision,
        )
        self.assertEqual(_announce(runtime, key=OTHER_KEY), {"status": "conflict"})

    async def test_revoked_binding_lets_the_tablet_wait_again(self):
        runtime = _runtime()
        _announce(runtime)
        enrolled = await runtime.async_approve(
            tablet_id="njorun-1",
            profile_id="home",
            expected_revision=runtime.configuration.revision,
        )
        runtime.bindings[enrolled["binding_id"]]["status"] = "revoked"
        self.assertEqual(_announce(runtime)["status"], "pending")

    async def test_approval_needs_a_live_announcement(self):
        runtime = _runtime()
        with self.assertRaises(RuntimeError):
            await runtime.async_approve(
                tablet_id="njorun-1", profile_id="home", expected_revision=0
            )
        _announce(runtime)
        runtime.announcements["njorun-1"]["expires_at"] = 0
        self.assertEqual(runtime.pending_announcements(), [])
        with self.assertRaises(RuntimeError):
            await runtime.async_approve(
                tablet_id="njorun-1",
                profile_id="home",
                expected_revision=runtime.configuration.revision,
            )

    def test_invalid_announcements_are_refused(self):
        runtime = _runtime()
        with self.assertRaises(RuntimeError):
            _announce(runtime, connection=types.SimpleNamespace())
        for changes in (
            {"tablet_id": ""},
            {"tablet_id": "x" * 129},
            {"name": " "},
            {"key": "AAAA"},
            {"capabilities": {"orientations": ["upside"], "brightness": True}},
        ):
            with self.subTest(changes=changes), self.assertRaises(RuntimeError):
                _announce(runtime, **changes)
        runtime._closed = True
        with self.assertRaises(RuntimeError):
            _announce(runtime)

    async def test_a_newly_waiting_tablet_is_offered_once(self):
        runtime = _runtime()
        offered = []
        runtime.on_announcement = offered.append
        _announce(runtime)
        _announce(runtime)
        _announce(runtime, tablet_id="njorun-2")
        self.assertEqual(offered, ["njorun-1", "njorun-2"])
        runtime.announcements["njorun-1"]["expires_at"] = 0
        _announce(runtime)
        self.assertEqual(offered, ["njorun-1", "njorun-2", "njorun-1"])

    async def test_announcement_lookup_forgets_expired_tablets(self):
        runtime = _runtime()
        _announce(runtime)
        self.assertEqual(
            runtime.announcement("njorun-1"),
            {"tablet_id": "njorun-1", "name": "Kitchen", "code": comparison_code(bytes(range(32)))},
        )
        self.assertIsNotNone(runtime.announcement_expires_at("njorun-1"))
        runtime.announcements["njorun-1"]["expires_at"] = 0
        self.assertIsNone(runtime.announcement("njorun-1"))
        self.assertIsNone(runtime.announcement_expires_at("njorun-1"))

    async def _enrolled(self):
        runtime = _runtime()
        await runtime.async_configure(
            {"operation": "add_profile", "profile_id": "other", "name": "Other"},
            expected_revision=runtime.configuration.revision,
        )
        _announce(runtime)
        await runtime.async_approve(
            tablet_id="njorun-1",
            profile_id="home",
            expected_revision=runtime.configuration.revision,
        )
        return runtime

    async def test_use_profile_values_clears_the_tablets_own_settings(self):
        runtime = await self._enrolled()
        await runtime.async_set_setting("njorun-1", "theme", "dark")
        self.assertEqual(runtime.configuration.tablets["njorun-1"]["overrides"], {"theme": "dark"})
        await runtime.async_use_profile_values("njorun-1")
        self.assertEqual(runtime.configuration.tablets["njorun-1"]["overrides"], {})
        revision = runtime.configuration.revision
        await runtime.async_use_profile_values("njorun-1")
        self.assertEqual(runtime.configuration.revision, revision, "nothing to clear, nothing to send")
        with self.assertRaises(RuntimeError):
            await runtime.async_use_profile_values("missing")

    async def test_choosing_another_profile_keeps_the_tablets_own_settings(self):
        runtime = await self._enrolled()
        await runtime.async_set_setting("njorun-1", "theme", "dark")
        await runtime.async_set_profile("njorun-1", "other")
        tablet = runtime.configuration.tablets["njorun-1"]
        self.assertEqual(tablet["profile_id"], "other")
        self.assertEqual(tablet["overrides"], {"theme": "dark"})
        revision = runtime.configuration.revision
        await runtime.async_set_profile("njorun-1", "other")
        self.assertEqual(runtime.configuration.revision, revision)
        with self.assertRaises(RuntimeError):
            await runtime.async_set_profile("njorun-1", "nope")

    async def test_the_log_sink_reaches_every_tablet_through_a_new_revision(self):
        runtime = await self._enrolled()
        before = runtime.configuration.tablets["njorun-1"]["revision"]
        await runtime.async_set_log_sink("https://loki.example.org")
        self.assertEqual(runtime.configuration.log_sink, "https://loki.example.org")
        self.assertGreater(runtime.configuration.tablets["njorun-1"]["revision"], before)
        self.assertEqual(runtime.store.data["configuration"]["log_sink"], "https://loki.example.org")
        with self.assertRaises(RuntimeError):
            await runtime.async_set_log_sink("http://user:pw@host")
        await runtime.async_set_log_sink("")
        self.assertEqual(runtime.configuration.log_sink, "")

    async def test_removing_a_tablet_forgets_its_trust_and_lets_it_wait_again(self):
        runtime = await self._enrolled()
        await runtime.async_remove_tablet("njorun-1")
        self.assertNotIn("njorun-1", runtime.configuration.tablets)
        self.assertEqual(runtime.bindings, {})
        self.assertEqual(_announce(runtime)["status"], "pending")

    async def test_a_removed_entity_can_be_created_again(self):
        runtime = await self._enrolled()
        first = _Unique("njorun-1_a")
        second = _Unique("njorun-1_b")
        runtime._entities_created = {
            ("select", "njorun-1", "njorun-1_a"),
            ("select", "njorun-1", "njorun-1_b"),
            ("select", "njorun-2", "njorun-2_a"),
        }
        runtime.add_entity("njorun-1", first)
        runtime.add_entity("njorun-1", second)
        runtime.remove_entity("njorun-1", first)
        self.assertEqual(
            runtime._entities_created,
            {("select", "njorun-1", "njorun-1_b"), ("select", "njorun-2", "njorun-2_a")},
        )
        runtime.remove_entity("njorun-1", second)
        self.assertEqual(runtime._entities_created, {("select", "njorun-2", "njorun-2_a")})

    def test_waiting_list_is_bounded(self):
        runtime = _runtime()
        for index in range(runtime_module.MAX_ANNOUNCEMENTS):
            _announce(runtime, tablet_id=f"t-{index}")
        with self.assertRaises(RuntimeError):
            _announce(runtime, tablet_id="one-too-many")
        self.assertEqual(_announce(runtime, tablet_id="t-0")["status"], "pending")


def _keypair(seed=b"pairing test tablet"):
    import hashlib

    private = hashlib.sha256(seed).digest()
    key = x25519.X25519PrivateKey.from_private_bytes(private)
    public = key.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    return private, public


def _public(runtime, public, tablet_id="njorun-1", **changes):
    args = {
        "tablet_id": tablet_id,
        "name": "Kitchen",
        "tablet_public_key": base64.b64encode(public).decode(),
        "capabilities": CAPABILITIES,
    }
    args.update(changes)
    return runtime.async_announce_public(**args)


async def _approve(runtime, **changes):
    args = {
        "tablet_id": "njorun-1",
        "profile_id": "home",
        "expected_revision": runtime.configuration.revision,
    }
    args.update(changes)
    return await runtime.async_approve(**args)


class PublicAnnounceTests(unittest.IsolatedAsyncioTestCase):
    """Plan 004: announcing without sign-in, access created at approval, the sealed grant."""

    async def test_the_endpoint_announces_like_the_websocket_command(self):
        runtime = _runtime()
        _, public = _keypair()
        result = await _public(runtime, public)
        self.assertEqual(result, {"status": "pending", "code": comparison_code(public)})
        self.assertEqual(await _public(runtime, public), result)
        other = await _public(runtime, _keypair(b"another")[1], tablet_id="njorun-2")
        self.assertEqual(other["status"], "pending")

    async def test_approval_creates_a_local_user_with_one_refresh_token(self):
        runtime = _runtime()
        _, public = _keypair()
        await _public(runtime, public)
        await _approve(runtime)
        auth = runtime.hass.auth
        (user,) = auth.users.values()
        self.assertEqual(user.name, "Tablet Kitchen")
        self.assertEqual(user.group_ids, ["system-users"])
        self.assertTrue(user.local_only)
        (token,) = auth.tokens.values()
        self.assertEqual(token.client_id, runtime_module.GRANT_CLIENT_ID)
        (binding,) = runtime.bindings.values()
        self.assertEqual(binding["user_id"], user.id)
        self.assertEqual(binding["refresh_token_id"], token.id)

    async def test_the_approved_tablet_opens_its_grant_and_nobody_else_can(self):
        runtime = _runtime()
        private, public = _keypair()
        await _public(runtime, public)
        await _approve(runtime)
        result = await _public(runtime, public)
        self.assertEqual(result["status"], "approved")
        binding_id = result["binding_id"]
        opened = grant_module.open_grant(private, binding_id, result["grant"])
        (token,) = runtime.hass.auth.tokens.values()
        self.assertEqual(
            opened,
            {"refresh_token": token.token, "client_id": runtime_module.GRANT_CLIENT_ID},
        )
        stranger, _ = _keypair(b"somebody else")
        with self.assertRaises(Exception):
            grant_module.open_grant(stranger, binding_id, result["grant"])
        with self.assertRaises(Exception):
            grant_module.open_grant(private, "another-binding", result["grant"])

    async def test_the_grant_is_repeated_until_the_first_confirmed_session(self):
        runtime = _runtime()
        private, public = _keypair()
        await _public(runtime, public)
        await _approve(runtime)
        first = await _public(runtime, public)
        second = await _public(runtime, public)
        self.assertNotEqual(first["grant"], second["grant"], "a fresh ephemeral key each time")
        self.assertEqual(
            grant_module.open_grant(private, first["binding_id"], first["grant"]),
            grant_module.open_grant(private, second["binding_id"], second["grant"]),
        )
        self.assertEqual(len(runtime.hass.auth.tokens), 1)
        runtime.bindings[first["binding_id"]]["status"] = "active"
        self.assertNotIn("grant", await _public(runtime, public))

    async def test_a_tablet_without_access_gets_new_access_only_when_it_asks(self):
        runtime = _runtime()
        private, public = _keypair()
        await _public(runtime, public)
        await _approve(runtime)
        result = await _public(runtime, public)
        runtime.bindings[result["binding_id"]]["status"] = "active"
        auth = runtime.hass.auth
        (old_user,) = auth.users.values()
        await auth.async_remove_user(old_user)
        self.assertNotIn("grant", await _public(runtime, public))
        self.assertEqual(len(auth.users), 0, "an announcement alone creates nothing")
        again = await _public(runtime, public, need_grant=True)
        opened = grant_module.open_grant(private, again["binding_id"], again["grant"])
        (new_token,) = auth.tokens.values()
        self.assertEqual(opened["refresh_token"], new_token.token)

    async def test_a_working_token_is_never_replaced_by_an_announcement(self):
        runtime = _runtime()
        _, public = _keypair()
        await _public(runtime, public)
        await _approve(runtime)
        result = await _public(runtime, public)
        runtime.bindings[result["binding_id"]]["status"] = "active"
        before = set(runtime.hass.auth.tokens)
        await _public(runtime, public, need_grant=True)
        self.assertEqual(set(runtime.hass.auth.tokens), before)
        self.assertEqual(len(runtime.hass.auth.users), 1)

    async def test_a_tablet_announced_over_the_websocket_gets_no_grant(self):
        runtime = _runtime()
        _announce(runtime)
        await _approve(runtime)
        result = _announce(runtime)
        self.assertEqual(result["status"], "approved")
        self.assertNotIn("grant", result)

    async def test_removing_the_tablet_removes_its_user_and_token(self):
        runtime = _runtime()
        _, public = _keypair()
        await _public(runtime, public)
        await _approve(runtime)
        (user,) = runtime.hass.auth.users.values()
        await runtime.async_remove_tablet("njorun-1")
        self.assertEqual(runtime.hass.auth.removed, [user.id])
        self.assertEqual(runtime.hass.auth.tokens, {})

    async def test_revoking_the_tablet_removes_its_user_and_forgets_the_ids(self):
        runtime = _runtime()
        _, public = _keypair()
        await _public(runtime, public)
        await _approve(runtime)
        await runtime.async_configure(
            {"operation": "revoke", "tablet_id": "njorun-1"},
            expected_revision=runtime.configuration.revision,
        )
        self.assertEqual(len(runtime.hass.auth.removed), 1)
        (binding,) = runtime.bindings.values()
        self.assertEqual(binding["status"], "revoked")
        self.assertNotIn("user_id", binding)
        self.assertNotIn("refresh_token_id", binding)

    async def test_removing_the_integration_removes_every_tablet_user(self):
        runtime = _runtime()
        runtime.store.async_remove = _noop_async
        for index, seed in enumerate((b"one", b"two")):
            _, public = _keypair(seed)
            await _public(runtime, public, tablet_id=f"njorun-{index}")
            await _approve(runtime, tablet_id=f"njorun-{index}")
        await runtime.async_remove()
        self.assertEqual(len(runtime.hass.auth.removed), 2)
        self.assertEqual(runtime.hass.auth.users, {})

    async def test_a_failed_approval_leaves_no_user_behind(self):
        runtime = _runtime()
        _, public = _keypair()
        await _public(runtime, public)
        with self.assertRaises(RuntimeError):
            await _approve(runtime, expected_revision=runtime.configuration.revision + 1)
        self.assertEqual(runtime.hass.auth.users, {})
        self.assertEqual(len(runtime.hass.auth.removed), 1)
        self.assertEqual((await _public(runtime, public))["status"], "pending")

    async def test_a_new_profile_for_this_tablet_only(self):
        runtime = _runtime()
        _, public = _keypair()
        await _public(runtime, public)
        await _approve(runtime, profile_id=None, new_profile=True)
        tablet = runtime.configuration.tablets["njorun-1"]
        profile = runtime.configuration.profiles[tablet["profile_id"]]
        self.assertEqual(profile["name"], "Kitchen")
        self.assertNotEqual(tablet["profile_id"], "home")
        self.assertIn("home", runtime.configuration.profiles)

    async def test_without_a_choice_there_is_no_profile(self):
        runtime = _runtime()
        _, public = _keypair()
        await _public(runtime, public)
        with self.assertRaises(RuntimeError):
            await _approve(runtime, profile_id=None)
        self.assertEqual(runtime.hass.auth.users, {})

    async def test_the_limits_and_the_input_checks_of_the_endpoint(self):
        runtime = _runtime()
        for index in range(runtime_module.MAX_ANNOUNCEMENTS):
            await _public(runtime, _keypair(str(index).encode())[1], tablet_id=f"t-{index}")
        with self.assertRaises(runtime_module.AnnounceLimitError):
            await _public(runtime, _keypair(b"x")[1], tablet_id="one-too-many")
        with self.assertRaises(RuntimeError):
            await _public(runtime, _keypair()[1], need_grant="yes")
        runtime._closed = True
        with self.assertRaises(runtime_module.AnnounceUnloadingError):
            await _public(runtime, _keypair()[1])
        runtime._closed = False
        with self.assertRaises(RuntimeError):
            await _public(runtime, b"short")

    async def test_a_conflicting_key_is_reported(self):
        runtime = _runtime()
        _, public = _keypair()
        await _public(runtime, public)
        await _approve(runtime)
        self.assertEqual(
            await _public(runtime, _keypair(b"intruder")[1]), {"status": "conflict"}
        )


async def _noop_async(*_args, **_kwargs):
    return None


class GrantFixtureTests(unittest.TestCase):
    """The test vector that the firmware reads as well (protocol/fixtures/grant.json)."""

    def setUp(self):
        import hashlib

        self.fixture = json.loads((ROOT / "protocol/fixtures/grant.json").read_text())
        self.tablet_private = hashlib.sha256(
            self.fixture["tablet_private_key_seed"].encode()
        ).digest()
        self.ephemeral = hashlib.sha256(
            self.fixture["ha_ephemeral_private_key_seed"].encode()
        ).digest()

    def test_the_seeds_give_the_published_public_key(self):
        _, public = _keypair_from(self.tablet_private)
        self.assertEqual(base64.b64encode(public).decode(), self.fixture["tablet_public_key"])

    def test_sealing_with_the_fixture_ephemeral_key_gives_the_fixture_grant(self):
        payload = json.loads(self.fixture["plaintext"])
        with warnings.catch_warnings():
            # The library warns about a preset ephemeral key, which is what a test vector needs.
            warnings.simplefilter("ignore", UserWarning)
            grant = grant_module.seal_grant(
                base64.b64decode(self.fixture["tablet_public_key"]),
                self.fixture["binding_id"],
                payload["refresh_token"],
                payload["client_id"],
                ephemeral_private_key=self.ephemeral,
            )
        self.assertEqual(grant, self.fixture["grant"])

    def test_the_fixture_grant_opens(self):
        opened = grant_module.open_grant(
            self.tablet_private, self.fixture["binding_id"], self.fixture["grant"]
        )
        self.assertEqual(opened, json.loads(self.fixture["plaintext"]))


def _keypair_from(private):
    key = x25519.X25519PrivateKey.from_private_bytes(private)
    return private, key.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )


class EndpointTests(unittest.IsolatedAsyncioTestCase):
    """The HTTP side of the announcement endpoint: statuses, limits, input checks."""

    def setUp(self):
        self.runtime = _runtime()
        self.limiter = http_module.RateLimiter()
        _, self.public = _keypair()

    def body(self, **changes):
        message = {
            "protocol": 1,
            "tablet_id": "njorun-1",
            "name": "Kitchen",
            "tablet_public_key": base64.b64encode(self.public).decode(),
            "capabilities": CAPABILITIES,
        }
        message.update(changes)
        return json.dumps({k: v for k, v in message.items() if v is not ...}).encode()

    async def call(self, body=None, content_type="application/json", remote="192.0.2.7", runtime=...):
        return await http_module.handle_announce(
            self.runtime if runtime is ... else runtime,
            self.limiter,
            remote,
            content_type,
            self.body() if body is None else body,
        )

    async def test_a_new_tablet_is_pending_with_its_code(self):
        status, payload, headers = await self.call()
        self.assertEqual((status, headers), (200, {}))
        self.assertEqual(payload, {"status": "pending", "code": comparison_code(self.public)})

    async def test_an_approved_tablet_gets_its_grant_in_the_same_answer(self):
        await self.call()
        await _approve(self.runtime)
        status, payload, _ = await self.call()
        self.assertEqual(status, 200)
        self.assertEqual(payload["status"], "approved")
        self.assertIn("grant", payload)

    async def test_without_an_installed_integration_the_answer_is_404(self):
        status, payload, _ = await self.call(runtime=None)
        self.assertEqual((status, payload), (404, None))

    async def test_a_conflict_is_a_normal_answer(self):
        await self.call()
        await _approve(self.runtime)
        other = _keypair(b"intruder")[1]
        status, payload, _ = await self.call(
            self.body(tablet_public_key=base64.b64encode(other).decode())
        )
        self.assertEqual((status, payload), (200, {"status": "conflict"}))

    async def test_bad_input_is_a_400_and_changes_nothing(self):
        cases = {
            "not json": b"{",
            "not an object": b"[]",
            "unknown field": self.body(extra=1),
            "missing field": self.body(name=...),
            "old protocol": self.body(protocol=2),
            "boolean protocol": self.body(protocol=True),
            "bad key": self.body(tablet_public_key="AAAA"),
            "need_grant not boolean": self.body(need_grant="yes"),
            "bad capabilities": self.body(capabilities={"orientations": "x"}),
        }
        for label, body in cases.items():
            with self.subTest(label):
                status, payload, _ = await self.call(body)
                self.assertEqual(status, 400)
                self.assertEqual(payload["error"], "invalid")
        self.assertEqual(self.runtime.announcements, {})

    async def test_only_json_and_only_small_bodies(self):
        status, _, _ = await self.call(content_type="text/plain")
        self.assertEqual(status, 400)
        status, _, _ = await self.call(content_type="application/json; charset=utf-8")
        self.assertEqual(status, 200)
        status, payload, _ = await self.call(b" " * (http_module.MAX_BODY_BYTES + 1))
        self.assertEqual(status, 400)

    async def test_a_full_waiting_list_and_an_unloading_integration_are_503(self):
        for index in range(runtime_module.MAX_ANNOUNCEMENTS):
            await _public(self.runtime, _keypair(str(index).encode())[1], tablet_id=f"t-{index}")
        status, payload, _ = await self.call()
        self.assertEqual((status, payload), (503, {"error": "too_many_waiting"}))
        self.runtime._closed = True
        status, payload, _ = await self.call()
        self.assertEqual((status, payload), (503, {"error": "unloading"}))

    async def test_the_rate_limit_is_per_address_and_has_a_retry_after(self):
        now = [1000.0]
        self.limiter = http_module.RateLimiter(limit=3, window=60.0, clock=lambda: now[0])
        for _ in range(3):
            self.assertEqual((await self.call())[0], 200)
        status, payload, headers = await self.call()
        self.assertEqual((status, payload), (429, {"error": "rate_limited"}))
        self.assertGreaterEqual(int(headers["Retry-After"]), 1)
        self.assertEqual((await self.call(remote="192.0.2.8"))[0], 200, "another address")
        now[0] += 61.0
        self.assertEqual((await self.call())[0], 200, "the window passed")

    def test_the_limiter_forgets_idle_clients_and_stays_bounded(self):
        now = [0.0]
        limiter = http_module.RateLimiter(limit=1, window=10.0, max_clients=2, clock=lambda: now[0])
        self.assertTrue(limiter.allow("a")[0])
        self.assertTrue(limiter.allow("b")[0])
        self.assertFalse(limiter.allow("c")[0], "the table is full")
        now[0] += 11.0
        self.assertTrue(limiter.allow("c")[0], "idle entries were dropped")


class TabletNameTests(unittest.IsolatedAsyncioTestCase):
    """The administrator names the tablet at approval (plan 004 follow-up)."""

    async def approved(self, **changes):
        runtime = _runtime()
        _, public = _keypair()
        await _public(runtime, public)
        await _approve(runtime, **changes)
        return runtime

    async def test_the_chosen_name_is_the_tablet_the_user_and_the_new_profile(self):
        runtime = await self.approved(name="  Küche ", profile_id=None, new_profile=True)
        tablet = runtime.configuration.tablets["njorun-1"]
        self.assertEqual(tablet["name"], "Küche")
        self.assertEqual(runtime.configuration.profiles[tablet["profile_id"]]["name"], "Küche")
        (user,) = runtime.hass.auth.users.values()
        self.assertEqual(user.name, "Tablet Küche")
        (token,) = runtime.hass.auth.tokens.values()
        self.assertEqual(token.client_name, "Tablet Küche")

    async def test_without_a_name_the_announced_one_is_used(self):
        runtime = await self.approved()
        self.assertEqual(runtime.configuration.tablets["njorun-1"]["name"], "Kitchen")
        (user,) = runtime.hass.auth.users.values()
        self.assertEqual(user.name, "Tablet Kitchen")

    async def test_the_user_name_does_not_repeat_the_brand(self):
        runtime = _runtime()
        _, public = _keypair()
        await _public(runtime, public, name="D1001 f3e281")
        await _approve(runtime)
        (user,) = runtime.hass.auth.users.values()
        self.assertEqual(user.name, "Tablet D1001 f3e281")
        self.assertNotIn("Njörun", user.name)

    async def test_an_unusable_name_is_refused_and_leaves_no_user(self):
        for bad in ("", "   ", "x" * 129):
            with self.subTest(bad=bad):
                runtime = _runtime()
                _, public = _keypair()
                await _public(runtime, public)
                with self.assertRaises(RuntimeError):
                    await _approve(runtime, name=bad)
                self.assertEqual(runtime.hass.auth.users, {})
                self.assertEqual(runtime.configuration.tablets, {})
                self.assertEqual((await _public(runtime, public))["status"], "pending")


if __name__ == "__main__":
    unittest.main()
