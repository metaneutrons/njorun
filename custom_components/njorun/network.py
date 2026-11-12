# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0
"""Network settings per interface (plan 006, decisions N3 and N4).

The same rules as `crates/app-core/src/network.rs`; `protocol/fixtures/network-cases.json`
pins both. Nothing here imports Home Assistant, and no message names a password.
"""

from __future__ import annotations

import ipaddress
import json
import unicodedata
from typing import Any

INTERFACES = ("wifi", "ethernet")
MODES = ("dhcp", "static")
PROOFS = ("session", "link")
REVERT_REASONS = ("no_association", "no_address", "no_home_assistant", "rejected_values")
MAX_OPERATION_ID_BYTES = 128
MAX_REVISION = 4_294_967_295
MAX_RESULT_BYTES = 768


class NetworkError(ValueError):
    """A rejected network configuration; `problem` is the word the fixture uses."""

    def __init__(self, problem: str, message: str) -> None:
        super().__init__(message)
        self.problem = problem


def _fail(problem: str, message: str) -> NetworkError:
    return NetworkError(problem, message)


def _only(value: Any, allowed: set[str], what: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) - allowed:
        raise _fail("parse", f"{what} has an unexpected shape")
    return value


def _address(value: Any, what: str) -> ipaddress.IPv4Address:
    if type(value) is not str:
        raise _fail("parse", f"{what} must be an IPv4 address")
    try:
        return ipaddress.IPv4Address(value)
    except ValueError as err:
        raise _fail("parse", f"{what} must be an IPv4 address") from err


def _unicast_host(address: ipaddress.IPv4Address) -> bool:
    return not (
        address.is_unspecified
        or address.is_loopback
        or address.is_multicast
        or address == ipaddress.IPv4Address("255.255.255.255")
        or address.is_link_local
        or int(address) >> 24 >= 240
    )


def _has_control(text: str) -> bool:
    return any(unicodedata.category(char) == "Cc" for char in text)


def _validate_ssid(ssid: Any) -> None:
    if type(ssid) is not str:
        raise _fail("parse", "The Wi-Fi name must be text")
    if not ssid or len(ssid.encode()) > 32 or _has_control(ssid):
        raise _fail("ssid", "The Wi-Fi name must be 1 to 32 bytes without control characters")


def _validate_password(password: Any) -> None:
    if type(password) is not str:
        raise _fail("parse", "The Wi-Fi password must be text")
    if not password:
        return  # an open network
    size = len(password.encode())
    passphrase = 8 <= size <= 63 and not _has_control(password)
    key = size == 64 and all(char in "0123456789abcdefABCDEF" for char in password)
    if not (passphrase or key):
        raise _fail("password", "The Wi-Fi password must be 8 to 63 characters or 64 hex digits")


def _validate_ip(ip: Any) -> None:
    ip = _only(ip, {"mode", "address", "prefix", "gateway", "dns"}, "The IP setting")
    mode = ip.get("mode")
    if mode not in MODES:
        raise _fail("parse", "The IP mode must be dhcp or static")
    dns = ip.get("dns", [])
    if not isinstance(dns, list):
        raise _fail("parse", "The DNS servers must be a list")
    # Parse everything first, as the tablet does, so a malformed value is a parse error.
    address = _address(ip["address"], "The address") if "address" in ip else None
    gateway = _address(ip["gateway"], "The router") if "gateway" in ip else None
    servers = [_address(server, "A DNS server") for server in dns]
    prefix = ip.get("prefix")
    if "prefix" in ip and (type(prefix) is not int or not 0 <= prefix <= 255):
        raise _fail("parse", "The prefix length must be a number")
    if mode == "dhcp":
        if address is not None or prefix is not None or gateway is not None or servers:
            raise _fail("shape", "DHCP takes no address, prefix, router or DNS server")
        return
    if address is None or prefix is None or gateway is None:
        raise _fail("shape", "A static setup needs an address, a prefix length and a router")
    if not 1 <= prefix <= 30:
        raise _fail("prefix", "The prefix length must be 1 to 30")
    network = ipaddress.IPv4Network((address, prefix), strict=False)
    if (
        not _unicast_host(address)
        or address == network.network_address
        or address == network.broadcast_address
    ):
        raise _fail("address", "The address is not a usable host address")
    if (
        not _unicast_host(gateway)
        or gateway == address
        or gateway not in network
        or gateway in (network.network_address, network.broadcast_address)
    ):
        raise _fail("gateway", "The router must be another host address in the same subnet")
    if len(servers) > 2 or not all(_unicast_host(server) for server in servers):
        raise _fail("dns", "At most two DNS servers, each a unicast address")


def validate_network(config: Any, interfaces: list[str] | tuple[str, ...]) -> dict[str, Any]:
    """Check a network configuration against the interfaces the board has.

    Raises `NetworkError`; returns the configuration unchanged when it is valid.
    """
    config = _only(config, set(INTERFACES), "The network settings")
    if not config:
        raise _fail("empty", "The network settings are empty")
    if "wifi" in config:
        if "wifi" not in interfaces:
            raise _fail("no_such_interface", "This board has no Wi-Fi")
        wifi = _only(config["wifi"], {"ssid", "password", "ip"}, "The Wi-Fi setting")
        if "ssid" not in wifi:
            raise _fail("parse", "The Wi-Fi setting needs a name")
        _validate_ssid(wifi["ssid"])
        if "password" in wifi and wifi["password"] is not None:
            _validate_password(wifi["password"])
        _validate_ip(wifi.get("ip", {"mode": "dhcp"}))
    if "ethernet" in config:
        if "ethernet" not in interfaces:
            raise _fail("no_such_interface", "This board has no Ethernet")
        ethernet = _only(config["ethernet"], {"ip"}, "The Ethernet setting")
        _validate_ip(ethernet.get("ip", {"mode": "dhcp"}))
    return config


def redacted(config: dict[str, Any]) -> dict[str, Any]:
    """A copy that is safe to log or show: the Wi-Fi password is replaced by whether it is set."""
    result = dict(config)
    wifi = result.get("wifi")
    if isinstance(wifi, dict):
        wifi = dict(wifi)
        if wifi.get("password"):
            wifi["password"] = "<set>"
        result["wifi"] = wifi
    return result


def _valid_operation_id(value: Any) -> bool:
    return (
        type(value) is str
        and 0 < len(value.encode()) <= MAX_OPERATION_ID_BYTES
        and not _has_control(value)
    )


def _valid_revision(value: Any) -> bool:
    return type(value) is int and 1 <= value <= MAX_REVISION


def check_network_message(
    message: Any,
    *,
    now: int,
    last_revision: int,
    interfaces: list[str] | tuple[str, ...],
) -> dict[str, Any]:
    """Check a `network` message the way the tablet does (`accept_network` in app-core).

    Raises `NetworkError` whose `problem` is `malformed`, `expired`, `stale` or `invalid`
    (the values do not pass `validate_network`); returns the interface settings otherwise.
    """
    if not isinstance(message, dict):
        raise _fail("malformed", "The network message is not an object")
    known = {"type", "operation_id", "revision", "expires_at", *INTERFACES}
    expires_at = message.get("expires_at")
    if (
        message.get("type") != "network"
        or set(message) - known
        or not _valid_operation_id(message.get("operation_id"))
        or not _valid_revision(message.get("revision"))
        or type(expires_at) is not int
        or expires_at < 0
    ):
        raise _fail("malformed", "The network message has an unexpected shape")
    if expires_at <= now:
        raise _fail("expired", "The network message has expired")
    if message["revision"] < last_revision:
        raise _fail("stale", "The network revision is older than the last finished one")
    config = {key: message[key] for key in INTERFACES if key in message}
    try:
        return validate_network(config, interfaces)
    except NetworkError as err:
        if err.problem == "parse":
            raise _fail("malformed", str(err)) from err
        raise _fail("invalid", str(err)) from err


def build_network_message(
    operation_id: str,
    revision: int,
    expires_at: int,
    config: dict[str, Any],
    interfaces: list[str] | tuple[str, ...],
) -> dict[str, Any]:
    """The `network` message for `config`; raises `NetworkError` for anything the tablet refuses."""
    message = {
        "type": "network",
        "operation_id": operation_id,
        "revision": revision,
        "expires_at": expires_at,
        **config,
    }
    # `now` is below every expiry and `last_revision` below every revision: only shape and
    # values are checked here.
    check_network_message(message, now=-1, last_revision=0, interfaces=interfaces)
    return message


def _interface_state(value: Any, interface: str) -> dict[str, Any]:
    allowed = {"mode", "address", "carries_session"} | ({"ssid"} if interface == "wifi" else set())
    if (
        not isinstance(value, dict)
        or set(value) - allowed
        or not {"mode", "address", "carries_session"} <= set(value)
        or value["mode"] not in MODES
        or type(value["carries_session"]) is not bool
    ):
        raise _fail("parse", "An interface state has an unexpected shape")
    if value["address"] is not None:
        _address(value["address"], "The address")
    if "ssid" in value:
        _validate_ssid(value["ssid"])
    return value


def parse_network_result(payload: Any, interfaces: list[str] | tuple[str, ...]) -> dict[str, Any]:
    """Check a `network_result` from a tablet; raises `NetworkError` when it is not valid."""
    if not isinstance(payload, dict) or payload.get("type") != "network_result":
        raise _fail("parse", "Not a network result")
    result = payload.get("result")
    extra = {"applied": {"proof"}, "reverted": {"reason"}}.get(result)
    if extra is None:
        raise _fail("parse", "The network result must be applied or reverted")
    if set(payload) != {"type", "operation_id", "revision", "result", "state"} | extra:
        raise _fail("parse", "The network result has an unexpected shape")
    if not _valid_operation_id(payload["operation_id"]) or not _valid_revision(payload["revision"]):
        raise _fail("parse", "The network result has no valid operation or revision")
    if result == "applied" and payload["proof"] not in PROOFS:
        raise _fail("parse", "The proof must be session or link")
    if result == "reverted" and payload["reason"] not in REVERT_REASONS:
        raise _fail("parse", "The reason is not one the tablet reports")
    state = payload["state"]
    if not isinstance(state, dict) or set(state) - set(interfaces):
        raise _fail("parse", "The network state names an interface the tablet does not have")
    for interface, value in state.items():
        _interface_state(value, interface)
    if len(json.dumps(payload, separators=(",", ":"), ensure_ascii=False).encode()) > MAX_RESULT_BYTES:
        raise _fail("parse", "The network result is too large")
    return payload
