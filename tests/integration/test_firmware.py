"""Firmware releases of decision D7: verification, catalog, cache, download and updates."""

from __future__ import annotations

import asyncio
import base64
import json
from pathlib import Path
import sys
import tempfile
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_live as base  # noqa: E402  (reuses the stubbed runtime module and helpers)

from aiohttp import web  # noqa: E402
from aiohttp.test_utils import TestClient, TestServer  # noqa: E402

ROOT = base.ROOT
sys.path.insert(0, str(ROOT / "scripts/release"))
import fixtures  # noqa: E402
import manifest as reference  # noqa: E402

PACKAGE = base.PACKAGE_NAME
# test_live drops its stubbed runtime from sys.modules after loading it; updates imports it.
sys.modules.setdefault(f"{PACKAGE}.runtime", base.runtime_module)
mf = base.importlib.import_module(f"{PACKAGE}.firmware_manifest")
fw = base.importlib.import_module(f"{PACKAGE}.firmware")
serve = base.importlib.import_module(f"{PACKAGE}.firmware_serve")
updates = base.importlib.import_module(f"{PACKAGE}.updates")
install_module = base.importlib.import_module(f"{PACKAGE}.install")

OTA = {
    "board": "d1001",
    "hardware_revision": "1",
    "partition_layout": "d1001-32m-2xota8m-v1",
}
CAPABILITIES = {
    "orientations": ["portrait"],
    "brightness": True,
    "live": ["wake", "backlight", "telemetry"],
    "ota": OTA,
}
KEY = fixtures.test_key("current")
KEY_ID = reference.key_id(reference.public_raw(KEY.public_key()))
TRUSTED = [reference.public_raw(KEY.public_key())]
REPO = fw.REPOSITORY
DOWNLOAD = fw.DOWNLOAD_PREFIX.format(repo=REPO)


def image_bytes(version: str) -> bytes:
    return (version.encode() * 4096)[:100_000]


def make_release(version, channel="stable", *, board="d1001", layout=None, revisions=("1",),
                 min_protocol=1, coprocessor=None, prerelease=None, key=KEY, tag=None):
    """A GitHub release item plus the files behind it: (item, files)."""
    image = image_bytes(version)
    name = f"njorun-{board}-{version}-app.bin"
    entry = {
        "board": board,
        "hardware_revisions": list(revisions),
        "partition_layout": layout or OTA["partition_layout"],
        "partition_table_sha256": "0" * 64,
        "coprocessor": {"chip": "esp32-c6", "min_firmware": coprocessor},
        "min_integration_protocol": min_protocol,
        "artifacts": {
            "factory": reference.artifact_entry(f"njorun-{board}-{version}-factory.bin", b"f" * 64),
            "app": reference.artifact_entry(name, image),
        },
    }
    text = reference.build_manifest(
        version=version, channel=channel, source_commit="a" * 40,
        signing_key_id=reference.key_id(reference.public_raw(key.public_key())), boards=[entry])
    signature = reference.sign(text, key)
    tag = tag or f"v{version}"
    files = {
        f"{DOWNLOAD}{tag}/manifest.json": text,
        f"{DOWNLOAD}{tag}/manifest.json.sig": signature,
        f"{DOWNLOAD}{tag}/{name}": image,
    }
    item = {
        "tag_name": tag,
        "draft": False,
        "prerelease": (channel == "beta") if prerelease is None else prerelease,
        "html_url": f"https://github.com/{REPO}/releases/tag/{tag}",
        "published_at": "2026-11-11T00:00:00Z",
        "assets": [{"name": url.rsplit("/", 1)[1], "browser_download_url": url} for url in files],
    }
    return item, files


class FakeGitHub:
    """The release list and the files, with a request log and switchable failures."""

    def __init__(self, *releases):
        self.items = [item for item, _ in releases]
        self.files = {}
        for _, files in releases:
            self.files.update(files)
        self.requests = []
        self.down = False

    async def fetch(self, url, *, limit, json_api=False):
        self.requests.append(url)
        if self.down:
            raise fw.FetchError("offline")
        if url.startswith(f"{fw.API_URL}/repos/{REPO}/releases"):
            return json.dumps(self.items).encode()
        data = self.files[url]
        if len(data) > limit:
            raise fw.FetchError("too large")
        return data


def catalog_for(github):
    return fw.ReleaseCatalog(github, TRUSTED)


class ManifestVerifierTests(unittest.TestCase):
    def test_every_shared_vector(self):
        committed = json.loads((ROOT / "protocol/fixtures/firmware-manifest.json").read_text())
        for case in committed["cases"]:
            with self.subTest(case["name"]):
                try:
                    trusted = [base64.b64decode(key) for key in case["trusted_keys"]]
                    parsed = mf.verify(case["manifest"].encode(), case["signature"], trusted)
                    request = case["request"]
                    entry = mf.select_board(
                        parsed, request["board"], request["hardware_revision"], request["channel"])
                    if "artifact" in case:
                        mf.check_artifact(entry, case["artifact"]["kind"],
                                          base64.b64decode(case["artifact"]["data"]),
                                          case.get("slot_size"))
                    result = "ok"
                except mf.ManifestError as error:
                    result = error.code
                self.assertEqual(result, case["expect"])

    def test_the_integration_ships_the_repository_keys(self):
        self.assertEqual(
            json.loads((ROOT / "protocol/release-keys.json").read_text()),
            json.loads(mf.KEYS_FILE.read_text()),
            "copy protocol/release-keys.json to custom_components/njorun/release-keys.json",
        )
        self.assertEqual(len(mf.load_trusted_keys()), 2)


class VersionTests(unittest.TestCase):
    def test_semver_precedence(self):
        order = ["0.1.0-beta.1", "0.1.0-beta.2", "0.1.0-beta.10", "0.1.0", "0.2.0-beta.1",
                 "0.2.0", "1.0.0"]
        self.assertEqual(sorted(reversed(order), key=fw.semver_key), order)
        self.assertTrue(fw.is_newer("0.2.0", "0.2.0-beta.4"))
        self.assertFalse(fw.is_newer("0.2.0", "0.2.0"))

    def test_update_window(self):
        at = lambda hour, minute: hour * 60 + minute  # noqa: E731
        self.assertTrue(fw.in_update_window(at(2, 0), "02:00"))
        self.assertTrue(fw.in_update_window(at(3, 59), "02:00"))
        self.assertFalse(fw.in_update_window(at(4, 0), "02:00"))
        self.assertFalse(fw.in_update_window(at(1, 59), "02:00"))
        self.assertTrue(fw.in_update_window(at(0, 30), "23:00"), "across midnight")
        self.assertFalse(fw.in_update_window(at(2, 0), "garbage"))


class CatalogTests(unittest.IsolatedAsyncioTestCase):
    async def test_channels_pick_their_newest_release(self):
        github = FakeGitHub(
            make_release("0.1.0"), make_release("0.2.0"),
            make_release("0.3.0-beta.1", "beta"), make_release("0.2.1-beta.2", "beta"))
        catalog = catalog_for(github)
        await catalog.refresh()
        self.assertEqual(catalog.latest("stable", OTA).version, "0.2.0")
        self.assertEqual(catalog.latest("beta", OTA).version, "0.3.0-beta.1")
        self.assertEqual(catalog.find("0.1.0", OTA).version, "0.1.0")
        self.assertEqual(catalog.problems, [])

    async def test_rejected_releases_are_skipped_and_named(self):
        forged = make_release("0.9.0", key=fixtures.test_key("other"))
        wrong_flag = make_release("0.8.0", prerelease=True)
        wrong_tag = make_release("0.7.0", tag="v0.7.1")
        draft = make_release("0.6.0")
        draft[0]["draft"] = True
        foreign = make_release("0.5.0")
        foreign[0]["assets"][2]["browser_download_url"] = "https://evil.example/x.bin"
        github = FakeGitHub(forged, wrong_flag, wrong_tag, draft, foreign, make_release("0.1.0"))
        # A release signed by an untrusted key never verifies; sign() refuses a wrong key id,
        # so forge the pair by hand.
        forged_files = [k for k in github.files if "0.9.0/manifest.json" in k and k.endswith("json")]
        github.files[forged_files[0] + ".sig"] = base64.b64encode(
            fixtures.test_key("other").sign(github.files[forged_files[0]])) + b"\n"
        catalog = catalog_for(github)
        await catalog.refresh()
        self.assertEqual([r.version for r in catalog.releases], ["0.1.0"])
        self.assertTrue(any("v0.5.0" in p for p in catalog.problems), "an image off GitHub")
        self.assertEqual(catalog.latest("stable", OTA).version, "0.1.0")
        self.assertTrue(any("v0.8.0" in p for p in catalog.problems))
        self.assertTrue(any("v0.7.1" in p for p in catalog.problems))
        self.assertFalse(any("v0.6.0" in p for p in catalog.problems), "drafts are ignored")

    async def test_a_tampered_manifest_is_rejected(self):
        item, files = make_release("0.2.0")
        manifest_url = next(k for k in files if k.endswith("manifest.json"))
        files[manifest_url] = files[manifest_url].replace(b'"0.2.0"', b'"0.2.9"')
        catalog = catalog_for(FakeGitHub((item, files)))
        await catalog.refresh()
        self.assertEqual(catalog.releases, [])
        self.assertIn("bad_signature", catalog.problems[0])

    async def test_verified_manifests_are_not_fetched_twice(self):
        github = FakeGitHub(make_release("0.1.0"))
        catalog = catalog_for(github)
        await catalog.refresh()
        first = len(github.requests)
        await catalog.refresh()
        self.assertEqual(len(github.requests), first + 1, "only the release list again")

    async def test_a_failing_list_keeps_the_previous_result(self):
        github = FakeGitHub(make_release("0.1.0"))
        catalog = catalog_for(github)
        await catalog.refresh()
        github.down = True
        with self.assertRaises(fw.FetchError):
            await catalog.refresh()
        self.assertEqual(catalog.latest("stable", OTA).version, "0.1.0")

    async def test_the_tablet_must_match_board_revision_and_layout(self):
        github = FakeGitHub(make_release("0.1.0"))
        catalog = catalog_for(github)
        await catalog.refresh()
        for ota, code in (
            ({**OTA, "board": "other"}, "board_mismatch"),
            ({**OTA, "hardware_revision": "2"}, "revision_unsupported"),
            ({**OTA, "partition_layout": "d1001-16m-v1"}, "layout_mismatch"),
            ({"board": "d1001"}, "unreported"),
            (None, "unreported"),
        ):
            with self.subTest(code):
                self.assertIsNone(catalog.latest("stable", ota))
                self.assertEqual(
                    catalog.incompatibility("stable", ota), code)
        self.assertIsNone(catalog.incompatibility("stable", OTA))

    async def test_releases_that_demand_more_than_ha_can_check_are_not_offered(self):
        github = FakeGitHub(
            make_release("0.1.0", min_protocol=2), make_release("0.2.0", coprocessor="1.0.0"))
        catalog = catalog_for(github)
        await catalog.refresh()
        self.assertIsNone(catalog.latest("stable", OTA))
        self.assertEqual(
            sorted({catalog.incompatibility("stable", OTA)}), ["coprocessor_unverifiable"])


class CacheTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name) / "cache"

    async def offer(self, version):
        github = FakeGitHub(make_release(version))
        catalog = catalog_for(github)
        await catalog.refresh()
        return github, catalog.find(version, OTA)

    async def test_an_image_is_downloaded_verified_and_reused(self):
        github, offer = await self.offer("0.2.0")
        cache = fw.FirmwareCache(self.root, TRUSTED)
        cached = await fw.ensure_image(github, cache, offer)
        self.assertEqual(cached.image_path.read_bytes(), image_bytes("0.2.0"))
        self.assertEqual(cached.manifest_bytes, offer.release.manifest_bytes)
        before = len(github.requests)
        again = await fw.ensure_image(github, cache, offer)
        self.assertEqual(again.sha256, cached.sha256)
        self.assertEqual(len(github.requests), before, "served from disk")

    async def test_a_wrong_image_is_never_cached(self):
        github, offer = await self.offer("0.2.0")
        url = offer.release.assets[offer.app["name"]]
        github.files[url] = bytes([github.files[url][0] ^ 1]) + github.files[url][1:]
        cache = fw.FirmwareCache(self.root, TRUSTED)
        with self.assertRaises(mf.ManifestError) as caught:
            await fw.ensure_image(github, cache, offer)
        self.assertEqual(caught.exception.code, "digest_mismatch")
        self.assertFalse(self.root.exists() and any(self.root.iterdir()))

    async def test_a_damaged_cache_is_discarded_and_fetched_again(self):
        github, offer = await self.offer("0.2.0")
        cache = fw.FirmwareCache(self.root, TRUSTED)
        cached = await fw.ensure_image(github, cache, offer)
        cached.image_path.write_bytes(b"x" * offer.app["size"])
        self.assertIsNone(cache.load("0.2.0", offer.entry))
        self.assertFalse((self.root / "0.2.0").exists())
        await fw.ensure_image(github, cache, offer)
        self.assertIsNotNone(cache.load("0.2.0", offer.entry))

    async def test_a_cached_manifest_that_no_longer_verifies_is_discarded(self):
        github, offer = await self.offer("0.2.0")
        cache = fw.FirmwareCache(self.root, TRUSTED)
        await fw.ensure_image(github, cache, offer)
        wrong = fw.FirmwareCache(self.root, [reference.public_raw(
            fixtures.test_key("other").public_key())])
        self.assertIsNone(wrong.load("0.2.0", offer.entry))

    async def test_retention_keeps_the_newest_and_the_protected(self):
        cache = fw.FirmwareCache(self.root, TRUSTED, keep=2)
        for version in ("0.1.0", "0.2.0", "0.3.0", "0.4.0"):
            github, offer = await self.offer(version)
            await fw.ensure_image(github, cache, offer)
        (self.root / "junk").mkdir()
        removed = cache.prune({"0.1.0"})
        self.assertEqual(removed, ["0.2.0"])
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), ["0.1.0", "0.3.0", "0.4.0"])


class ServeTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        github = FakeGitHub(make_release("0.2.0"))
        catalog = catalog_for(github)
        await catalog.refresh()
        self.offer = catalog.find("0.2.0", OTA)
        self.image = await fw.ensure_image(
            github, fw.FirmwareCache(Path(self.directory.name), TRUSTED), self.offer)
        self.now = 1000.0
        self.registry = serve.GrantRegistry(clock=lambda: self.now)
        self.open = {"t1"}
        app = web.Application()

        async def handler(request):
            return await serve.serve(
                request, request.match_info["version"], request.match_info["name"],
                self.registry, lambda tablet: tablet in self.open)

        app.router.add_route("*", "/api/njorun/firmware/{version}/{name}", handler)
        self.client = TestClient(TestServer(app))
        await self.client.start_server()
        self.addAsyncCleanup(self.client.close)
        self.token = self.registry.issue("t1", self.image, self.now + 300)
        self.base = "/api/njorun/firmware/0.2.0/"
        self.image_url = self.base + self.image.name

    def auth(self, token=None):
        return {"Authorization": f"Bearer {token or self.token}"}

    async def test_the_three_files_are_served_byte_for_byte(self):
        manifest = await self.client.get(self.base + "manifest.json", headers=self.auth())
        self.assertEqual(await manifest.read(), self.image.manifest_bytes)
        signature = await self.client.get(self.base + "manifest.json.sig", headers=self.auth())
        self.assertEqual(await signature.read(), self.image.signature)
        full = await self.client.get(self.image_url, headers=self.auth())
        self.assertEqual(full.status, 200)
        self.assertEqual(await full.read(), image_bytes("0.2.0"))
        self.assertEqual(full.headers["Cache-Control"], "no-store")
        head = await self.client.head(self.image_url, headers=self.auth())
        self.assertEqual(int(head.headers["Content-Length"]), self.image.size)

    async def test_ranges_resume_a_download(self):
        data = image_bytes("0.2.0")
        part = await self.client.get(
            self.image_url, headers={**self.auth(), "Range": "bytes=1000-1999"})
        self.assertEqual(part.status, 206)
        self.assertEqual(part.headers["Content-Range"], f"bytes 1000-1999/{len(data)}")
        self.assertEqual(await part.read(), data[1000:2000])
        tail = await self.client.get(
            self.image_url, headers={**self.auth(), "Range": "bytes=99000-"})
        self.assertEqual(await tail.read(), data[99000:])
        self.assertEqual(
            (await self.client.get(self.image_url, headers={**self.auth(), "Range": "bytes=999999-"})
             ).status, 416)

    async def test_everything_without_a_valid_grant_is_401(self):
        async def status(headers=None, url=self.image_url):
            return (await self.client.get(url, headers=headers or {})).status
        self.assertEqual(await status(), 401)
        self.assertEqual(await status({"Authorization": "Bearer " + "A" * 43}), 401)
        self.assertEqual(await status({"Authorization": "Basic " + self.token}), 401)
        self.assertEqual(await status(url=self.image_url + f"?token={self.token}"), 401, "no query tokens")
        self.open.clear()
        self.assertEqual(await status(self.auth()), 401, "the install is no longer open")
        self.open.add("t1")
        self.assertEqual(await status(self.auth()), 200)
        self.now += 301
        self.assertEqual(await status(self.auth()), 401, "the token expires with the command")

    async def test_unknown_files_and_versions_are_404_and_a_new_grant_replaces_the_old(self):
        self.assertEqual(
            (await self.client.get(self.base + "other.bin", headers=self.auth())).status, 404)
        self.assertEqual((await self.client.get(
            "/api/njorun/firmware/0.9.9/" + self.image.name, headers=self.auth())).status, 404)
        newer = self.registry.issue("t1", self.image, self.now + 300)
        self.assertEqual(
            (await self.client.get(self.image_url, headers=self.auth())).status, 401)
        self.assertEqual(
            (await self.client.get(self.image_url, headers=self.auth(newer))).status, 200)
        self.registry.revoke("t1")
        self.assertEqual(
            (await self.client.get(self.image_url, headers=self.auth(newer))).status, 401)

    async def test_tokens_have_the_install_message_shape(self):
        self.assertTrue(install_module.valid_install({
            "type": "install", "install_id": "i1", "version": "0.2.0", "channel": "stable",
            "size": 10, "sha256": "0" * 64, "url": "http://ha.local:8123" + self.image_url,
            "token": self.token, "expires_at": 1}))


CAPS = {**CAPABILITIES}


class ManagerTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.github = FakeGitHub(
            make_release("0.1.0"), make_release("0.2.0"), make_release("0.3.0-beta.1", "beta"))
        self.runtime = base._runtime(CAPS)
        self.session, self.connection, self.noise = await base._establish(self.runtime)
        self.registry = serve.GrantRegistry()
        self.clock = time.time
        self.manager = updates.FirmwareManager(
            self.runtime, catalog_for(self.github),
            fw.FirmwareCache(Path(self.directory.name), TRUSTED), self.registry,
            lambda: "http://ha.local:8123")
        await self.manager.async_refresh(force=True)
        self.report("0.1.0")

    def report(self, version):
        self.session.telemetry = {**base.TELEMETRY, "firmware_version": version}

    async def install(self, *args, **kwargs):
        """Run an install while the tablet accepts it."""
        task = asyncio.create_task(self.manager.async_install("t1", *args, **kwargs))
        for _ in range(50):
            await asyncio.sleep(0.01)
            if self.noise.sent and self.noise.sent[-1].get("type") == "install":
                break
        sent = self.noise.sent[-1]
        await base._receive(self.runtime, self.session, self.connection, self.noise, {
            "type": "install_result", "install_id": sent["install_id"],
            "state": "accepted", "reason": None})
        return sent, await task

    async def test_the_update_is_offered_from_the_tablets_channel(self):
        self.assertEqual(self.manager.latest("t1").version, "0.2.0")
        self.assertEqual(self.manager.installed_version("t1"), "0.1.0")
        self.runtime.configuration.tablets["t1"]["overrides"]["update_channel"] = "beta"
        self.assertEqual(self.manager.latest("t1").version, "0.3.0-beta.1")

    async def test_install_sends_the_verified_image_with_a_grant(self):
        sent, install_id = await self.install()
        self.assertEqual(sent["install_id"], install_id)
        image = image_bytes("0.2.0")
        self.assertEqual(
            (sent["version"], sent["channel"], sent["size"], sent["sha256"]),
            ("0.2.0", "stable", len(image), reference.sha256_hex(image)))
        self.assertEqual(
            sent["url"], "http://ha.local:8123/api/njorun/firmware/0.2.0/njorun-d1001-0.2.0-app.bin")
        self.assertTrue(install_module.valid_install(sent))
        grant = self.registry.lookup(sent["token"])
        self.assertIsNotNone(grant)
        self.assertEqual(grant.tablet_id, "t1")
        self.assertAlmostEqual(grant.expires_at, sent["expires_at"], delta=1)
        self.assertTrue(self.manager.install_open("t1"))
        with self.assertRaises(updates.UpdateError):
            await self.manager.async_install("t1")  # one open install per tablet

    async def test_a_rejected_install_revokes_the_grant(self):
        task = asyncio.create_task(self.manager.async_install("t1"))
        await asyncio.sleep(0.05)
        sent = self.noise.sent[-1]
        await base._receive(self.runtime, self.session, self.connection, self.noise, {
            "type": "install_result", "install_id": sent["install_id"],
            "state": "rejected", "reason": "busy"})
        with self.assertRaises(updates.UpdateError):
            await task
        self.assertIsNone(self.registry.lookup(sent["token"]))
        self.assertFalse(self.manager.install_open("t1"))

    async def test_downgrades_and_reinstalls_need_the_explicit_flag(self):
        self.report("0.2.0")
        with self.assertRaises(updates.UpdateError) as caught:
            await self.manager.async_install("t1", "0.1.0")
        self.assertIn("allow_downgrade", str(caught.exception))
        with self.assertRaises(updates.UpdateError):
            await self.manager.async_install("t1", "0.2.0", allow_downgrade=True)
        self.assertEqual(self.noise.sent, [])
        sent, _ = await self.install("0.1.0", allow_downgrade=True)
        self.assertEqual(sent["version"], "0.1.0")

    async def test_install_needs_a_live_session_and_a_fitting_release(self):
        with self.assertRaises(updates.UpdateError):
            await self.manager.async_install("t1", "9.9.9")
        self.runtime.configuration.tablets["t1"]["capabilities"]["ota"] = {"board": "d1001"}
        with self.assertRaises(updates.UpdateError) as caught:
            await self.manager.async_install("t1")
        self.assertIn("hardware revision", str(caught.exception))
        self.runtime.configuration.tablets["t1"]["capabilities"]["ota"] = OTA
        self.runtime.close_session = None
        self.runtime.sessions.clear()
        self.runtime.session_by_tablet.clear()
        with self.assertRaises(updates.UpdateError):
            await self.manager.async_install("t1")

    async def test_a_bad_download_stops_before_anything_is_sent(self):
        url = self.manager.latest("t1").release.assets["njorun-d1001-0.2.0-app.bin"]
        self.github.files[url] = b"x" * len(self.github.files[url])
        with self.assertRaises(updates.UpdateError):
            await self.manager.async_install("t1")
        self.assertEqual(self.noise.sent, [])

    async def test_the_policy_installs_once_inside_the_window(self):
        at_two = 2 * 60 + 5
        self.runtime.configuration.tablets["t1"]["overrides"]["update_policy"] = "manual"
        self.assertEqual(await self.manager.async_apply_policy(at_two), [])
        self.runtime.configuration.tablets["t1"]["overrides"]["update_policy"] = "automatic"
        self.assertEqual(await self.manager.async_apply_policy(5 * 60), [], "outside the window")
        task = asyncio.create_task(self.manager.async_apply_policy(at_two))
        await asyncio.sleep(0.05)
        sent = self.noise.sent[-1]
        await base._receive(self.runtime, self.session, self.connection, self.noise, {
            "type": "install_result", "install_id": sent["install_id"],
            "state": "rejected", "reason": "busy"})
        self.assertEqual(await task, [])
        sends = len(self.noise.sent)
        self.assertEqual(await self.manager.async_apply_policy(at_two), [], "once per version")
        self.assertEqual(len(self.noise.sent), sends)

    async def test_the_policy_never_downgrades_or_repeats_the_installed_version(self):
        self.report("0.2.0")
        self.assertEqual(await self.manager.async_apply_policy(2 * 60 + 5), [])
        self.report("1.0.0")
        self.assertEqual(await self.manager.async_apply_policy(2 * 60 + 5), [])
        self.assertEqual(self.noise.sent, [])

    async def test_the_last_known_version_survives_an_offline_tablet(self):
        self.assertEqual(self.manager.installed_version("t1"), "0.1.0")
        self.session.telemetry = None
        self.assertEqual(self.manager.installed_version("t1"), "0.1.0")

    async def test_a_failing_refresh_is_reported_not_raised(self):
        self.github.down = True
        await self.manager.async_refresh(force=True)
        self.assertEqual(self.manager.last_error, "offline")
        self.assertEqual(self.manager.latest("t1").version, "0.2.0")
        self.github.down = False
        await self.manager.async_refresh(force=True)
        self.assertIsNone(self.manager.last_error)

    def test_progress_is_indicative_over_the_stages(self):
        def install(stage, done, state="accepted"):
            return {"state": state, "stage": stage, "done": done, "total": 100}
        self.assertEqual(updates.progress_percentage(None), None)
        self.assertEqual(updates.progress_percentage(install(None, 0)), 0)
        self.assertEqual(updates.progress_percentage(install("download", 50)), 25)
        self.assertEqual(updates.progress_percentage(install("write", 100)), 95)
        self.assertIsNone(updates.progress_percentage(install("write", 100, "installed")))


class CapabilityTests(unittest.TestCase):
    def test_ota_may_carry_revision_and_layout(self):
        plain = {"orientations": ["portrait"], "brightness": True}
        self.assertEqual(base.model.validate_capabilities(CAPABILITIES), CAPABILITIES)
        for ota in ({"board": "d1001", "hardware_revision": ""},
                    {"board": "d1001", "partition_layout": 1},
                    {"hardware_revision": "1"},
                    {"board": "d1001", "extra": "x"}):
            with self.assertRaises(base.model.ConfigurationError, msg=ota):
                base.model.validate_capabilities({**plain, "ota": ota})


if __name__ == "__main__":
    unittest.main()
