#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: GPL-3.0-or-later
"""First setup of a tablet over USB-C (plan 005): Improv Wi-Fi Serial with the extension strings.

    scripts/usb-provision.py info
    scripts/usb-provision.py scan
    NJORUN_WIFI_PASSWORD=... scripts/usb-provision.py provision --ssid Home --url https://ha.example.org

The Wi-Fi password is read from NJORUN_WIFI_PASSWORD or asked for without echo; it is never an
argument, so it stays out of the shell history and the process list. `--open` is for a network
without a password. The tool needs no packages; it uses termios (macOS, Linux).
"""

from __future__ import annotations

import argparse
import getpass
import glob
import os
import select
import sys
import time

MAGIC = b"IMPROV"
VERSION = 1
CURRENT_STATE, ERROR_STATE, RPC_COMMAND, RPC_RESULT = 1, 2, 3, 4
WIFI_SETTINGS, GET_STATE, GET_DEVICE_INFO, SCAN_WIFI = 1, 2, 3, 4
STATE_NAMES = {2: "ready", 3: "provisioning", 4: "provisioned"}
ERROR_NAMES = {
    0: "none",
    1: "the tablet did not understand the request (or refused a field)",
    2: "unknown command",
    3: "the tablet could not join the network",
    4: "the address is not allowed",
    5: "the tablet is already paired with Home Assistant, erase it to set it up again",
    6: "authorization failed",
    7: "authorization expired",
}
MAX_SETTINGS_BYTES = 253


def encode_frame(frame_type: int, payload: bytes) -> bytes:
    if len(payload) > 255:
        raise ValueError("the payload does not fit one frame")
    frame = MAGIC + bytes([VERSION, frame_type, len(payload)]) + payload
    return frame + bytes([sum(frame) & 0xFF])


def rpc_command(command: int, data: bytes = b"") -> bytes:
    return encode_frame(RPC_COMMAND, bytes([command, len(data)]) + data)


def _string(text: str) -> bytes:
    raw = text.encode("utf-8")
    if len(raw) > 255:
        raise ValueError("a string is longer than 255 bytes")
    return bytes([len(raw)]) + raw


def wifi_settings_payload(
    ssid: str,
    password: str,
    url: str | None = None,
    voucher: str | None = None,
    ntp: str | None = None,
    utc: int | None = None,
) -> bytes:
    """SSID and password, then the optional strings by position (empty placeholders before a later one)."""
    payload = _string(ssid) + _string(password)
    has_time = utc is not None
    if url or voucher or ntp is not None or has_time:
        payload += _string(url or "")
    if voucher or ntp is not None or has_time:
        payload += _string(voucher or "")
    if ntp is not None or has_time:
        payload += _string(ntp or "")
    if has_time:
        payload += _string(str(int(utc)))
    if len(payload) > MAX_SETTINGS_BYTES:
        raise ValueError(f"the profile is too long ({len(payload)} of {MAX_SETTINGS_BYTES} bytes)")
    return payload


def wifi_settings(ssid: str, password: str, **fields) -> bytes:
    return rpc_command(WIFI_SETTINGS, wifi_settings_payload(ssid, password, **fields))


class Parser:
    """Finds frames in a stream that also carries log text."""

    def __init__(self) -> None:
        self.buffer = bytearray()

    def push(self, data: bytes) -> list[tuple[int, bytes]]:
        self.buffer += data
        frames = []
        while True:
            start = self._start()
            if start is None:
                del self.buffer[: max(0, len(self.buffer) - (len(MAGIC) - 1))]
                break
            del self.buffer[:start]
            if (len(self.buffer) > 6 and self.buffer[6] != VERSION) or (
                len(self.buffer) > 7 and self.buffer[7] not in (1, 2, 3, 4)
            ):
                del self.buffer[:1]
                continue
            if len(self.buffer) < 10:
                break
            total = 10 + self.buffer[8]
            if len(self.buffer) < total:
                break
            if sum(self.buffer[: total - 1]) & 0xFF == self.buffer[total - 1]:
                frames.append((self.buffer[7], bytes(self.buffer[9 : total - 1])))
                del self.buffer[:total]
            else:
                del self.buffer[:1]
        if len(self.buffer) > 520:
            del self.buffer[: len(self.buffer) - 520]
        return frames

    def _start(self) -> int | None:
        for start in range(len(self.buffer)):
            tail = self.buffer[start : start + len(MAGIC)]
            if bytes(tail) == MAGIC[: len(tail)]:
                return start
        return None


def decode_result(payload: bytes) -> tuple[int | None, list[str]]:
    """An RPC_RESULT payload: the command and its length-prefixed strings."""
    if not payload:
        return None, []
    command = payload[0]
    end = min(2 + (payload[1] if len(payload) > 1 else 0), len(payload))
    strings, position = [], 2
    while position < end:
        length = payload[position]
        position += 1
        strings.append(payload[position : position + length].decode("utf-8", "replace"))
        position += length
    return command, strings


def open_port(path: str) -> int:
    import termios

    descriptor = os.open(path, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    attributes = termios.tcgetattr(descriptor)
    attributes[0] = 0  # iflag
    attributes[1] = 0  # oflag
    attributes[2] = termios.CS8 | termios.CREAD | termios.CLOCAL  # cflag
    attributes[3] = 0  # lflag
    attributes[4] = attributes[5] = termios.B115200  # the native USB port ignores the speed
    attributes[6][termios.VMIN] = 0
    attributes[6][termios.VTIME] = 0
    termios.tcsetattr(descriptor, termios.TCSANOW, attributes)
    return descriptor


def find_port(requested: str | None) -> str:
    if requested:
        return requested
    ports = sorted(glob.glob("/dev/cu.usbmodem*") + glob.glob("/dev/ttyACM*"))
    if len(ports) != 1:
        raise SystemExit(
            "found " + (f"{len(ports)} USB serial ports: " + ", ".join(ports) if ports else "no USB serial port")
            + "; connect one tablet or pass --port"
        )
    return ports[0]


class Link:
    def __init__(self, descriptor: int, verbose: bool) -> None:
        self.descriptor = descriptor
        self.parser = Parser()
        self.verbose = verbose
        self.pending: list[tuple[int, bytes]] = []

    def send(self, frame: bytes) -> None:
        os.write(self.descriptor, frame)

    def frames(self, timeout: float):
        """Frames that arrive within `timeout` seconds; log text is shown with --verbose."""
        deadline = time.monotonic() + timeout
        while True:
            while self.pending:
                yield self.pending.pop(0)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return
            ready, _, _ = select.select([self.descriptor], [], [], min(remaining, 0.25))
            if not ready:
                continue
            try:
                data = os.read(self.descriptor, 4096)
            except BlockingIOError:
                continue
            if self.verbose:
                sys.stderr.buffer.write(data)
            self.pending.extend(self.parser.push(data))

    def wait_ready(self, seconds: float = 25.0) -> bool:
        """Opening the port can restart the tablet, so ask until it answers."""
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.send(rpc_command(GET_STATE))
            for frame_type, _payload in self.frames(1.0):
                if frame_type == CURRENT_STATE:
                    return True
        return False


def command_info(link: Link) -> int:
    link.send(rpc_command(GET_DEVICE_INFO))
    for frame_type, payload in link.frames(5.0):
        if frame_type == RPC_RESULT:
            command, strings = decode_result(payload)
            if command == GET_DEVICE_INFO:
                labels = ["device", "firmware", "chip", "board"]
                for label, value in zip(labels, strings):
                    print(f"{label}: {value}")
                return 0
    print("the tablet did not answer", file=sys.stderr)
    return 1


def command_scan(link: Link) -> int:
    link.send(rpc_command(SCAN_WIFI))
    found = 0
    for frame_type, payload in link.frames(20.0):
        if frame_type != RPC_RESULT:
            continue
        command, strings = decode_result(payload)
        if command != SCAN_WIFI:
            continue
        if not strings:
            return 0
        ssid, rssi, secured = (strings + ["", "", ""])[:3]
        print(f"{ssid}\t{rssi} dBm\t{'password' if secured == 'YES' else 'open'}")
        found += 1
    print(f"the list did not end ({found} networks)", file=sys.stderr)
    return 1


def command_provision(link: Link, args: argparse.Namespace) -> int:
    if args.open:
        password = ""
    else:
        password = os.environ.get("NJORUN_WIFI_PASSWORD")
        if password is None:
            password = getpass.getpass(f"Wi-Fi password for {args.ssid}: ")
    utc = None if args.no_time else int(time.time())
    link.send(wifi_settings(args.ssid, password, url=args.url or None, utc=utc))
    for frame_type, payload in link.frames(args.timeout):
        if frame_type == CURRENT_STATE and payload:
            print("state:", STATE_NAMES.get(payload[0], payload[0]))
            if payload[0] == 4:
                continue
        elif frame_type == ERROR_STATE and payload and payload[0] != 0:
            print("error:", ERROR_NAMES.get(payload[0], payload[0]), file=sys.stderr)
            return 1
        elif frame_type == RPC_RESULT:
            command, strings = decode_result(payload)
            if command == WIFI_SETTINGS:
                print("done, the tablet is on the network" + (f": {strings[0]}" if strings else ""))
                return 0
    print("the tablet did not finish in time", file=sys.stderr)
    return 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--port", help="serial port (default: the only USB serial port)")
    parser.add_argument("--verbose", action="store_true", help="show the tablet's log text on stderr")
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("info", help="show the device information")
    sub.add_parser("scan", help="list the Wi-Fi networks the tablet sees")
    provision = sub.add_parser("provision", help="send the Wi-Fi network and the Home Assistant address")
    provision.add_argument("--ssid", required=True)
    provision.add_argument("--url", help="Home Assistant address, https://... or http://...")
    provision.add_argument("--open", action="store_true", help="the network has no password")
    provision.add_argument("--no-time", action="store_true", help="do not set the tablet's clock")
    provision.add_argument("--timeout", type=float, default=60.0, help="seconds to wait for the connection")
    args = parser.parse_args(argv)

    descriptor = open_port(find_port(args.port))
    link = Link(descriptor, args.verbose)
    try:
        if not link.wait_ready():
            print("the tablet does not answer on this port", file=sys.stderr)
            return 1
        if args.command == "info":
            return command_info(link)
        if args.command == "scan":
            return command_scan(link)
        return command_provision(link, args)
    finally:
        os.close(descriptor)


if __name__ == "__main__":
    raise SystemExit(main())
