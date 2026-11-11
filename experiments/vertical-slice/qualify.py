#!/usr/bin/env python3
"""Exercise the real HA integration and Rust simulator in an isolated HA instance.

Uses the pinned image and sanitized HTTP helpers from the callback experiment.
Private state lives in a temporary directory and per-run Docker volume. No
existing HA is contacted; both are removed after the probe completes.
"""
from __future__ import annotations

import asyncio
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import queue
import secrets
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import sys

import aiohttp
import aiohttp.web

SIMULATORS = []
STAGES = {}
ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("callback_probe", ROOT / "experiments/ha-callback/qualify.py")
probe = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = probe
SPEC.loader.exec_module(probe)


class Failure(RuntimeError):
    """Only sanitized diagnostic descriptions may be raised."""


def stage(name, status, **details):
    STAGES[name] = {"status": status, **details}


def available_host_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def file_sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_template_light_fixture(config_dir, internal_url=None):
    fixture = "\n".join(
        [
            *(["homeassistant:", f"  internal_url: {internal_url}"] if internal_url else []),
            "default_config:",
            "input_boolean:",
            "  njorun_test_switch:",
            "    name: Njörun Test Switch",
            "template:",
            "  - light:",
            "      - name: Njörun Test Light",
            "        unique_id: njorun_test_light",
            '        state: "{{ is_state(\'input_boolean.njorun_test_switch\', \'on\') }}"',
            "        turn_on:",
            "          - action: input_boolean.turn_on",
            "            target:",
            "              entity_id: input_boolean.njorun_test_switch",
            "        turn_off:",
            "          - action: input_boolean.turn_off",
            "            target:",
            "              entity_id: input_boolean.njorun_test_switch",
        ]
    ) + "\n"
    fixture_path = config_dir / "configuration.yaml"
    fixture_path.write_text(fixture)
    return fixture_path


async def wait_for_core_running(base_url, access_token):
    deadline = time.monotonic() + 240
    while time.monotonic() < deadline:
        try:
            status, payload = probe.http_request(
                base_url, "/api/core/state", bearer=access_token
            )
            if status in (401, 403):
                raise Failure("Home Assistant rejected the existing qualification credentials")
            if status == 200 and isinstance(payload, dict):
                if payload.get("state") == "RUNNING":
                    return payload["state"]
        except probe.ProbeFailure:
            pass
        await asyncio.sleep(1)
    raise Failure("Home Assistant did not reach RUNNING after restart")


def check(condition, message):
    if not condition:
        raise Failure(message)


def private_json(path, value):
    with path.open("x") as output:
        path.chmod(0o600)
        json.dump(value, output)


def bootstrap(base_url):
    client_id = "http://127.0.0.1/njorun-test"
    username, password = secrets.token_hex(12), secrets.token_urlsafe(32)
    status, user = probe.post_json(base_url, "/api/onboarding/users", {
        "name": "Njörun qualification", "username": username, "password": password,
        "client_id": client_id, "language": "en",
    })
    check(status == 200, "Onboarding failed")
    status, tokens = probe.http_request(base_url, "/auth/token", method="POST", form_body={
        "grant_type": "authorization_code", "client_id": client_id, "code": user["auth_code"],
    })
    check(status == 200, "Onboarding token exchange failed")
    for path, payload in [
        ("/api/onboarding/core_config", {}), ("/api/onboarding/analytics", {}),
        ("/api/onboarding/integration", {"client_id": client_id, "redirect_uri": client_id}),
    ]:
        status, _ = probe.post_json(base_url, path, payload, tokens["access_token"])
        check(status == 200, "Onboarding completion failed")
    return tokens, username, password


def device_credentials(base_url, username, password, tablet_id):
    status, providers = probe.http_request(base_url, "/auth/providers")
    check(status == 200, "Auth providers unavailable")
    provider = next(item for item in providers["providers"] if item["type"] == "homeassistant")
    client_id = f"http://127.0.0.1/njorun-test/{tablet_id}"
    status, result, verifier = probe.login_code(base_url, client_id, client_id,
        [provider["type"], provider.get("id")], username, password)
    check(status == 200 and result["type"] == "create_entry", "Device authorization failed")
    status, tokens = probe.http_request(base_url, "/auth/token", method="POST", form_body={
        "grant_type": "authorization_code", "client_id": client_id,
        "code": result["result"], "code_verifier": verifier,
    })
    check(status == 200, "Device token exchange failed")
    return tokens, client_id


class Admin:
    def __init__(self, socket):
        self.socket = socket
        self.next_id = 1

    async def request(self, command, *, success=True):
        command = dict(command, id=self.next_id)
        self.next_id += 1
        await self.socket.send_json(command)
        while True:
            message = await self.socket.receive_json(timeout=20)
            if message.get("id") == command["id"]:
                check(message.get("type") == "result", "Unexpected admin response")
                check(message.get("success") is success, "Admin request result differed from expectation")
                return message.get("result") if success else message.get("error")

    async def configure(self, operation, **arguments):
        state = await self.request({"type": "njorun/list"})
        return await self.request({"type": "njorun/configure", "operation": operation,
                                   "expected_revision": state["revision"], **arguments})


def effective_brightness(snapshot, tablet_id):
    tablet = snapshot["tablets"][tablet_id]
    profile = snapshot["profiles"][tablet["profile_id"]]
    return tablet["overrides"].get("brightness", profile["settings"]["brightness"])


async def wait_for_applied_brightness(admin, tablet_id, brightness, timeout=20):
    """Wait until HA's native number reflects the device's persisted ACK."""
    deadline = time.monotonic() + timeout
    unique_id = f"{tablet_id}_brightness"
    while time.monotonic() < deadline:
        registry = await admin.request({"type": "config/entity_registry/list"})
        entity_id = next(
            (
                entity["entity_id"]
                for entity in registry
                if entity.get("unique_id") == unique_id
            ),
            None,
        )
        if entity_id is not None:
            states = await admin.request({"type": "get_states"})
            entity_state = next(
                (state for state in states if state.get("entity_id") == entity_id),
                None,
            )
            if entity_state is not None:
                try:
                    if float(entity_state["state"]) == brightness:
                        return entity_id
                except (TypeError, ValueError):
                    pass
        await asyncio.sleep(0.2)
    raise Failure(f"HA applied ACK brightness did not reach {tablet_id}")


async def entity_id_for_unique_id(admin, unique_id, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        registry = await admin.request({"type": "config/entity_registry/list"})
        entity_id = next(
            (
                entity["entity_id"]
                for entity in registry
                if entity.get("unique_id") == unique_id
            ),
            None,
        )
        if entity_id is not None:
            return entity_id
        await asyncio.sleep(0.2)
    raise Failure(f"HA native entity did not register: {unique_id}")


async def wait_for_entity_state(admin, entity_id, expected, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        states = await admin.request({"type": "get_states"})
        state = next(
            (item.get("state") for item in states if item.get("entity_id") == entity_id),
            None,
        )
        if state is not None:
            if isinstance(expected, (int, float)):
                try:
                    if float(state) == expected:
                        return state
                except (TypeError, ValueError):
                    pass
            elif state == expected:
                return state
        await asyncio.sleep(0.2)
    raise Failure(f"HA entity state did not reach expected value: {entity_id}")


async def entity_state(admin, entity_id):
    states = await admin.request({"type": "get_states"})
    return next((item for item in states if item.get("entity_id") == entity_id), None)


async def wait_for_state(admin, entity_id, predicate, description, timeout=25):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        state = await entity_state(admin, entity_id)
        if state is not None and predicate(state):
            return state
        await asyncio.sleep(0.3)
    raise Failure(f"HA entity {entity_id} did not reach: {description}")


RELEASES = None  # the fake release server of this run, set by main()
RELEASE_REPO = "metaneutrons/njorun"
RELEASE_BOARD = ("simulator", "qualify", "qualify-v1")


class ReleaseServer:
    """A fake GitHub on the host: a release list and signed files, for the D7 update stage.

    The integration in the container is a copy with the release keys replaced by throwaway
    test keys and the two GitHub URLs pointed here (see `patched_components`). Everything
    else, including the verification, is the code that ships.
    """

    def __init__(self):
        sys.path.insert(0, str(ROOT / "scripts/release"))
        import fixtures
        import manifest as reference
        self.reference = reference
        self.key = fixtures.test_key("current")
        self.next_key = fixtures.test_key("next")
        self.items = []
        self.files = {}
        self.port = None
        self._loop = None
        self._thread = None
        self._ready = threading.Event()
        for version, channel in (("9.0.0", "stable"), ("8.0.0", "stable"), ("9.1.0-beta.1", "beta")):
            self.add(version, channel)

    def add(self, version, channel):
        reference = self.reference
        board, revision, layout = RELEASE_BOARD
        image = (version.encode() * 8192)[:65536]
        name = f"njorun-{board}-{version}-app.bin"
        entry = {
            "board": board, "hardware_revisions": [revision], "partition_layout": layout,
            "partition_table_sha256": "0" * 64,
            "coprocessor": {"chip": "esp32-c6", "min_firmware": None},
            "min_integration_protocol": 1,
            "artifacts": {
                "factory": reference.artifact_entry(f"njorun-{board}-{version}-factory.bin", b"f" * 64),
                "app": reference.artifact_entry(name, image)}}
        text = reference.build_manifest(
            version=version, channel=channel, source_commit="a" * 40,
            signing_key_id=reference.key_id(reference.public_raw(self.key.public_key())),
            boards=[entry])
        tag = f"v{version}"
        for file_name, data in (("manifest.json", text),
                                ("manifest.json.sig", reference.sign(text, self.key)), (name, image)):
            self.files[f"/{RELEASE_REPO}/releases/download/{tag}/{file_name}"] = data
        self.items.append({
            "tag_name": tag, "draft": False, "prerelease": channel == "beta",
            "html_url": f"https://github.com/{RELEASE_REPO}/releases/tag/{tag}",
            "published_at": "2026-11-11T00:00:00Z",
            "assets": [{"name": file_name,
                        "browser_download_url": f"{{BASE}}/{RELEASE_REPO}/releases/download/{tag}/{file_name}"}
                       for file_name in ("manifest.json", "manifest.json.sig", name)]})

    def public_keys(self):
        reference = self.reference
        return {"description": "Throwaway keys of the qualification run.", "keys": {
            "current": base64.b64encode(reference.public_raw(self.key.public_key())).decode(),
            "next": base64.b64encode(reference.public_raw(self.next_key.public_key())).decode()}}

    def start(self):
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()
        check(self._ready.wait(15), "The fake release server did not start")

    def _run(self):
        async def serve():
            async def releases(_request):
                base = f"http://host.docker.internal:{self.port}"
                return aiohttp.web.json_response(json.loads(json.dumps(self.items).replace("{BASE}", base)))

            async def download(request):
                data = self.files.get(request.path)
                return aiohttp.web.Response(body=data) if data is not None else aiohttp.web.Response(status=404)

            app = aiohttp.web.Application()
            app.router.add_get(f"/api/repos/{RELEASE_REPO}/releases", releases)
            app.router.add_get("/{tail:.*}", download)
            runner = aiohttp.web.AppRunner(app)
            await runner.setup()
            # All interfaces: the container reaches the host through host.docker.internal.
            site = aiohttp.web.TCPSite(runner, "0.0.0.0", 0)
            await site.start()
            self.port = site._server.sockets[0].getsockname()[1]
            self._loop = asyncio.get_running_loop()
            self._stop = asyncio.Event()
            self._ready.set()
            await self._stop.wait()
            await runner.cleanup()

        asyncio.run(serve())

    def stop(self):
        if self._loop is not None and self._thread is not None:
            self._loop.call_soon_threadsafe(self._stop.set)
            self._thread.join(10)


def patched_components(directory):
    """A copy of custom_components for the container that trusts the run's keys and fetches
    releases from the fake server. Fails if the constants it replaces moved."""
    target = directory / "components" / "custom_components"
    shutil.copytree(ROOT / "custom_components", target,
                    ignore=shutil.ignore_patterns("__pycache__"))
    firmware = target / "njorun" / "firmware.py"
    text = firmware.read_text()
    for old, new in (
        ('API_URL = "https://api.github.com"',
         f'API_URL = "http://host.docker.internal:{RELEASES.port}/api"'),
        ('DOWNLOAD_PREFIX = "https://github.com/{repo}/releases/download/"',
         f'DOWNLOAD_PREFIX = "http://host.docker.internal:{RELEASES.port}/{{repo}}/releases/download/"'),
    ):
        check(old in text, "The release URL constants of the integration changed")
        text = text.replace(old, new)
    firmware.write_text(text)
    (target / "njorun" / "release-keys.json").write_text(json.dumps(RELEASES.public_keys()))
    return target


async def verify_updates(base_url, username, password, binary, directory, admin):
    """Decision D7: the update entity against real HA, the fake release server and the simulator.

    HA discovers releases, verifies them against the run's keys, caches the image, sends the
    install command on the Noise session and reports progress. The simulator plays the tablet's
    side of the install (it does not download), so the endpoint is checked for its refusals only;
    its content, ranges and token life are covered by tests/integration/test_firmware.py.
    """
    state_dir = directory / "updates"
    state_dir.mkdir()
    identity_path = state_dir / "identity.json"
    result = subprocess.run([str(binary), "identity", str(identity_path)], capture_output=True, timeout=15)
    check(result.returncode == 0, "Rust identity generation failed")
    identity = json.loads(identity_path.read_text())
    listing = await admin.request({"type": "njorun/list"})
    board, revision, layout = RELEASE_BOARD
    binding = await admin.request({
        "type": "njorun/enroll", "tablet_id": "upd", "name": "Updates", "profile_id": "shared",
        "tablet_public_key": identity["public_key"], "expected_revision": listing["revision"],
        "capabilities": {"orientations": ["portrait", "landscape"], "brightness": True,
                         "live": ["wake", "backlight", "telemetry"],
                         "ota": {"board": board, "hardware_revision": revision,
                                 "partition_layout": layout}}})
    await admin.configure("set_overrides", tablet_id="upd", overrides={"update_policy": "manual"})
    tokens, client_id = device_credentials(base_url, username, password, "upd")
    credentials_path = state_dir / "credentials.json"
    private_json(credentials_path, {
        "ha_url": base_url, "access_token": tokens["access_token"],
        "refresh_token": tokens["refresh_token"], "client_id": client_id, "allow_http": True,
        "tablet_id": "upd", "binding_id": binding["binding_id"],
        "tablet_private_key": identity["private_key"], "ha_public_key": binding["ha_public_key"]})
    simulator = Simulator(binary, credentials_path, state_dir)
    simulator.start()
    entity = await entity_id_for_unique_id(admin, "upd_firmware_update")

    async def service(domain, name, success=True, **data):
        return await admin.request(
            {"type": "call_service", "domain": domain, "service": name,
             "target": {"entity_id": entity}, "service_data": data}, success=success)

    async def attributes():
        return (await entity_state(admin, entity)).get("attributes", {})

    await wait_for_state(admin, entity, lambda state: state["attributes"].get("installed_version"),
                         "the running version from telemetry")
    initial = (await attributes())["installed_version"]
    await service("homeassistant", "update_entity")
    offered = await wait_for_state(
        admin, entity, lambda state: state["attributes"].get("latest_version") == "9.0.0",
        "the newest stable release", timeout=40)
    check(offered["state"] == "on", "HA did not offer the update")
    check(offered["attributes"].get("supported_features") == 7,
          "The update entity does not support install, progress and a specific version")
    check(not offered["attributes"].get("in_progress"), "An install was reported before any was started")

    # The endpoint refuses everything without the token of an open install.
    async with aiohttp.ClientSession() as http:
        for headers in ({}, {"Authorization": "Bearer " + secrets.token_urlsafe(32)}):
            async with http.get(f"{base_url}/api/njorun/firmware/9.0.0/app.bin",
                                headers=headers) as response:
                check(response.status == 401, "The firmware endpoint answered without a valid token")
    stage("update_offered", "passed", installed=initial, latest="9.0.0")

    # A release that does not fit is refused before anything reaches the tablet.
    error = await service("update", "install", success=False, version="7.7.7")
    check("does not fit" in error.get("message", ""), f"HA offered an unknown release: {error}")

    # Install: progress is visible, the tablet reports the version and the entity settles.
    await service("update", "install")
    seen = set()
    deadline = time.monotonic() + 40
    while time.monotonic() < deadline:
        state = await entity_state(admin, entity)
        if state["attributes"].get("in_progress"):
            seen.add(state["attributes"].get("install_stage"))
        if state["attributes"].get("installed_version") == "9.0.0" and state["state"] == "off":
            break
        await asyncio.sleep(0.25)
    else:
        raise Failure("The update did not finish")
    check(seen, "The update entity never showed an install in progress")
    status = (await admin.request({"type": "njorun/install_status", "tablet_id": "upd"}))["install"]
    check(status["state"] == "installed" and status["version"] == "9.0.0", "The install did not end as installed")
    stage("update_installed", "passed", stages=sorted(s for s in seen if s))

    # The explicit downgrade flag is the only way back to an older release.
    error = await service("update", "install", success=False, version="8.0.0")
    check("allow_downgrade" in error.get("message", ""), f"HA did not refuse the downgrade: {error}")
    check(not (await attributes()).get("in_progress"), "A refused downgrade reached the tablet")
    await service("njorun", "install_firmware", version="8.0.0", allow_downgrade=True)
    await wait_for_state(
        admin, entity, lambda state: state["attributes"].get("installed_version") == "8.0.0"
        and not state["attributes"].get("in_progress"), "the downgraded version", timeout=40)
    stage("update_downgrade_flag", "passed")

    # The channel setting selects the release stream.
    await admin.configure("set_overrides", tablet_id="upd",
                          overrides={"update_policy": "manual", "update_channel": "beta"})
    await wait_for_state(
        admin, entity, lambda state: state["attributes"].get("latest_version") == "9.1.0-beta.1",
        "the newest beta release")
    stage("update_channel", "passed")
    # The simulator is stopped, and its shutdown checked, with all others at the end of the run.



async def verify_install(base_url, username, password, binary, directory, admin):
    """Decision D7: the install protocol against real HA with the simulator's scripted flow.

    The simulator runs the real checks and then plays the flow that the pre-release part of the
    version asks for (see crates/simulator/src/ota.rs). HA itself never sends an expired or
    repeated install or one while another is open, so `expired`, `duplicate`, `busy` and
    `malformed` are scripted answers here; this stage tests how HA handles each of them.
    """
    state_dir = directory / "install"
    state_dir.mkdir()
    identity_path = state_dir / "identity.json"
    result = subprocess.run([str(binary), "identity", str(identity_path)], capture_output=True, timeout=15)
    check(result.returncode == 0, "Rust identity generation failed")
    identity = json.loads(identity_path.read_text())
    listing = await admin.request({"type": "njorun/list"})
    binding = await admin.request({
        "type": "njorun/enroll", "tablet_id": "ota", "name": "Install", "profile_id": "shared",
        "tablet_public_key": identity["public_key"], "expected_revision": listing["revision"],
        "capabilities": {"orientations": ["portrait", "landscape"], "brightness": True,
                         "live": ["wake", "backlight", "telemetry"], "ota": {"board": "simulator"}}})
    tokens, client_id = device_credentials(base_url, username, password, "ota")
    credentials_path = state_dir / "credentials.json"
    private_json(credentials_path, {
        "ha_url": base_url, "access_token": tokens["access_token"],
        "refresh_token": tokens["refresh_token"], "client_id": client_id, "allow_http": True,
        "tablet_id": "ota", "binding_id": binding["binding_id"],
        "tablet_private_key": identity["private_key"], "ha_public_key": binding["ha_public_key"]})
    simulator = Simulator(binary, credentials_path, state_dir)
    simulator.start()
    firmware = await entity_id_for_unique_id(admin, "ota_firmware_version")
    initial = await wait_for_state(
        admin, firmware, lambda state: state["state"] not in ("unavailable", "unknown"), "a firmware version")

    image = {"channel": "stable", "size": 1048576, "sha256": "0123456789abcdef" * 4,
             "url": f"{base_url}/api/njorun/firmware/qualify"}

    async def install(version, *, success=True, **changes):
        return await admin.request(
            {"type": "njorun/install", "tablet_id": "ota", "version": version,
             "token": secrets.token_urlsafe(32), **{**image, **changes}}, success=success)

    async def status():
        return (await admin.request({"type": "njorun/install_status", "tablet_id": "ota"}))["install"]

    async def final(install_id, description, timeout=40):
        deadline = time.monotonic() + timeout
        seen = []
        while time.monotonic() < deadline:
            current = await status()
            check(current is not None and current["install_id"] == install_id, "The install status changed")
            if current["stage"] and (not seen or seen[-1] != current["stage"]):
                seen.append(current["stage"])
            if current["state"] not in ("sent", "accepted"):
                return current, seen
            await asyncio.sleep(0.25)
        raise Failure(f"The install did not finish: {description}")

    check(await status() is None, "An install was reported before any was sent")

    # Accepted, progress through the stages, installed; a second install waits for the first.
    started = await install("9.0.0")
    refused = await install("9.0.5", success=False)
    check("already open" in refused.get("message", ""), "HA allowed a second open install")
    done, seen = await final(started["install_id"], "installed")
    check(done["state"] == "installed" and done["reported_version"] == "9.0.0", "The install did not end as installed")
    check(seen and seen[-1] in ("write", "reboot") and seen == sorted(seen, key=("download", "verify", "write", "reboot").index),
          f"Progress stages were not in order: {seen}")
    await wait_for_state(admin, firmware, lambda state: state["state"] == "9.0.0", "the installed firmware version")
    stage("install_installed", "passed", stages=seen, from_version=initial["state"])

    # The new boot is not confirmed, or the image fails its checks: rolled_back, version unchanged.
    for version, label in (("9.0.1-rollback", "rollback"), ("9.0.2-fail", "fail")):
        started = await install(version)
        done, _ = await final(started["install_id"], label)
        check(done["state"] == "rolled_back" and done["reported_version"] == "9.0.0",
              f"The {label} install did not end as rolled_back with the running version")
        await wait_for_state(admin, firmware, lambda state: state["state"] == "9.0.0", "the unchanged firmware version")
    stage("install_rolled_back", "passed")

    # Every reject reason: HA reports it, keeps nothing as applied and takes the next install.
    cases = [("9.0.3-reject.expired", {}, "expired"), ("9.0.3-reject.duplicate", {}, "duplicate"),
             ("9.0.3-reject.busy", {}, "busy"), ("9.0.3-reject.malformed", {}, "malformed"),
             ("0.0.1", {}, "incompatible"), ("9.0.4", {"size": 8 * 1024 * 1024 + 1}, "too_large")]
    for version, changes, reason in cases:
        error = await install(version, success=False, **changes)
        check(reason in error.get("message", ""), f"HA did not report the {reason} rejection: {error}")
        current = await status()
        check(current["state"] == "rejected" and current["reason"] == reason,
              f"HA did not keep the {reason} rejection as final")
    await wait_for_state(admin, firmware, lambda state: state["state"] == "9.0.0", "no change after rejections")
    stage("install_rejected_reasons", "passed", reasons=[case[2] for case in cases])

    # An install is live state: after the session ends nothing is resent and a new one is possible.
    stopped = await asyncio.to_thread(simulator.stop)
    check(stopped in ("graceful", "already_exited"), "The install simulator did not stop")
    error = await install("9.0.6", success=False)
    check("not connected" in error.get("message", ""), "HA sent an install without a live session")
    stage("install_needs_live_session", "passed")


async def verify_live_commands(base_url, username, password, binary, directory, admin):
    """Decision D6: the wake button, the backlight light and the diagnostic sensors."""
    state_dir = directory / "livecmd"
    state_dir.mkdir()
    identity_path = state_dir / "identity.json"
    result = subprocess.run([str(binary), "identity", str(identity_path)], capture_output=True, timeout=15)
    check(result.returncode == 0, "Rust identity generation failed")
    identity = json.loads(identity_path.read_text())
    listing = await admin.request({"type": "njorun/list"})
    binding = await admin.request({
        "type": "njorun/enroll", "tablet_id": "livecmd", "name": "Live", "profile_id": "shared",
        "tablet_public_key": identity["public_key"], "expected_revision": listing["revision"],
        "capabilities": {"orientations": ["portrait", "landscape"], "brightness": True,
                         "live": ["wake", "backlight", "telemetry"]}})
    tokens, client_id = device_credentials(base_url, username, password, "livecmd")
    credentials_path = state_dir / "credentials.json"
    private_json(credentials_path, {
        "ha_url": base_url, "access_token": tokens["access_token"],
        "refresh_token": tokens["refresh_token"], "client_id": client_id, "allow_http": True,
        "tablet_id": "livecmd", "binding_id": binding["binding_id"],
        "tablet_private_key": identity["private_key"], "ha_public_key": binding["ha_public_key"]})
    simulator = Simulator(binary, credentials_path, state_dir)
    simulator.start()
    entities = {
        key: await entity_id_for_unique_id(admin, f"livecmd_{key}")
        for key in ("wake", "backlight", "firmware_version", "uptime", "wifi_signal",
                    "free_heap", "reset_reason")
    }
    check(entities["wake"].startswith("button.") and entities["backlight"].startswith("light."),
          "Live entities have the wrong domains")
    registry = await admin.request({"type": "config/entity_registry/list"})
    for key in ("firmware_version", "uptime", "wifi_signal", "free_heap", "reset_reason"):
        entry = next(item for item in registry if item["entity_id"] == entities[key])
        check(entry.get("entity_category") == "diagnostic", f"{key} is not a diagnostic entity")

    light = await wait_for_state(
        admin, entities["backlight"], lambda state: state["state"] == "on", "on after the session")
    setting = effective_brightness(await admin.request({"type": "njorun/list"}), "livecmd")
    check(light["attributes"].get("brightness") == round(setting * 255 / 100),
          "The light does not show the brightness setting")
    version = await wait_for_state(
        admin, entities["firmware_version"], lambda state: state["state"] not in ("unavailable", "unknown"),
        "a firmware version")
    reset = await entity_state(admin, entities["reset_reason"])
    check(reset["state"] == "power_on", "The reset reason was not reported")
    stage("live_entities", "passed", firmware_version=version["state"])

    async def call(domain, service, entity_id, **data):
        await admin.request({"type": "call_service", "domain": domain, "service": service,
                             "target": {"entity_id": entity_id}, "service_data": data})

    before = await admin.request({"type": "njorun/list"})
    await call("light", "turn_off", entities["backlight"])
    await wait_for_state(admin, entities["backlight"], lambda state: state["state"] == "off", "off")
    await call("light", "turn_on", entities["backlight"], brightness=128)
    light = await wait_for_state(
        admin, entities["backlight"],
        lambda state: state["state"] == "on" and abs(state["attributes"].get("brightness", 0) - 128) <= 2,
        "on at about half")
    await call("button", "press", entities["wake"])
    after = await admin.request({"type": "njorun/list"})
    check(after["revision"] == before["revision"], "A live command changed the configuration revision")
    number = await entity_state(admin, await entity_id_for_unique_id(admin, "livecmd_brightness"))
    check(float(number["state"]) == setting, "A live command changed the brightness setting")
    stage("live_commands", "passed")

    # A newly applied configuration returns the display to the persistent setting.
    await admin.configure("set_overrides", tablet_id="livecmd", overrides={"brightness": 60})
    await wait_for_state(
        admin, entities["backlight"],
        lambda state: state["state"] == "on" and state["attributes"].get("brightness") == 153,
        "the 60 % setting after a new configuration")
    stage("live_backlight_separation", "passed")

    stopped = await asyncio.to_thread(simulator.stop)
    check(stopped in ("graceful", "already_exited"), "The live simulator did not stop")
    for key, entity_id in entities.items():
        await wait_for_state(admin, entity_id, lambda state: state["state"] == "unavailable",
                             "unavailable without a session")
    stage("live_unavailable_without_session", "passed")


async def verify_capability_refresh(base_url, username, password, binary, directory, admin):
    """Decision D8: a tablet reports its capabilities when its session is confirmed.

    A tablet approved before D6 has no `live` capability stored. The simulator reports new
    ones in its confirmation (`capabilities` in its credentials file): HA stores them, bumps
    the revision and creates the live entities. Capabilities that would break a setting are
    refused with a repair issue and the old ones stay; a later fitting report clears it.
    """
    old = {"orientations": ["portrait", "landscape"], "brightness": True}
    live = {**old, "live": ["wake", "backlight", "telemetry"]}

    async def start(tablet_id, name, stored, reported, binding=None):
        state_dir = directory / f"refresh-{tablet_id}"
        if binding is None:
            state_dir.mkdir()
            identity_path = state_dir / "identity.json"
            result = subprocess.run([str(binary), "identity", str(identity_path)],
                                    capture_output=True, timeout=15)
            check(result.returncode == 0, "Rust identity generation failed")
            identity = json.loads(identity_path.read_text())
            listing = await admin.request({"type": "njorun/list"})
            enrolled = await admin.request({
                "type": "njorun/enroll", "tablet_id": tablet_id, "name": name,
                "profile_id": "shared", "tablet_public_key": identity["public_key"],
                "expected_revision": listing["revision"], "capabilities": stored})
            binding = {"binding_id": enrolled["binding_id"], "ha_public_key": enrolled["ha_public_key"],
                       "tablet_private_key": identity["private_key"]}
        return state_dir, binding

    def launch(state_dir, tablet_id, binding, reported):
        tokens, client_id = device_credentials(base_url, username, password, tablet_id)
        credentials_path = state_dir / "credentials.json"
        credentials_path.unlink(missing_ok=True)
        private_json(credentials_path, {
            "ha_url": base_url, "access_token": tokens["access_token"],
            "refresh_token": tokens["refresh_token"], "client_id": client_id, "allow_http": True,
            "tablet_id": tablet_id, "binding_id": binding["binding_id"],
            "tablet_private_key": binding["tablet_private_key"],
            "ha_public_key": binding["ha_public_key"], "capabilities": reported})
        simulator = Simulator(binary, credentials_path, state_dir)
        simulator.start()
        return simulator

    async def unique_ids():
        registry = await admin.request({"type": "config/entity_registry/list"})
        return {entry.get("unique_id") for entry in registry}

    async def issues():
        found = await admin.request({"type": "repairs/list_issues"})
        return {item["issue_id"] for item in found["issues"] if item["domain"] == "njorun"}

    # Accepted: enrolled without `live`, reports it on the next session.
    state_dir, binding = await start("refresh", "Refresh", old, live)
    before = await admin.request({"type": "njorun/list"})
    check("live" not in before["tablets"]["refresh"]["capabilities"], "The tablet was enrolled with live")
    simulator = launch(state_dir, "refresh", binding, live)
    wake = await entity_id_for_unique_id(admin, "refresh_wake")
    backlight = await entity_id_for_unique_id(admin, "refresh_backlight")
    uptime = await entity_id_for_unique_id(admin, "refresh_uptime")
    check(wake.startswith("button.") and backlight.startswith("light."), "Refreshed entities have the wrong domains")
    await wait_for_state(admin, backlight, lambda state: state["state"] == "on", "on after the refreshed session")
    await wait_for_state(admin, uptime, lambda state: state["state"] not in ("unavailable", "unknown"), "uptime")
    after = await admin.request({"type": "njorun/list"})
    check(after["tablets"]["refresh"]["capabilities"] == live, "HA did not store the reported capabilities")
    check(after["revision"] > before["revision"]
          and after["tablets"]["refresh"]["revision"] > before["tablets"]["refresh"]["revision"],
          "Refreshing the capabilities did not bump the revision")
    check("capabilities_refresh" not in await issues(), "A repair issue was raised for valid capabilities")
    stage("capability_refresh", "passed", revision=after["revision"])

    # Refused: the tablet's own orientation override is not among the reported orientations.
    stored = {**live, "live": ["wake"]}
    state_dir, binding = await start("refuse", "Refuse", stored, None)
    await admin.configure("set_overrides", tablet_id="refuse", overrides={"orientation": "landscape"})
    before = await admin.request({"type": "njorun/list"})
    narrow = {"orientations": ["portrait"], "brightness": True, "live": ["wake"]}
    refuse = launch(state_dir, "refuse", binding, narrow)
    wake_refuse = await entity_id_for_unique_id(admin, "refuse_wake")
    await wait_for_state(admin, wake_refuse, lambda state: state["state"] != "unavailable",
                         "a live session although the capabilities were refused")
    deadline = time.monotonic() + 20
    while "capabilities_refuse" not in await issues():
        check(time.monotonic() < deadline, "HA raised no repair issue for refused capabilities")
        await asyncio.sleep(0.3)
    kept = await admin.request({"type": "njorun/list"})
    check(kept["tablets"]["refuse"]["capabilities"] == stored, "HA replaced capabilities that break a setting")
    check(kept["revision"] == before["revision"], "A refused refresh changed the revision")
    stage("capability_refresh_refused", "passed")

    # Fixed by the administrator: the next report fits, the issue clears and the dropped part goes.
    await admin.configure("set_overrides", tablet_id="refuse", overrides={"orientation": "portrait"})
    stopped = await asyncio.to_thread(refuse.stop)
    check(stopped in ("graceful", "already_exited"), "The refused simulator did not stop")
    fitting = {"orientations": ["portrait"], "brightness": True, "live": []}
    refuse = launch(state_dir, "refuse", binding, fitting)
    deadline = time.monotonic() + 25
    while True:
        current = await admin.request({"type": "njorun/list"})
        if current["tablets"]["refuse"]["capabilities"] == fitting:
            break
        check(time.monotonic() < deadline, "HA did not store the fitting capabilities")
        await asyncio.sleep(0.3)
    check("capabilities_refuse" not in await issues(), "The repair issue stayed after a fitting report")
    deadline = time.monotonic() + 20
    while "refuse_wake" in await unique_ids():
        check(time.monotonic() < deadline, "The wake button stayed after its capability went")
        await asyncio.sleep(0.3)
    stage("capability_refresh_cleared", "passed")

    for simulator_to_stop in (simulator, refuse):
        stopped = await asyncio.to_thread(simulator_to_stop.stop)
        check(stopped in ("graceful", "already_exited"), "A capability simulator did not stop")


async def verify_discovery_ux(base_url, access_token, entry_id, admin):
    """Approve a waiting tablet on its Discovered card and manage it on its device page."""
    announced = await admin.request({
        "type": "njorun/announce", "protocol": 1, "tablet_id": "disco", "name": "Hallway",
        "tablet_public_key": base64.b64encode(bytes(range(32))).decode(),
        "capabilities": {"orientations": ["portrait", "landscape"], "brightness": True},
    })
    check(announced["status"] == "pending", "Announcement was not accepted")
    flow = None
    flows = []
    for _ in range(20):
        flows = await admin.request({"type": "config_entries/flow/progress"})
        flow = next(
            (item for item in flows
             if item.get("handler") == "njorun"
             and item["context"].get("source") == "integration_discovery"
             and item["context"].get("unique_id") == "disco"),
            None,
        )
        if flow:
            break
        await asyncio.sleep(0.5)
    check(flow is not None, f"A waiting tablet did not appear as a Discovered card: {flows}")
    status, form = probe.http_request(
        base_url, f"/api/config/config_entries/flow/{flow['flow_id']}", bearer=access_token
    )
    code = announced["code"]
    check(
        status == 200 and form.get("step_id") == "confirm"
        and form["description_placeholders"]["code"] == f"{code[:3]} {code[3:]}"
        and "profile_id" in [field["name"] for field in form["data_schema"]],
        "The Discovered card does not show the code and the profile choice",
    )
    status, done = probe.post_json(
        base_url, f"/api/config/config_entries/flow/{flow['flow_id']}",
        {"profile_id": "shared"}, access_token,
    )
    check(
        status == 200 and done.get("type") == "abort" and done.get("reason") == "tablet_approved",
        "Confirming the Discovered card did not approve the tablet",
    )
    snapshot = await admin.request({"type": "njorun/list"})
    check(
        snapshot["tablets"].get("disco", {}).get("profile_id") == "shared",
        "The approved tablet is not in the chosen profile",
    )
    profile_entity = await entity_id_for_unique_id(admin, "disco_profile")
    button_entity = await entity_id_for_unique_id(admin, "disco_use_profile_values")
    await wait_for_entity_state(admin, profile_entity, "Shared")
    await admin.request({
        "type": "call_service", "domain": "select", "service": "select_option",
        "target": {"entity_id": profile_entity}, "service_data": {"option": "Other"},
    })
    await wait_for_entity_state(admin, profile_entity, "Other")
    snapshot = await admin.request({"type": "njorun/list"})
    check(snapshot["tablets"]["disco"]["profile_id"] == "other", "Profile select did not move the tablet")
    await admin.configure("set_overrides", tablet_id="disco", overrides={"brightness": 12})
    await admin.request({
        "type": "call_service", "domain": "button", "service": "press",
        "target": {"entity_id": button_entity}, "service_data": {},
    })
    for _ in range(20):
        snapshot = await admin.request({"type": "njorun/list"})
        if not snapshot["tablets"]["disco"]["overrides"]:
            break
        await asyncio.sleep(0.5)
    check(not snapshot["tablets"]["disco"]["overrides"], "Use profile values did not clear the overrides")
    devices = await admin.request({"type": "config/device_registry/list"})
    device = next(
        (item for item in devices if ["njorun", "disco"] in [list(i) for i in item["identifiers"]]),
        None,
    )
    check(device is not None, "The approved tablet has no device")
    await admin.request({
        "type": "config/device_registry/remove_config_entry",
        "device_id": device["id"], "config_entry_id": entry_id,
    })
    snapshot = await admin.request({"type": "njorun/list"})
    check("disco" not in snapshot["tablets"], "Deleting the device did not remove the tablet")
    stage("discovery_ux", "passed", tablet="disco")


def edit_profile_via_options_flow(base_url, access_token, entry_id, profile):
    """Submit a real HA-native options flow and return its final response."""
    status, flow = probe.post_json(
        base_url,
        "/api/config/config_entries/options/flow",
        {"handler": entry_id},
        access_token,
    )
    check(status == 200 and flow.get("type") == "menu", "Options flow did not open as a menu")
    check(
        "edit_profile" in flow.get("menu_options", []),
        "Options menu does not offer editing a profile",
    )
    flow_id = flow["flow_id"]
    status, flow = probe.post_json(
        base_url,
        f"/api/config/config_entries/options/flow/{flow_id}",
        {"next_step_id": "edit_profile"},
        access_token,
    )
    check(
        status == 200
        and flow.get("type") == "form"
        and flow.get("step_id") == "edit_profile",
        "Options flow did not show the profile selector",
    )
    status, flow = probe.post_json(
        base_url,
        f"/api/config/config_entries/options/flow/{flow_id}",
        {"profile_id": "shared"},
        access_token,
    )
    check(
        status == 200
        and flow.get("type") == "form"
        and flow.get("step_id") == "edit_profile_settings",
        "Options flow did not show native profile settings",
    )
    values = {
        key: value
        for key, value in profile.items()
        if key not in {"favorites", "room_order", "entity_order", "excluded_entities"}
    }
    values.update({"brightness": 55, "theme": "dark"})
    advanced = {
        key: profile[key]
        for key in ("favorites", "room_order", "entity_order", "excluded_entities")
        if key in profile
    }
    values["advanced_settings_json"] = json.dumps(advanced, separators=(",", ":"))
    status, result = probe.post_json(
        base_url,
        f"/api/config/config_entries/options/flow/{flow_id}",
        values,
        access_token,
    )
    check(
        status == 200 and result.get("type") == "create_entry",
        "HA rejected the native profile options form",
    )
    return result


def verify_private_store(container_name):
    """Require HA's runtime store containing Noise keys to stay private."""
    files = probe.docker(
        "exec",
        container_name,
        "find",
        "/config/.storage",
        "-maxdepth",
        "1",
        "-type",
        "f",
        "-name",
        "njorun.*",
    ).splitlines()
    check(len(files) == 1, "Njörun private store was not found")
    mode = probe.docker("exec", container_name, "stat", "-c", "%a", files[0])
    check(mode == "600", "Njörun private store permissions were not 0600")
    return mode


async def run_live_ui(binary, credentials_path, state_directory, output_path, entity_id, room_id):
    arguments = [
        str(binary),
        "--credentials",
        str(credentials_path),
        "--state-dir",
        str(state_directory),
        "--output",
        str(output_path),
        "--entity",
        entity_id,
        "--room",
        room_id,
        "--timeout-seconds",
        "120",
    ]
    process = await asyncio.create_subprocess_exec(
        *arguments,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    try:
        stdout, _stderr = await asyncio.wait_for(process.communicate(), timeout=140)
    except asyncio.TimeoutError as error:
        process.kill()
        await process.wait()
        raise Failure("Live Slint UI qualification timed out") from error
    except asyncio.CancelledError:
        if process.returncode is None:
            process.kill()
            await process.wait()
        raise
    check(process.returncode == 0, "Live Slint UI qualification failed")
    try:
        report = json.loads(stdout.decode().strip().splitlines()[-1])
    except (IndexError, json.JSONDecodeError) as error:
        raise Failure("Live Slint UI did not return its result report") from error
    report_path = Path(output_path).with_suffix(".json")
    try:
        saved_report = json.loads(report_path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise Failure("Live Slint UI did not save its result report") from error
    check(
        saved_report == report
        and report.get("status") == "ok"
        and report.get("entity_id") == entity_id
        and report.get("room_id") == room_id
        and report.get("state_before") in {"on", "off"}
        and report.get("state_after") in {"on", "off"}
        and report.get("state_before") != report.get("state_after")
        and Path(output_path).is_file(),
        "Live Slint UI report did not confirm a real state change",
    )
    return report


class Simulator:
    def __init__(self, binary, credentials, state_dir):
        self.binary, self.credentials, self.state_dir = binary, credentials, state_dir
        self.process = None
        self.events = queue.Queue()
        self.latest = {}
        SIMULATORS.append(self)

    def start(self):
        self.events = queue.Queue()
        self.latest = {}
        self.forced_shutdown = False
        self.process = subprocess.Popen(
            [str(self.binary), "run", "--headless", "--credentials", str(self.credentials),
             "--state-dir", str(self.state_dir)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True, bufsize=1)
        def read():
            for line in self.process.stdout:
                try:
                    self.events.put(json.loads(line))
                except ValueError:
                    self.events.put({"event": "invalid"})
        threading.Thread(target=read, daemon=True).start()

    async def until(self, predicate, description, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            while True:
                try:
                    event = self.events.get_nowait()
                except queue.Empty:
                    break
                self.latest[event.get("event")] = event.get("payload")
            if predicate(self.latest):
                return
            check(self.process.poll() is None, "Simulator exited during " + description)
            await asyncio.sleep(0.1)
        dashboard = self.latest.get("dashboard") or {}
        settings = self.latest.get("settings") or {}
        raise Failure(
            "Simulator timeout: "
            + description
            + "; connection="
            + str(dashboard.get("connection"))
            + "; status="
            + str(self.latest.get("status"))
            + "; revision="
            + str(settings.get("revision"))
        )

    def send(self, command):
        self.process.stdin.write(json.dumps(command) + "\n")
        self.process.stdin.flush()

    def stop(self):
        if self.process is None:
            return "not_started"
        if self.process.poll() is not None:
            return "already_exited" if self.process.returncode == 0 else "failed"
        try:
            self.send({"type": "stop"})
        except BrokenPipeError:
            self.process.wait(timeout=20)
            return "already_exited" if self.process.returncode == 0 else "failed"
        try:
            self.process.wait(timeout=20)
            return "graceful" if self.process.returncode == 0 else "failed"
        except subprocess.TimeoutExpired:
            self.forced_shutdown = True
            self.process.kill()
            self.process.wait(timeout=5)
            return "forced"


def tile(state, entity_id):
    dashboard = state.get("dashboard") or {}
    return next((entity for room in dashboard.get("rooms", []) for entity in room["entities"]
                 if entity["entity_id"] == entity_id), None)


async def exercise(
    base_url, directory, binary, live_ui_binary, container_name, host_port
):
    tokens, username, password = bootstrap(base_url)
    stage("onboarding", "passed")
    core_state = await wait_for_core_running(base_url, tokens["access_token"])
    stage("ha_core_state", "passed", state=core_state)
    status, states = probe.http_request(
        base_url, "/api/states", bearer=tokens["access_token"]
    )
    check(status == 200 and isinstance(states, list), "Home Assistant states unavailable")
    loaded_entities = {item.get("entity_id") for item in states}
    check(
        "input_boolean.njorun_test_switch" in loaded_entities,
        "Template light fixture input_boolean did not load",
    )
    status, api_config = probe.http_request(
        base_url, "/api/config", bearer=tokens["access_token"]
    )
    components = api_config.get("components", []) if isinstance(api_config, dict) else []
    check(
        status == 200 and "template" in components,
        "Home Assistant template component did not load",
    )
    stage("fixture_components", "passed", components=["input_boolean", "template"])
    status, flow = probe.post_json(base_url, "/api/config/config_entries/flow", {"handler": "njorun"}, tokens["access_token"])
    check(status == 200, "Njörun config flow did not start")
    check(
        flow.get("type") == "create_entry",
        "Njörun setup must finish in one step without asking questions",
    )
    status, entries = probe.http_request(
        base_url,
        "/api/config/config_entries/entry?domain=njorun",
        bearer=tokens["access_token"],
    )
    check(
        status == 200
        and isinstance(entries, list)
        and len(entries) == 1
        and entries[0].get("domain") == "njorun",
        "Njörun config entry could not be found for its options flow",
    )
    entry_id = entries[0]["entry_id"]
    simulators = []
    async with aiohttp.ClientSession() as http:
        async with http.ws_connect(base_url.replace("http://", "ws://") + "/api/websocket") as socket:
            check((await socket.receive_json())["type"] == "auth_required", "HA did not request authentication")
            await socket.send_json({"type": "auth", "access_token": tokens["access_token"]})
            check((await socket.receive_json())["type"] == "auth_ok", "Admin authentication failed")
            admin = Admin(socket)
            await admin.configure("add_profile", profile_id="shared", name="Shared", settings={})
            await admin.configure("add_profile", profile_id="other", name="Other", settings={"brightness": 40})
            areas = await admin.request({"type": "config/area_registry/list"})
            area = next((item for item in areas if item.get("name", "").casefold() == "living room"), None)
            if area is None:
                area = await admin.request({"type": "config/area_registry/create", "name": "Living room"})
            lamp = None
            deadline = time.monotonic() + 20
            while lamp is None and time.monotonic() < deadline:
                registry = await admin.request({"type": "config/entity_registry/list"})
                lamp = next((entry["entity_id"] for entry in registry if entry.get("unique_id") == "njorun_test_light"), None)
                if lamp is None:
                    await asyncio.sleep(0.5)
            check(lamp is not None, "Home Assistant template light did not register")
            stage("fixture_template_light", "passed", entity_id=lamp)
            await admin.request({"type": "config/entity_registry/update", "entity_id": lamp, "area_id": area["area_id"]})
            applied_entities = {}
            for tablet_id, profile_id in (("one", "shared"), ("two", "shared"), ("three", "other")):
                state_dir = directory / tablet_id
                state_dir.mkdir()
                identity_path = state_dir / "identity.json"
                result = subprocess.run([str(binary), "identity", str(identity_path)], capture_output=True, timeout=15)
                check(result.returncode == 0, "Rust identity generation failed")
                identity = json.loads(identity_path.read_text())
                state = await admin.request({"type": "njorun/list"})
                binding = await admin.request({"type": "njorun/enroll", "tablet_id": tablet_id,
                    "name": tablet_id.title(), "profile_id": profile_id,
                    "tablet_public_key": identity["public_key"], "expected_revision": state["revision"],
                    "capabilities": {"orientations": ["portrait", "landscape"], "brightness": True}})
                device_tokens, client_id = device_credentials(base_url, username, password, tablet_id)
                credentials_path = state_dir / "credentials.json"
                private_json(credentials_path, {"ha_url": base_url, "access_token": device_tokens["access_token"],
                    "refresh_token": device_tokens["refresh_token"], "client_id": client_id, "allow_http": True,
                    "tablet_id": tablet_id, "binding_id": binding["binding_id"],
                    "tablet_private_key": identity["private_key"], "ha_public_key": binding["ha_public_key"]})
                simulator = Simulator(binary, credentials_path, state_dir)
                simulator.start()
                simulators.append(simulator)
            try:
                initial_snapshot = await admin.request({"type": "njorun/list"})
                initial_brightness = {"one": 80, "two": 80, "three": 40}
                for tablet_id, simulator in zip(("one", "two", "three"), simulators):
                    expected_revision = initial_snapshot["tablets"][tablet_id]["revision"]
                    expected_brightness = effective_brightness(initial_snapshot, tablet_id)
                    check(
                        expected_revision > 0
                        and expected_brightness == initial_brightness[tablet_id],
                        "Initial server configuration differed from the fixture",
                    )
                    await simulator.until(
                        lambda state, revision=expected_revision, brightness=expected_brightness:
                            (state.get("dashboard") or {}).get("connection") == "ready"
                            and (state.get("settings") or {}).get("revision") == revision
                            and (state.get("settings") or {}).get("settings", {}).get("brightness") == brightness,
                        "initial per-tablet configuration revision",
                    )
                    applied_entities[tablet_id] = await wait_for_applied_brightness(
                        admin, tablet_id, expected_brightness
                    )
                    stage(
                        f"initial_ack_{tablet_id}",
                        "passed",
                        revision=expected_revision,
                        brightness=expected_brightness,
                    )
                store_mode = verify_private_store(container_name)
                stage("private_store_permissions", "passed", mode=store_mode)
                options_result = edit_profile_via_options_flow(
                    base_url,
                    tokens["access_token"],
                    entry_id,
                    initial_snapshot["profiles"]["shared"]["settings"],
                )
                check(options_result.get("type") == "create_entry", "Options flow did not finish")
                shared_snapshot = await admin.request({"type": "njorun/list"})
                check(
                    shared_snapshot["profiles"]["shared"]["settings"]["brightness"] == 55
                    and shared_snapshot["profiles"]["shared"]["settings"]["theme"] == "dark",
                    "HA options flow did not change the shared profile",
                )
                for tablet_id, simulator in zip(("one", "two"), simulators[:2]):
                    expected_revision = shared_snapshot["tablets"][tablet_id]["revision"]
                    await simulator.until(
                        lambda state, revision=expected_revision:
                            (state.get("settings") or {}).get("revision") == revision
                            and (state.get("settings") or {}).get("settings", {}).get("brightness") == 55
                            and (state.get("settings") or {}).get("settings", {}).get("theme") == "dark",
                        "native options-flow profile propagation",
                    )
                    applied_entities[tablet_id] = await wait_for_applied_brightness(
                        admin, tablet_id, 55
                    )
                theme_entity = await entity_id_for_unique_id(admin, "one_theme")
                await wait_for_entity_state(admin, theme_entity, "dark")
                stage(
                    "native_options_profile_edit",
                    "passed",
                    revision=shared_snapshot["tablets"]["one"]["revision"],
                    brightness=55,
                    theme="dark",
                )
                simulator_three = simulators[2]
                unchanged_revision = initial_snapshot["tablets"]["three"]["revision"]
                check(
                    shared_snapshot["tablets"]["three"]["revision"] == unchanged_revision
                    and effective_brightness(shared_snapshot, "three") == 40,
                    "Other profile desired state changed",
                )
                await simulator_three.until(
                    lambda state:
                        (state.get("settings") or {}).get("revision") == unchanged_revision
                        and (state.get("settings") or {}).get("settings", {}).get("brightness") == 40,
                    "unrelated profile isolation",
                )
                applied_entities["three"] = await wait_for_applied_brightness(
                    admin, "three", 40
                )
                stage(
                    "profile_isolation",
                    "passed",
                    unchanged_revision=unchanged_revision,
                    brightness=40,
                )
                await verify_discovery_ux(base_url, tokens["access_token"], entry_id, admin)
                await admin.configure("set_log_sink", url="https://loki.example.org")
                sink_snapshot = await admin.request({"type": "njorun/list"})
                check(
                    sink_snapshot["log_sink"] == "https://loki.example.org",
                    "The remote logging option was not stored",
                )
                for tablet_id, simulator in zip(("one", "two", "three"), simulators):
                    revision = sink_snapshot["tablets"][tablet_id]["revision"]
                    await simulator.until(
                        lambda state, revision=revision:
                            (state.get("settings") or {}).get("revision") == revision,
                        "remote logging option reaching every tablet",
                    )
                await admin.configure("set_log_sink", url="")
                stage("remote_logging_option", "passed", tablets=3)
                await verify_live_commands(base_url, username, password, binary, directory, admin)
                await verify_install(base_url, username, password, binary, directory, admin)
                await verify_updates(base_url, username, password, binary, directory, admin)
                await verify_capability_refresh(base_url, username, password, binary, directory, admin)
                ui_directory = directory / "live-ui"
                ui_directory.mkdir()
                ui_identity_path = ui_directory / "identity.json"
                ui_identity_result = subprocess.run(
                    [str(binary), "identity", str(ui_identity_path)],
                    capture_output=True,
                    timeout=15,
                )
                check(ui_identity_result.returncode == 0, "Rust identity generation failed")
                ui_identity = json.loads(ui_identity_path.read_text())
                ui_snapshot = await admin.request({"type": "njorun/list"})
                ui_binding = await admin.request(
                    {
                        "type": "njorun/enroll",
                        "tablet_id": "liveui",
                        "name": "Live UI",
                        "profile_id": "other",
                        "tablet_public_key": ui_identity["public_key"],
                        "expected_revision": ui_snapshot["revision"],
                        "capabilities": {
                            "orientations": ["portrait", "landscape"],
                            "brightness": True,
                        },
                    }
                )
                await admin.configure(
                    "set_overrides",
                    tablet_id="liveui",
                    overrides={"brightness": 100},
                )
                ui_desired = await admin.request({"type": "njorun/list"})
                check(
                    ui_desired["tablets"]["liveui"]["overrides"]["brightness"] == 100,
                    "Live UI brightness override was not persisted",
                )
                ui_tokens, ui_client_id = device_credentials(
                    base_url, username, password, "liveui"
                )
                ui_credentials_path = ui_directory / "credentials.json"
                private_json(
                    ui_credentials_path,
                    {
                        "ha_url": base_url,
                        "access_token": ui_tokens["access_token"],
                        "refresh_token": ui_tokens["refresh_token"],
                        "client_id": ui_client_id,
                        "allow_http": True,
                        "tablet_id": "liveui",
                        "binding_id": ui_binding["binding_id"],
                        "tablet_private_key": ui_identity["private_key"],
                        "ha_public_key": ui_binding["ha_public_key"],
                    },
                )
                live_ui_output = ROOT / "target" / "qualification" / "live-ui.png"
                live_ui_output.parent.mkdir(parents=True, exist_ok=True)
                ui_task = asyncio.create_task(
                    run_live_ui(
                        live_ui_binary,
                        ui_credentials_path,
                        ui_directory / "client-state",
                        live_ui_output,
                        lamp,
                        area["area_id"],
                    )
                )
                try:
                    await wait_for_applied_brightness(admin, "liveui", 100, timeout=60)
                    ui_report = await ui_task
                except BaseException:
                    if not ui_task.done():
                        ui_task.cancel()
                    try:
                        await ui_task
                    except (asyncio.CancelledError, Exception):
                        pass
                    raise
                live_ui_states = await admin.request({"type": "get_states"})
                live_ui_lamp = next(
                    item.get("state")
                    for item in live_ui_states
                    if item.get("entity_id") == lamp
                )
                check(
                    live_ui_lamp == ui_report["state_after"],
                    "HA state did not independently confirm the native UI action",
                )
                stage(
                    "live_native_ui_action",
                    "passed",
                    entity_id=lamp,
                    room_id=area["area_id"],
                    state_before=ui_report["state_before"],
                    state_after=live_ui_lamp,
                    applied_brightness=100,
                    width=ui_report["width"],
                    height=ui_report["height"],
                    png=str(live_ui_output),
                    report=str(live_ui_output.with_suffix(".json")),
                )
                first = simulators[0]
                await first.until(lambda state: tile(state, lamp) is not None, "real HA room/entity")
                initial = tile(first.latest, lamp)["state"]["state"]
                first.send({"type": "tap", "value": lamp})
                await first.until(lambda state: tile(state, lamp)["state"]["state"] != initial, "real light action")
                await admin.request(
                    {
                        "type": "call_service",
                        "domain": "number",
                        "service": "set_value",
                        "service_data": {
                            "entity_id": applied_entities["one"],
                            "value": 25,
                        },
                    }
                )
                await first.until(
                    lambda state: (state.get("settings") or {}).get("settings", {}).get("brightness") == 25,
                    "native number per-tablet override",
                )
                native_override_snapshot = await admin.request({"type": "njorun/list"})
                check(
                    native_override_snapshot["tablets"]["one"]["overrides"].get("brightness") == 25,
                    "Native number service did not persist the tablet override",
                )
                select_entity = await entity_id_for_unique_id(admin, "one_orientation")
                await admin.request(
                    {
                        "type": "call_service",
                        "domain": "select",
                        "service": "select_option",
                        "service_data": {
                            "entity_id": select_entity,
                            "option": "landscape",
                        },
                    }
                )
                await first.until(
                    lambda state: (state.get("settings") or {}).get("settings", {}).get("orientation") == "landscape",
                    "native select per-tablet override",
                )
                await wait_for_entity_state(admin, select_entity, "landscape")
                stage(
                    "native_number_override",
                    "passed",
                    brightness=25,
                    orientation="landscape",
                )
                first_stop = first.stop()
                check(first_stop == "graceful", "Simulator did not stop gracefully before restart")
                stage("simulator_graceful_stop", "passed")
                await admin.configure("edit_profile", profile_id="shared", patch={"brightness": 65, "language": "de"})
                first.start()
                await first.until(lambda state: (state.get("dashboard") or {}).get("connection") == "ready"
                    and (state.get("settings") or {}).get("settings", {}).get("language") == "de", "restart reconciliation")
                check(first.latest["settings"]["settings"]["brightness"] == 25, "Restart lost override")
                final_snapshot = await admin.request({"type": "njorun/list"})
                expected_settings = {
                    tablet_id: final_snapshot["profiles"][record["profile_id"]]["settings"]
                    | record["overrides"]
                    for tablet_id, record in final_snapshot["tablets"].items()
                }
                for tablet_id, simulator in zip(("one", "two", "three"), simulators):
                    expected_revision = final_snapshot["tablets"][tablet_id]["revision"]
                    expected = expected_settings[tablet_id]
                    await simulator.until(
                        lambda state, revision=expected_revision, settings=expected:
                            (state.get("settings") or {}).get("revision") == revision
                            and (state.get("settings") or {}).get("settings") == settings,
                        "final applied configuration before HA restart",
                    )
                    applied_entities[tablet_id] = await wait_for_applied_brightness(
                        admin, tablet_id, expected["brightness"]
                    )
                lamp_before_ha_disconnect = tile(first.latest, lamp)["state"]["state"]
                probe.docker("stop", "--time", "30", container_name, timeout=60)
                for simulator in simulators:
                    await simulator.until(
                        lambda state: (state.get("dashboard") or {}).get("connection") == "offline",
                        "Home Assistant graceful-stop disconnect",
                    )
                    simulator.latest.pop("status", None)
                first.send({"type": "tap", "value": lamp})
                stage("ha_restart_disconnected", "passed")
                probe.docker("start", container_name)
                port_after_ha_restart = int(
                    probe.docker("port", container_name, "8123/tcp").rsplit(":", 1)[1]
                )
                check(port_after_ha_restart == host_port, "Home Assistant port changed after restart")
                await wait_for_core_running(base_url, tokens["access_token"])
                for tablet_id, simulator in zip(("one", "two", "three"), simulators):
                    expected_revision = final_snapshot["tablets"][tablet_id]["revision"]
                    expected = expected_settings[tablet_id]
                    await simulator.until(
                        lambda state, revision=expected_revision, settings=expected:
                            (state.get("dashboard") or {}).get("connection") == "ready"
                            and state.get("status") == "Connected"
                            and (state.get("settings") or {}).get("revision") == revision
                            and (state.get("settings") or {}).get("settings") == settings,
                        f"management and dashboard recovery after HA restart for {tablet_id}",
                        timeout=90,
                    )
                stage("ha_restart_recovery", "passed", port=port_after_ha_restart)
                async with aiohttp.ClientSession() as reconnect_http:
                    async with reconnect_http.ws_connect(
                        base_url.replace("http://", "ws://") + "/api/websocket"
                    ) as reconnect_socket:
                        check(
                            (await reconnect_socket.receive_json())["type"] == "auth_required",
                            "HA did not request reauthentication after restart",
                        )
                        await reconnect_socket.send_json(
                            {"type": "auth", "access_token": tokens["access_token"]}
                        )
                        check(
                            (await reconnect_socket.receive_json())["type"] == "auth_ok",
                            "Admin reauthentication failed after HA restart",
                        )
                        reconnected_admin = Admin(reconnect_socket)
                        restored = await reconnected_admin.request({"type": "njorun/list"})
                        check(restored == final_snapshot, "HA restart lost Njörun desired configuration")
                        for tablet_id in ("one", "two", "three"):
                            await wait_for_applied_brightness(
                                reconnected_admin,
                                tablet_id,
                                expected_settings[tablet_id]["brightness"],
                            )
                        original_simulator_pids = {
                            simulator.process.pid for simulator in simulators[:3]
                        }
                        disabled = await reconnected_admin.request(
                            {
                                "type": "config_entries/disable",
                                "entry_id": entry_id,
                                "disabled_by": "user",
                            }
                        )
                        check(
                            disabled.get("require_restart") is False,
                            "HA could not unload Njörun without a core restart",
                        )
                        for simulator in simulators[:3]:
                            await simulator.until(
                                lambda state:
                                    (state.get("dashboard") or {}).get("connection") == "ready"
                                    and state.get("status")
                                    == "Dashboard connected; Njörun management is unavailable",
                                "HA integration unload with dashboard retained",
                                timeout=45,
                            )
                        check(
                            original_simulator_pids
                            == {simulator.process.pid for simulator in simulators[:3]}
                            and all(simulator.process.poll() is None for simulator in simulators[:3]),
                            "A simulator restarted while Njörun was unloaded",
                        )
                        stage("integration_unload_dashboard_retained", "passed")
                        enabled = await reconnected_admin.request(
                            {
                                "type": "config_entries/disable",
                                "entry_id": entry_id,
                                "disabled_by": None,
                            }
                        )
                        check(
                            enabled.get("require_restart") is False,
                            "HA could not reload Njörun without a core restart",
                        )
                        for tablet_id, simulator in zip(("one", "two", "three"), simulators):
                            expected_revision = final_snapshot["tablets"][tablet_id]["revision"]
                            expected = expected_settings[tablet_id]
                            await simulator.until(
                                lambda state, revision=expected_revision, settings=expected:
                                    (state.get("dashboard") or {}).get("connection") == "ready"
                                    and state.get("status") == "Connected"
                                    and (state.get("settings") or {}).get("revision") == revision
                                    and (state.get("settings") or {}).get("settings") == settings,
                                "management retry after integration reload",
                                timeout=60,
                            )
                            await wait_for_applied_brightness(
                                reconnected_admin,
                                tablet_id,
                                expected["brightness"],
                                timeout=30,
                            )
                        check(
                            original_simulator_pids
                            == {simulator.process.pid for simulator in simulators[:3]},
                            "A simulator restarted before management recovered",
                        )
                        stage("integration_reload_management_retry", "passed")
                        reconnected_states = await reconnected_admin.request(
                            {"type": "get_states"}
                        )
                        restored_lamp = next(
                            item.get("state")
                            for item in reconnected_states
                            if item.get("entity_id") == lamp
                        )
                        check(
                            restored_lamp == lamp_before_ha_disconnect,
                            "An offline light action replayed after Home Assistant recovered",
                        )
                        stage(
                            "offline_action_not_replayed",
                            "passed",
                            light_state=restored_lamp,
                        )

                        # A pending binding stays inactive until the matching pinned key proves possession.
                        proof_state_dir = directory / "proof"
                        proof_state_dir.mkdir()
                        correct_identity_path = proof_state_dir / "identity.json"
                        wrong_identity_path = proof_state_dir / "wrong-identity.json"
                        for identity_path in (correct_identity_path, wrong_identity_path):
                            generated = subprocess.run(
                                [str(binary), "identity", str(identity_path)],
                                capture_output=True,
                                timeout=15,
                            )
                            check(generated.returncode == 0, "Rust identity generation failed")
                        correct_identity = json.loads(correct_identity_path.read_text())
                        wrong_identity = json.loads(wrong_identity_path.read_text())
                        proof_snapshot = await reconnected_admin.request({"type": "njorun/list"})
                        proof_binding = await reconnected_admin.request(
                            {
                                "type": "njorun/enroll",
                                "tablet_id": "proof",
                                "name": "Proof",
                                "profile_id": "other",
                                "tablet_public_key": correct_identity["public_key"],
                                "expected_revision": proof_snapshot["revision"],
                                "capabilities": {
                                    "orientations": ["portrait", "landscape"],
                                    "brightness": True,
                                },
                            }
                        )
                        proof_tokens, proof_client_id = device_credentials(
                            base_url, username, password, "proof"
                        )
                        proof_credentials = proof_state_dir / "credentials.json"
                        proof_state_file = proof_state_dir / "client-state"
                        private_json(
                            proof_credentials,
                            {
                                "ha_url": base_url,
                                "access_token": proof_tokens["access_token"],
                                "refresh_token": proof_tokens["refresh_token"],
                                "client_id": proof_client_id,
                                "allow_http": True,
                                "tablet_id": "proof",
                                "binding_id": proof_binding["binding_id"],
                                "tablet_private_key": wrong_identity["private_key"],
                                "ha_public_key": proof_binding["ha_public_key"],
                            },
                        )
                        wrong_peer = Simulator(
                            binary, proof_credentials, proof_state_file
                        )
                        simulators.append(wrong_peer)
                        wrong_peer.start()
                        await wrong_peer.until(
                            lambda state:
                                (state.get("dashboard") or {}).get("connection") == "ready"
                                and state.get("status")
                                == "Dashboard connected; Njörun management is unavailable",
                            "wrong pinned peer rejection",
                        )
                        proof_number = await entity_id_for_unique_id(
                            reconnected_admin, "proof_brightness"
                        )
                        await wait_for_entity_state(
                            reconnected_admin, proof_number, "unavailable"
                        )
                        stage("wrong_peer_rejected", "passed")
                        wrong_stop = wrong_peer.stop()
                        check(wrong_stop == "graceful", "Wrong-peer simulator did not stop gracefully")

                        proof_credentials.write_text(
                            json.dumps(
                                {
                                    "ha_url": base_url,
                                    "access_token": proof_tokens["access_token"],
                                    "refresh_token": proof_tokens["refresh_token"],
                                    "client_id": proof_client_id,
                                    "allow_http": True,
                                    "tablet_id": "proof",
                                    "binding_id": proof_binding["binding_id"],
                                    "tablet_private_key": correct_identity["private_key"],
                                    "ha_public_key": proof_binding["ha_public_key"],
                                }
                            )
                        )
                        proof_credentials.chmod(0o600)
                        correct_peer = Simulator(
                            binary, proof_credentials, proof_state_file
                        )
                        simulators.append(correct_peer)
                        correct_peer.start()
                        proof_revision = proof_binding["revision"]
                        await correct_peer.until(
                            lambda state:
                                (state.get("dashboard") or {}).get("connection") == "ready"
                                and state.get("status") == "Connected"
                                and (state.get("settings") or {}).get("revision")
                                == proof_revision,
                            "correct peer activation after wrong peer rejection",
                        )
                        await wait_for_applied_brightness(
                            reconnected_admin, "proof", 40
                        )
                        stage("correct_peer_activated", "passed", revision=proof_revision)

                        await reconnected_admin.configure("revoke", tablet_id="proof")
                        await correct_peer.until(
                            lambda state:
                                (state.get("dashboard") or {}).get("connection") == "ready"
                                and state.get("status")
                                == "Dashboard connected; Njörun management is unavailable",
                            "revoked binding stays unavailable",
                        )
                        await wait_for_entity_state(
                            reconnected_admin, proof_number, "unavailable"
                        )
                        await reconnected_admin.request(
                            {
                                "type": "njorun/session_start",
                                "protocol": 1,
                                "binding_id": proof_binding["binding_id"],
                            },
                            success=False,
                        )
                        stage("revocation_closes_and_denies", "passed")
                return {"enrollment": "passed", "shared_profiles": "passed", "isolated_profile": "passed",
                        "real_light_action": "passed (HA template light)",
                        "live_native_ui_action": "passed (Slint callback to HA service)",
                        "live_ui_png": str(live_ui_output),
                        "live_ui_report": str(live_ui_output.with_suffix(".json")),
                        "simulator_restart": "passed", "ha_restart_recovery": "passed", "overrides": "passed",
                        "native_number_override": "passed", "native_options_profile_edit": "passed",
                        "offline_action_not_replayed": "passed", "wrong_peer_rejected": "passed",
                        "revocation_closes_and_denies": "passed",
                        "ha_core_state": core_state,
                        "ha_port": host_port}
            finally:
                for simulator in simulators:
                    simulator.stop()


def main():
    global RELEASES
    STAGES.clear()
    SIMULATORS.clear()
    RELEASES = ReleaseServer()
    directory = Path(tempfile.mkdtemp(prefix="njorun-vertical-slice-"))
    name = "njorun-vertical-slice-" + secrets.token_hex(6)
    volume_name = name + "-config"
    container_attempted = False
    volume_attempted = False
    result = {}
    binary = Path(
        os.environ.get("NJORUN_SIMULATOR", ROOT / "target/debug/njorun-simulator")
    )
    live_ui_binary = Path(
        os.environ.get(
            "NJORUN_LIVE_UI", ROOT / "target/debug/examples/live_ui"
        )
    )
    try:
        config = directory / "ha"
        config.mkdir()
        host_port = available_host_port()
        # Firmware downloads are announced on HA's internal URL, which must be the origin a tablet
        # connects to (the tablet checks it); here that is the published port.
        initial_config = write_template_light_fixture(config, f"http://127.0.0.1:{host_port}")
        check(
            binary.is_file() and os.access(binary, os.X_OK),
            "Rust simulator executable is unavailable",
        )
        check(
            live_ui_binary.is_file() and os.access(live_ui_binary, os.X_OK),
            "Rust live UI qualification executable is unavailable",
        )
        volume_attempted = True
        probe.docker("volume", "create", volume_name)
        container_attempted = True
        probe.docker(
            "create",
            "--name",
            name,
            "--restart=no",
            "--add-host",
            "host.docker.internal:host-gateway",
            "--publish",
            f"127.0.0.1:{host_port}:8123",
            "--volume",
            f"{volume_name}:/config",
            probe.IMAGE,
        )
        probe.docker("cp", str(initial_config), f"{name}:/config/configuration.yaml")
        RELEASES.start()
        probe.docker("cp", str(patched_components(directory)), f"{name}:/config/")
        copied_initial = config / "initial-config-roundtrip.yaml"
        probe.docker("cp", f"{name}:/config/configuration.yaml", str(copied_initial))
        check(
            copied_initial.read_text() == initial_config.read_text(),
            "Docker configuration copy round trip failed",
        )
        stage("docker_volume_seed", "passed")
        probe.docker("start", name)
        port = int(probe.docker("port", name, "8123/tcp").rsplit(":", 1)[1])
        check(port == host_port, "Home Assistant host port did not match the requested port")
        base_url = f"http://127.0.0.1:{port}"
        probe.wait_for_home_assistant(base_url)
        stage("ha_initial_ready", "passed")
        result = asyncio.run(
            exercise(base_url, directory, binary, live_ui_binary, name, port)
        )
        result["status"] = "passed"
    except (Failure, probe.ProbeFailure) as error:
        result = {"status": "failed", "reason": str(error)}
        stage("failure", "failed", reason=str(error))
    except Exception:
        result = {"status": "failed", "reason": "Unexpected experiment error; secret-bearing details suppressed"}
        stage("failure", "failed", reason=result["reason"])
    finally:
        RELEASES.stop()
        shutdown_results = []
        for simulator in SIMULATORS:
            shutdown_results.append((simulator, simulator.stop()))
        forced_shutdowns = sum(
            simulator.forced_shutdown or result == "forced"
            for simulator, result in shutdown_results
        )
        failed_shutdowns = sum(
            result == "failed" for _simulator, result in shutdown_results
        )
        if forced_shutdowns or failed_shutdowns:
            result["status"] = "failed"
            result["simulator_shutdown"] = "forced" if forced_shutdowns else "failed"
            stage(
                "simulator_shutdown",
                "failed",
                forced_count=forced_shutdowns,
                failed_count=failed_shutdowns,
            )
        elif shutdown_results:
            result["simulator_shutdown"] = "graceful"
            stage(
                "simulator_shutdown",
                "passed",
                count=len(shutdown_results),
            )
        preserve_failed = (
            os.environ.get("NJORUN_KEEP_FAILED") == "1"
            and result.get("status") == "failed"
        )
        if preserve_failed:
            result["container_cleanup"] = "preserved_for_diagnostics"
            result["volume_cleanup"] = "preserved_for_diagnostics"
            result["config_cleanup"] = "preserved_for_diagnostics"
            result["config_path"] = str(directory)
            result["volume_name"] = volume_name
        elif container_attempted:
            try:
                probe.docker("rm", "--force", name, timeout=30)
                result["container_cleanup"] = "removed"
            except probe.ProbeFailure:
                result["container_cleanup"] = "failed"
        else:
            result["container_cleanup"] = "not_created"
        if not preserve_failed and volume_attempted and result.get("container_cleanup") in (
            "removed",
            "not_created",
        ):
            try:
                probe.docker("volume", "rm", volume_name, timeout=30)
                result["volume_cleanup"] = "removed"
            except probe.ProbeFailure:
                result["volume_cleanup"] = "failed"
        if not preserve_failed and result.get("container_cleanup") in ("removed", "not_created") and (
            not volume_attempted or result.get("volume_cleanup") == "removed"
        ):
            shutil.rmtree(directory)
            result["config_cleanup"] = "removed"
        elif not preserve_failed:
            result["config_cleanup"] = "preserved"
            result["config_path"] = str(directory)
            if volume_attempted:
                result["volume_name"] = volume_name
    result["stages"] = STAGES
    result["simulator"] = {
        "path": str(binary),
        "sha256": file_sha256(binary) if binary.is_file() else None,
    }
    result["live_ui"] = {
        "path": str(live_ui_binary),
        "sha256": file_sha256(live_ui_binary) if live_ui_binary.is_file() else None,
    }
    result["ha_image"] = probe.IMAGE
    report = ROOT / "target" / "qualification" / "vertical-slice.json"
    result["report_path"] = str(report)
    report.parent.mkdir(parents=True, exist_ok=True)
    with report.open("w") as output:
        output.write(json.dumps(result, indent=2) + "\n")
    report.chmod(0o600)
    print(json.dumps(result, indent=2))
    return 0 if result.get("status") == "passed" and result.get("container_cleanup") == "removed" and result.get("volume_cleanup") == "removed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
