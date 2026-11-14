#!/usr/bin/env python3
"""Probe current Home Assistant media-player schemas in a disposable local instance."""

from __future__ import annotations

import asyncio
import importlib.util
import json
from pathlib import Path
import secrets
import sys
import tempfile
import time
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
VERTICAL_SLICE_PATH = ROOT / "experiments/vertical-slice/qualify.py"
VERTICAL_SLICE_SPEC = importlib.util.spec_from_file_location(
    "njorun_vertical_slice_probe", VERTICAL_SLICE_PATH
)
if VERTICAL_SLICE_SPEC is None or VERTICAL_SLICE_SPEC.loader is None:
    raise RuntimeError("Vertical-slice qualification helpers are unavailable")
vertical_slice = importlib.util.module_from_spec(VERTICAL_SLICE_SPEC)
sys.modules[VERTICAL_SLICE_SPEC.name] = vertical_slice
VERTICAL_SLICE_SPEC.loader.exec_module(vertical_slice)
probe = vertical_slice.probe

IMAGE = probe.IMAGE
PLAY = 16_384
PLAY_MEDIA = 512
VOLUME_SET = 4
VOLUME_MUTE = 8
BROWSE_MEDIA = 131_072
SEARCH_MEDIA = 4_194_304


class ProbeFailure(RuntimeError):
    """A sanitized failure suitable for a public qualification report."""


def check(condition: bool, reason: str) -> None:
    if not condition:
        raise ProbeFailure(reason)


def error_code(message: dict[str, Any]) -> str | None:
    error = message.get("error")
    return error.get("code") if isinstance(error, dict) else None


def summarize_result(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        return {"kind": type(value).__name__}
    children = value.get("children")
    results = value.get("result")
    if isinstance(children, list):
        count = len(children)
    elif isinstance(results, list):
        count = len(results)
    else:
        count = None
    return {
        "title": value.get("title") if isinstance(value.get("title"), str) else None,
        "children_or_results": count,
        "keys": sorted(value),
    }


async def probe_home_assistant(base_url: str, token: str) -> dict[str, Any]:
    import aiohttp

    report: dict[str, Any] = {}
    async with aiohttp.ClientSession() as session:
        async with session.ws_connect(
            base_url.replace("http://", "ws://") + "/api/websocket"
        ) as websocket:
            handshake = await websocket.receive_json(timeout=20)
            check(handshake.get("type") == "auth_required", "Unexpected HA WebSocket handshake")
            await websocket.send_json({"type": "auth", "access_token": token})
            authenticated = await websocket.receive_json(timeout=20)
            check(authenticated.get("type") == "auth_ok", "HA rejected disposable credentials")

            next_id = 1

            async def request(payload: dict[str, Any]) -> dict[str, Any]:
                nonlocal next_id
                request_id = next_id
                next_id += 1
                await websocket.send_json({"id": request_id, **payload})
                while True:
                    response = await websocket.receive_json(timeout=30)
                    if response.get("id") == request_id:
                        return response

            services_response = await request({"type": "get_services"})
            check(services_response.get("success") is True, "HA get_services failed")
            services = services_response.get("result", {}).get("media_player", {})
            report["media_player_service_names"] = sorted(services)
            play_media_fields = services.get("play_media", {}).get("fields", {})
            media_selector = play_media_fields.get("media", {})
            check(
                media_selector.get("required") is True
                and isinstance(media_selector.get("selector", {}).get("media"), dict),
                "HA play_media service metadata did not expose its required media selector",
            )
            check(
                {"enqueue", "announce"} <= set(play_media_fields),
                "HA play_media service metadata omitted queue/announcement selectors",
            )
            report["play_media_action_selector"] = {
                "required": media_selector["required"],
                "selector": "media",
                "optional_fields": sorted(
                    key for key, value in play_media_fields.items() if not value.get("required")
                ),
            }

            deadline = time.monotonic() + 45
            states: list[dict[str, Any]] = []
            while time.monotonic() < deadline:
                states_response = await request({"type": "get_states"})
                check(states_response.get("success") is True, "HA get_states failed")
                states = states_response.get("result", [])
                if any(
                    state.get("entity_id") in ("media_player.browse", "media_player.search")
                    for state in states
                ):
                    break
                await asyncio.sleep(1)

            players = {
                state["entity_id"]: state
                for state in states
                if state.get("entity_id")
                in ("media_player.living_room", "media_player.browse", "media_player.search")
            }
            check("media_player.browse" in players, "Demo browse player did not register")
            check("media_player.search" in players, "Demo search player did not register")
            report["demo_player_features"] = {
                entity_id: int(state.get("attributes", {}).get("supported_features", 0))
                for entity_id, state in players.items()
            }

            async def call_service(
                entity_id: str, name: str, service_data: dict[str, Any] | None = None
            ) -> dict[str, Any]:
                command: dict[str, Any] = {
                    "type": "call_service",
                    "domain": "media_player",
                    "service": name,
                    "target": {"entity_id": entity_id},
                }
                if service_data is not None:
                    command["service_data"] = service_data
                return await request(command)

            living_room = players.get("media_player.living_room")
            check(living_room is not None, "Demo living-room player did not register")
            living_features = int(
                living_room.get("attributes", {}).get("supported_features", 0)
            )
            if living_features & VOLUME_SET:
                result = await call_service(
                    "media_player.living_room", "volume_set", {"volume_level": 0.37}
                )
                check(result.get("success") is True, "HA rejected volume_level service data")
                report["volume_set"] = "passed"
            if living_features & VOLUME_MUTE:
                result = await call_service(
                    "media_player.living_room",
                    "volume_mute",
                    {"is_volume_muted": True},
                )
                check(result.get("success") is True, "HA rejected is_volume_muted service data")
                report["volume_mute"] = "passed"
            if living_features & PLAY:
                result = await call_service("media_player.living_room", "media_play")
                check(result.get("success") is True, "HA media_play service failed")
                report["media_play"] = "passed"
            if living_features & PLAY_MEDIA:
                result = await call_service(
                    "media_player.living_room",
                    "play_media",
                    {
                        "media_content_type": "music",
                        "media_content_id": "media-source://media_source/local/njorun-track.mp3",
                    },
                )
                check(
                    result.get("success") is True,
                    "HA rejected flattened media_content_type/media_content_id service data",
                )
                report["play_media_flat_service_data"] = "passed"
                excluded = await call_service(
                    "media_player.living_room",
                    "play_media",
                    {
                        "media_content_type": "music",
                        "media_content_id": "media-source://media_source/local/njorun-track.mp3",
                        "enqueue": "add",
                        "announce": False,
                    },
                )
                check(
                    excluded.get("success") is False
                    and error_code(excluded) == "invalid_format",
                    "HA accepted mutually exclusive enqueue and announce fields",
                )
                report["enqueue_announce_exclusion"] = "passed"

            half_pair = await request(
                {
                    "type": "media_player/browse_media",
                    "entity_id": "media_player.browse",
                    "media_content_type": "music",
                }
            )
            check(
                half_pair.get("success") is False
                and error_code(half_pair) == "invalid_format",
                "HA browse_media accepted a partial content-type/ID pair",
            )
            report["browse_pair_validation"] = "passed"

            browse = await request(
                {"type": "media_player/browse_media", "entity_id": "media_player.browse"}
            )
            check(browse.get("success") is True, "Advertised demo browse_media query failed")
            browse_result = browse.get("result")
            check(isinstance(browse_result, dict), "HA browse_media returned no result object")
            children = browse_result.get("children", [])
            check(isinstance(children, list), "HA browse_media result had no children array")
            report["browse_root"] = summarize_result(browse_result)
            report["browse_children"] = [
                {
                    "title": child.get("title") if isinstance(child.get("title"), str) else None,
                    "media_class": child.get("media_class"),
                    "can_expand": child.get("can_expand") is True,
                    "can_play": child.get("can_play") is True,
                }
                for child in children[:20]
                if isinstance(child, dict)
            ]

            expanded = next(
                (
                    child
                    for child in children
                    if child.get("can_expand") is True
                    and str(child.get("title", "")).casefold() == "my media"
                    and isinstance(child.get("media_content_type"), str)
                    and isinstance(child.get("media_content_id"), str)
                ),
                None,
            )
            if expanded is None:
                expanded = next(
                    (
                        child
                        for child in children
                        if child.get("can_expand") is True
                        and isinstance(child.get("media_content_type"), str)
                        and isinstance(child.get("media_content_id"), str)
                    ),
                    None,
                )
            nested_status = "no_expandable_child"
            if expanded is not None:
                nested = await request(
                    {
                        "type": "media_player/browse_media",
                        "entity_id": "media_player.browse",
                        "media_content_type": expanded["media_content_type"],
                        "media_content_id": expanded["media_content_id"],
                    }
                )
                nested_status = (
                    "passed" if nested.get("success") is True else error_code(nested) or "failed"
                )
                if nested.get("success") is True:
                    report["browse_nested"] = summarize_result(nested.get("result"))
            report["browse_nested_status"] = nested_status

            invalid_filter = await request(
                {
                    "type": "media_player/search_media",
                    "entity_id": "media_player.search",
                    "search_query": "music",
                    "media_filter_classes": ["invalid-class"],
                }
            )
            check(
                invalid_filter.get("success") is False
                and error_code(invalid_filter) == "invalid_format",
                "HA search_media accepted an invalid media class",
            )
            report["search_filter_validation"] = "passed"

            search = await request(
                {
                    "type": "media_player/search_media",
                    "entity_id": "media_player.search",
                    "search_query": "music",
                    "media_filter_classes": ["album", "track"],
                }
            )
            check(search.get("success") is True, "Advertised demo search_media query failed")
            report["search_results"] = summarize_result(search.get("result"))
    return report


def main() -> int:
    name = "njorun-entity-controls-" + secrets.token_hex(6)
    volume = name + "-config"
    container_created = False
    volume_created = False
    result: dict[str, Any] = {"ha_image": IMAGE, "status": "failed"}
    try:
        with tempfile.TemporaryDirectory(prefix="njorun-entity-controls-") as scratch:
            config = Path(scratch) / "config"
            media_directory = config / "media"
            media_directory.mkdir(parents=True)
            (config / "configuration.yaml").write_text(
                "homeassistant:\n  media_dirs:\n    local: /config/media\n"
                "default_config:\ndemo:\n"
            )
            # The media source starts with one harmless placeholder item, so nested browsing can
            # exercise a directory that actually exists in the container.
            (media_directory / "njorun-track.mp3").write_bytes(
                b"ID3\x04\x00\x00\x00\x00\x00\x00"
            )
            probe.docker("pull", IMAGE)
            probe.docker(
                "volume",
                "create",
                "--label",
                "njorun.qualification=entity-controls",
                volume,
            )
            volume_created = True
            probe.docker(
                "create",
                "--name",
                name,
                "--restart=no",
                "--publish",
                "127.0.0.1::8123",
                "--volume",
                f"{volume}:/config",
                "--env",
                "TZ=UTC",
                IMAGE,
            )
            container_created = True
            probe.docker("cp", str(config / "configuration.yaml"), f"{name}:/config/")
            probe.docker("cp", str(media_directory), f"{name}:/config/")
            probe.docker("start", name)
            host_port = int(probe.docker("port", name, "8123/tcp").rsplit(":", 1)[1])
            base_url = f"http://127.0.0.1:{host_port}"
            probe.wait_for_home_assistant(base_url)
            tokens, _username, _password = vertical_slice.bootstrap(base_url)
            result.update(asyncio.run(probe_home_assistant(base_url, tokens["access_token"])))
            result["status"] = "passed"
    except ProbeFailure as error:
        result["reason"] = str(error)
    except Exception as error:  # noqa: BLE001 - suppress unexpected secret-bearing diagnostics.
        result["reason"] = f"Unexpected probe error ({type(error).__name__}); details suppressed"
    finally:
        if container_created:
            try:
                probe.docker("rm", "--force", name, timeout=30)
                result["container_cleanup"] = "removed"
            except ProbeFailure:
                result["container_cleanup"] = "failed"
        else:
            result["container_cleanup"] = "not_created"
        if volume_created:
            try:
                probe.docker("volume", "rm", volume, timeout=30)
                result["volume_cleanup"] = "removed"
            except ProbeFailure:
                result["volume_cleanup"] = "failed"
        else:
            result["volume_cleanup"] = "not_created"

    print(json.dumps(result, sort_keys=True))
    return int(
        result.get("status") != "passed"
        or result["container_cleanup"] != "removed"
        or result["volume_cleanup"] != "removed"
    )


if __name__ == "__main__":
    raise SystemExit(main())
