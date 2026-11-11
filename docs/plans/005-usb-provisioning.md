# First setup over USB-C

A tablet gets its network and its Home Assistant address over the USB-C cable,
without touching the screen. This follows the way Vellum does it on the same
hardware (Improv Wi-Fi Serial with extension fields, see
`firmware/components/vellum_serial` there) and it is the first part of the USB
provisioning that the [blueprint](001-embedded-ha-blueprint.md) names under
"Persistent trust, profiles and USB recovery" (M2H-A9). The first screens of the
tablet stay as the fallback.

## Outcome

An administrator connects the tablet with a cable, runs a tool on the computer
and gives it the Wi-Fi name and password and the Home Assistant address. The tablet
joins the network and starts to pair with Home Assistant (plan
[004](004-tablet-first-pairing.md)). The Wi-Fi password never appears on a command
line or in a log.

## Decisions for review

- **U1 Improv Serial with Vellum's extension.** The frames are those of the Improv
  Wi-Fi Serial specification: `IMPROV`, version 1, type, length, payload and an
  8-bit sum. `WIFI_SETTINGS` carries the SSID and the password and then optional
  strings by position: the Home Assistant address, a voucher, an NTP server and the
  current UTC time as Unix seconds, in Vellum's order, so that one client can talk to
  both products. Later strings need empty placeholders before them. The payload is at
  most 253 bytes. The protocol is reimplemented; no Vellum code is copied. The
  [test vectors](../../protocol/fixtures/improv-frames.json) were written by Vellum's
  client and checked with its parser.
- **U2 What the first slice does.** The commands `GET_STATE`, `GET_DEVICE_INFO`,
  `SCAN_WIFI` and `WIFI_SETTINGS` with the SSID, password, Home Assistant address
  and UTC time. A voucher or an NTP server in the profile is refused with
  `INVALID_RPC`, so that nothing a caller sent is silently dropped. The device info
  is `Njörun`, the firmware version, `ESP32-P4` and `reTerminal D1001`.
- **U3 What is protected.** Reading (state, info, scan) is always open. A profile is
  accepted only while the tablet has no binding to Home Assistant. Afterwards
  `WIFI_SETTINGS` answers `AUTH_REQUIRED`; changing the network of a paired tablet
  needs the protected re-provisioning of a later slice. Until then a paired tablet is
  set up again by erasing its storage. This is Vellum's fail-closed rule.
- **U4 What the screen does.** After a profile has joined the network, the setup
  skips the Wi-Fi and address pages: with an address the tablet pairs at once, and
  without one it looks for Home Assistant as after the Wi-Fi page.
- **U5 The address is checked like the typed one.** A profile whose address is not a
  valid `http://` or `https://` base URL is refused with `INVALID_RPC`.
- **U6 The tool.** `scripts/usb-provision.py` needs no packages. It lists the
  networks the tablet sees, shows the device info and sends a profile. The password
  comes from the environment or a hidden prompt, never from an argument. It sets the
  tablet's clock from the computer's.

## Later slices

- **Voucher.** Home Assistant issues a one-time voucher. The tablet sends it with its
  announcement, and Home Assistant approves it without a card and without the
  comparison code, because the USB cable is the authenticated channel (M2H-A9). The
  voucher crosses the cable in clear text, as in Vellum; it is single use, short lived
  and approves a new tablet only.
- **Protected re-provisioning.** A challenge and an HMAC that Home Assistant derives
  from the keys the paired tablet and the binding share, as Vellum does with its
  device token. The browser never sees the key.
- **Static addresses and more interfaces.** Further positional strings or a command of
  our own for the IP mode (DHCP or static address, netmask, gateway, DNS) per
  interface. The D1001 has Wi-Fi only; the device reports which interfaces exist so
  that a board with Ethernet needs no new protocol.
- **NTP server and the RTC.** The NTP string, and writing the time to the D1001's
  real-time clock.
- **Browser.** The same frames over Web Serial, as a panel of the integration (which
  has the administrator session for the voucher and the authorization) or in the
  installer page.

## Verified and not verified

Read in Vellum (its README, `vellum_serial.c`, `improv-serial.ts` and its tests):
the frame layout, the positional strings and their limits, the states and error
codes, the lock after the first enrollment, and that the D1001 uses its native
USB-Serial-JTAG port, that opening the port restarts the device and that frames and
console text share one stream.

Not verified in this repository before the work: that the running application can
read bytes on that port (the first slice reads with the USB-Serial-JTAG driver),
that the tool can open the port on macOS without putting the tablet into the
download mode, and what the chip does with a time set by `settimeofday` when the
RTC is not written.

## Risks

- The Wi-Fi password and later the voucher cross the cable in clear text. A cable
  held by the administrator during setup is the assumption, as in Vellum.
- Frames and log lines share one stream. A frame written between two halves of a log
  line is rare, and the parsers of both sides resynchronize on the magic and the sum.
