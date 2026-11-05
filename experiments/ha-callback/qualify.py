#!/usr/bin/env python3
"""Run a bounded Home Assistant authorization callback qualification.

Requires Docker and Python 3.10+. The probe pulls one pinned public Home
Assistant Core image, starts a temporary container with a random localhost-only
port and disposable configuration, creates a random onboarding user, exercises
the auth API, and removes only its own container and temporary configuration.

No credential, authorization code, access token, refresh token, or callback
query is printed. The login flow uses Home Assistant's documented HTTP API; this
does not exercise a phone browser or claim browser-to-device callback UX.
"""

from __future__ import annotations

import base64
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import secrets
import shutil
import subprocess
import sys
import threading
import time
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.parse import parse_qs, urlencode, urlsplit
from urllib.request import Request, urlopen


IMAGE_TAG = "ghcr.io/home-assistant/home-assistant:2026.11.0"
IMAGE = (
    "ghcr.io/home-assistant/home-assistant"
    "@sha256:c6c1ca9931036bf3a2c4cc088d8db25dfea3b33935a5ecc83c40349462e2f69e"
)
READY_TIMEOUT_SECONDS = 240
HTTP_TIMEOUT_SECONDS = 15
DOCKER_TIMEOUT_SECONDS = 300


class ProbeFailure(RuntimeError):
    """A sanitized error that is safe to print."""


def docker(*args: str, timeout: int = DOCKER_TIMEOUT_SECONDS) -> str:
    """Run Docker without echoing potentially sensitive output."""
    try:
        result = subprocess.run(
            ["docker", *args],
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout,
        )
    except OSError as exc:
        raise ProbeFailure("Docker CLI could not be started") from exc
    except subprocess.TimeoutExpired as exc:
        raise ProbeFailure(f"Docker command timed out ({args[0]})") from exc
    if result.returncode:
        raise ProbeFailure(f"Docker command failed ({args[0]})")
    return result.stdout.strip()


def decode_body(body: bytes) -> Any:
    if not body:
        return None
    try:
        return json.loads(body)
    except (json.JSONDecodeError, UnicodeDecodeError):
        return None


def http_request(
    base_url: str,
    path: str,
    *,
    method: str = "GET",
    json_body: dict[str, Any] | None = None,
    form_body: dict[str, str] | None = None,
    bearer: str | None = None,
) -> tuple[int, Any]:
    headers = {"Accept": "application/json"}
    if bearer is not None:
        headers["Authorization"] = f"Bearer {bearer}"
    if json_body is not None:
        headers["Content-Type"] = "application/json"
        body = json.dumps(json_body).encode()
    elif form_body is not None:
        headers["Content-Type"] = "application/x-www-form-urlencoded"
        body = urlencode(form_body).encode()
    else:
        body = None

    request = Request(
        f"{base_url}{path}", data=body, headers=headers, method=method
    )
    try:
        with urlopen(request, timeout=HTTP_TIMEOUT_SECONDS) as response:
            return response.status, decode_body(response.read())
    except HTTPError as exc:
        return exc.code, decode_body(exc.read())
    except (URLError, TimeoutError, OSError) as exc:
        raise ProbeFailure(f"HTTP request failed ({method} {path})") from exc


class CallbackHandler(BaseHTTPRequestHandler):
    server: CallbackReceiver

    def do_GET(self) -> None:
        parsed = urlsplit(self.path)
        params = parse_qs(parsed.query, keep_blank_values=True)
        state = params.get("state", [""])[0]
        code = params.get("code", [""])[0]
        if parsed.path != "/callback" or state != self.server.expected_state or not code:
            self.send_response(400)
            self.end_headers()
            return
        self.server.received_code = code
        self.server.received_state = state
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.end_headers()
        self.wfile.write(b"callback accepted")

    def log_message(self, _format: str, *_args: object) -> None:
        # BaseHTTPRequestHandler would log callback query strings.
        return


class CallbackReceiver(HTTPServer):
    expected_state: str
    received_code: str | None
    received_state: str | None

    def __init__(self) -> None:
        self.expected_state = ""
        self.received_code = None
        self.received_state = None
        super().__init__(("127.0.0.1", 0), CallbackHandler)


def callback_request(url: str) -> int:
    """Request a local callback without exposing its query in error output."""
    try:
        with urlopen(url, timeout=HTTP_TIMEOUT_SECONDS) as response:
            return response.status
    except HTTPError as exc:
        return exc.code
    except (URLError, TimeoutError, OSError) as exc:
        raise ProbeFailure("Local callback receiver request failed") from exc


def post_json(
    base_url: str,
    path: str,
    payload: dict[str, Any],
    bearer: str | None = None,
) -> tuple[int, Any]:
    return http_request(
        base_url, path, method="POST", json_body=payload, bearer=bearer
    )


def wait_for_home_assistant(base_url: str) -> None:
    deadline = time.monotonic() + READY_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        try:
            status, payload = http_request(base_url, "/api/onboarding")
            if status == 200 and isinstance(payload, list):
                return
        except ProbeFailure:
            pass
        time.sleep(2)
    raise ProbeFailure("Home Assistant did not become ready before timeout")


def login_code(
    base_url: str,
    client_id: str,
    redirect_uri: str,
    handler: list[Any],
    username: str,
    password: str,
) -> tuple[int, Any, str]:
    verifier = secrets.token_urlsafe(64)
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest())
    status, flow = post_json(
        base_url,
        "/auth/login_flow",
        {
            "client_id": client_id,
            "handler": handler,
            "redirect_uri": redirect_uri,
            "response_type": "code",
            "code_challenge": challenge.decode().rstrip("="),
            "code_challenge_method": "S256",
        },
    )
    if status != 200 or not isinstance(flow, dict) or not flow.get("flow_id"):
        return status, flow, verifier
    return post_json(
        base_url,
        f"/auth/login_flow/{flow['flow_id']}",
        {"client_id": client_id, "username": username, "password": password},
    ) + (verifier,)


def check(condition: bool, message: str) -> None:
    if not condition:
        raise ProbeFailure(message)


def run_protocol_probe(base_url: str) -> dict[str, Any]:
    callback = CallbackReceiver()
    callback_thread = threading.Thread(target=callback.serve_forever, daemon=True)
    callback_thread.start()
    try:
        callback_port = callback.server_address[1]
        callback_origin = f"http://127.0.0.1:{callback_port}"
        client_id = f"{callback_origin}/client"
        redirect_uri = f"{callback_origin}/callback"
        username = f"probe_{secrets.token_hex(8)}"
        password = secrets.token_urlsafe(32)

        status, onboarding = http_request(base_url, "/api/onboarding")
        check(status == 200 and isinstance(onboarding, list), "Onboarding status failed")
        check(
            all(not item.get("done") for item in onboarding if isinstance(item, dict)),
            "Disposable instance was not fresh",
        )

        status, user_result = post_json(
            base_url,
            "/api/onboarding/users",
            {
                "name": "Temporary callback probe",
                "username": username,
                "password": password,
                "client_id": client_id,
                "language": "en",
            },
        )
        check(
            status == 200 and isinstance(user_result, dict) and user_result.get("auth_code"),
            "Local onboarding user creation failed",
        )

        status, bootstrap = http_request(
            base_url,
            "/auth/token",
            method="POST",
            form_body={
                "grant_type": "authorization_code",
                "client_id": client_id,
                "code": user_result["auth_code"],
            },
        )
        check(
            status == 200
            and isinstance(bootstrap, dict)
            and bootstrap.get("access_token")
            and bootstrap.get("refresh_token"),
            "Standard onboarding authorization-code exchange failed",
        )

        for path, payload in (
            ("/api/onboarding/core_config", {}),
            ("/api/onboarding/analytics", {}),
            (
                "/api/onboarding/integration",
                {"client_id": client_id, "redirect_uri": redirect_uri},
            ),
        ):
            status, _ = post_json(base_url, path, payload, bootstrap["access_token"])
            check(status == 200, f"Onboarding step failed ({path})")

        status, providers = http_request(base_url, "/auth/providers")
        check(status == 200 and isinstance(providers, dict), "Auth provider discovery failed")
        provider = next(
            (item for item in providers.get("providers", []) if item.get("type") == "homeassistant"),
            None,
        )
        check(provider is not None, "Home Assistant local auth provider missing")
        handler = [provider["type"], provider.get("id")]

        # A different host fails redirect URI validation after the password is
        # checked. The loopback client ID itself remains valid.
        invalid_redirect = f"http://localhost:{callback_port}/callback"
        bad_status, bad_result, _ = login_code(
            base_url,
            client_id,
            invalid_redirect,
            handler,
            username,
            password,
        )
        check(
            bad_status == 403
            and isinstance(bad_result, dict)
            and bad_result.get("message") == "Invalid redirect URI",
            "Mismatched callback origin was not rejected",
        )

        # A separate code proves the server rejects a verifier that does not
        # match its S256 challenge.
        invalid_verifier_status, invalid_verifier_result, valid_verifier = login_code(
            base_url, client_id, redirect_uri, handler, username, password
        )
        check(
            invalid_verifier_status == 200
            and isinstance(invalid_verifier_result, dict)
            and invalid_verifier_result.get("type") == "create_entry"
            and invalid_verifier_result.get("result"),
            "PKCE authorization-code issuance failed",
        )
        invalid_verifier = "x" * len(valid_verifier)
        if invalid_verifier == valid_verifier:
            invalid_verifier = "y" * len(valid_verifier)
        invalid_status, invalid_payload = http_request(
            base_url,
            "/auth/token",
            method="POST",
            form_body={
                "grant_type": "authorization_code",
                "client_id": client_id,
                "code": invalid_verifier_result["result"],
                "code_verifier": invalid_verifier,
            },
        )
        check(
            invalid_status == 400
            and isinstance(invalid_payload, dict)
            and invalid_payload.get("error") == "invalid_grant",
            "Invalid PKCE verifier was not rejected",
        )

        # A fresh code is used for the successful token exchange.
        state = secrets.token_urlsafe(24)
        callback.expected_state = state
        good_status, good_result, verifier = login_code(
            base_url, client_id, redirect_uri, handler, username, password
        )
        check(
            good_status == 200
            and isinstance(good_result, dict)
            and good_result.get("type") == "create_entry"
            and good_result.get("result"),
            "PKCE authorization-code issuance failed",
        )

        callback_url = (
            f"{redirect_uri}?"
            f"{urlencode({'code': good_result['result'], 'state': 'wrong'})}"
        )
        wrong_state_status = callback_request(callback_url)
        check(wrong_state_status == 400, "Local callback accepted a mismatched state")

        callback_url = (
            f"{redirect_uri}?"
            f"{urlencode({'code': good_result['result'], 'state': state})}"
        )
        callback_status = callback_request(callback_url)
        check(
            callback_status == 200
            and callback.received_code == good_result["result"]
            and callback.received_state == state,
            "Local callback did not accept the matching state and code",
        )

        status, tokens = http_request(
            base_url,
            "/auth/token",
            method="POST",
            form_body={
                "grant_type": "authorization_code",
                "client_id": client_id,
                "code": good_result["result"],
                "code_verifier": verifier,
            },
        )
        check(
            status == 200
            and isinstance(tokens, dict)
            and tokens.get("access_token")
            and tokens.get("refresh_token"),
            "Valid S256 token exchange failed",
        )

        api_status, _ = http_request(
            base_url, "/api/", bearer=tokens["access_token"]
        )
        check(api_status == 200, "Issued access token did not authorize the API")

        refresh_status, refreshed = http_request(
            base_url,
            "/auth/token",
            method="POST",
            form_body={
                "grant_type": "refresh_token",
                "client_id": client_id,
                "refresh_token": tokens["refresh_token"],
            },
        )
        check(
            refresh_status == 200
            and isinstance(refreshed, dict)
            and refreshed.get("access_token"),
            "Refresh-token grant failed",
        )

        revoke_status, _ = http_request(
            base_url,
            "/auth/revoke",
            method="POST",
            form_body={"token": tokens["refresh_token"]},
        )
        check(revoke_status == 200, "Refresh-token revocation endpoint failed")

        post_revoke_refresh_status, post_revoke_refresh = http_request(
            base_url,
            "/auth/token",
            method="POST",
            form_body={
                "grant_type": "refresh_token",
                "client_id": client_id,
                "refresh_token": tokens["refresh_token"],
            },
        )
        check(
            post_revoke_refresh_status == 400
            and isinstance(post_revoke_refresh, dict)
            and post_revoke_refresh.get("error") == "invalid_grant",
            "Revoked refresh token remained usable",
        )
        revoked_api_status, _ = http_request(
            base_url, "/api/", bearer=refreshed["access_token"]
        )
        check(revoked_api_status == 401, "Access token remained usable after revocation")

        return {
            "onboarding_user": "passed",
            "local_callback_state_match": "passed",
            "local_callback_state_mismatch": "rejected",
            "invalid_callback_origin": "rejected",
            "invalid_pkce_verifier": "rejected",
            "authorization_code_exchange": "passed",
            "access_token_api_auth": "passed",
            "refresh_token_grant": "passed",
            "refresh_token_revocation": "passed",
            "post_revocation_refresh": "rejected",
            "post_revocation_access_token": "rejected",
            "callback_mode": "API-issued code delivered to a local test receiver; no browser",
        }
    finally:
        callback.shutdown()
        callback.server_close()
        callback_thread.join(timeout=2)


def main() -> int:
    if shutil.which("docker") is None:
        print("Docker CLI is required", file=sys.stderr)
        return 2

    container_name = f"njorun-ha-callback-{secrets.token_hex(6)}"
    run_attempted = False
    config_volume = f"{container_name}-config"
    volume_attempted = False
    result: dict[str, Any] = {}
    try:
        volume_attempted = True
        docker("volume", "create", "--label", "njorun.qualification=ha-callback", config_volume)
        docker("pull", IMAGE)
        repo_digest = docker(
            "image",
            "inspect",
            "--format",
            "{{index .RepoDigests 0}}",
            IMAGE,
        )
        run_attempted = True
        docker(
            "run",
            "--detach",
            "--name",
            container_name,
            "--restart=no",
            "--publish",
            "127.0.0.1::8123",
            "--volume",
            f"{config_volume}:/config",
            "--env",
            "TZ=UTC",
            IMAGE,
        )
        port_binding = docker("port", container_name, "8123/tcp")
        host_port = int(port_binding.rsplit(":", 1)[1])
        base_url = f"http://127.0.0.1:{host_port}"
        wait_for_home_assistant(base_url)
        protocol_results = run_protocol_probe(base_url)
        result = {
            "status": "passed",
            "version": "2026.11.0",
            "image_tag": IMAGE_TAG,
            "image": IMAGE,
            "repo_digest": repo_digest,
            "host_binding": "127.0.0.1 only",
            "protocol": protocol_results,
        }
    except ProbeFailure as exc:
        result = {"status": "blocked", "reason": str(exc)}
    except Exception:
        # Keep unhandled exceptions from printing callback URLs or response data.
        result = {"status": "blocked", "reason": "Unexpected probe error (details suppressed)"}
    finally:
        container_cleanup = "not created"
        if run_attempted:
            try:
                names = docker(
                    "ps",
                    "--all",
                    "--format",
                    "{{.Names}}",
                    timeout=30,
                ).splitlines()
                if container_name in names:
                    docker("rm", "--force", container_name, timeout=30)
                    container_cleanup = "removed"
                else:
                    container_cleanup = "not created"
            except ProbeFailure:
                container_cleanup = "failed"
        result["container_cleanup"] = container_cleanup
        if volume_attempted:
            if container_cleanup == "failed":
                result["config_cleanup"] = "preserved"
                result["config_volume"] = config_volume
            else:
                try:
                    docker("volume", "rm", config_volume, timeout=30)
                except ProbeFailure:
                    result["config_cleanup"] = "failed"
                    result["config_volume"] = config_volume
                else:
                    result["config_cleanup"] = "removed"

    print(json.dumps(result, indent=2, sort_keys=True))
    return (
        0
        if result.get("status") == "passed"
        and result.get("container_cleanup") == "removed"
        and result.get("config_cleanup") == "removed"
        else 1
    )


if __name__ == "__main__":
    raise SystemExit(main())
