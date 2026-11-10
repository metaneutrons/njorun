# D1001 management link to the Home Assistant integration

This delivery connects the D1001 firmware to the Njörun Home Assistant
integration. It implements part of
[M2H](001-embedded-ha-blueprint.md#m2h-native-home-assistant-device-management),
tracked by [#5](https://github.com/metaneutrons/njorun/issues/5). The
[blueprint](001-embedded-ha-blueprint.md) remains the acceptance authority; this
plan owns the slice order, design boundaries and risks only.

## Baseline (2026-11-08)

- The host simulator and the development integration pass the management
  vertical slice ([002](002-host-vertical-slice.md), PR #51): enrollment,
  Noise IK session, shared profiles, overrides, applied acknowledgements, reload
  and revocation.
- The D1001 firmware ([PR #58](https://github.com/metaneutrons/njorun/pull/58))
  commissions, restores its setup from NVS and runs the live dashboard over
  HA's standard WebSocket API. It does not use the `njorun/*` commands.
- The firmware has local orientation and theme editors and reads its log sink
  from the helper `input_text.njorun_log_sink`. Both are interim and conflict
  with the configuration decision (PR #39) until this plan lands.
- Measured on the D1001 after the HA snapshot: about 67 KB internal RAM free,
  largest block 34.8 KB, low-water 24 KB. The application uses 91 % of its
  8 MiB OTA slot.

## Outcome

An administrator pairs a D1001 in Home Assistant. From then on, every panel
setting the blueprint assigns to HA is changed there, applied by the tablet,
acknowledged with the applied value and kept across restarts and HA outages.
The tablet has no local preference editors after pairing.

## Non-goals

Browser/USB installation and recovery (M2H-A9, A10, A16), PIN lock (A8),
backup/restore (A13), replacement and profile deletion (A14, A15) and OTA (M3)
keep their own work. The interim pairing in S4 must not be presented as the
browser/USB trust ceremony.

## Decisions for review

These are proposed in the same pull request as this plan and recorded in the
blueprint's decision log once accepted.

- **D1 Remote logging is an integration setting.** One installation-wide option
  in the integration ("Remote logging": off or a Loki push URL), delivered to
  every enrolled tablet with its configuration. Off by default. The helper stays
  only until S6 and is then removed. New criterion M2H-A17.
  *Implemented 2026-11-09:* the option is in the integration's options ("Remote
  logging") and travels as `log_sink` with every configuration. The helper path is
  gone. A previous boot's log is replayed only after a crash (panic, watchdog or CPU
  lockup), not after a normal restart or update.
- **D2 Orientation is chosen once during first setup, then owned by HA.** The
  mounting orientation must be readable before HA exists, so commissioning
  keeps the orientation choice. After pairing, orientation and theme are
  changed only in HA; the tablet's editors are removed in S5.
- **D3 Interim pairing by comparison code.** Until the browser/USB ceremony
  exists, the tablet announces its public key over the authenticated HA
  connection and shows a short code derived from both public keys. An
  administrator approves the pending tablet in the integration after comparing
  the code. Labelled interim; it does not satisfy M2H-A9 or A10.
- **D4 Four rotations as a capability.** Replace `orientation`
  (`portrait`/`landscape`) by `rotation` (0, 90, 180, 270) with the qualified
  set reported in the tablet's capabilities, as the blueprint already allows.
  The D1001 reports all four once each is qualified on hardware.
  *Implemented 2026-11-09:* kept the setting name `orientation` and widened its
  values to `portrait`, `landscape`, `portrait_flipped` and `landscape_flipped`
  (the keys the firmware already used); a tablet offers only the values in its
  reported capabilities. Existing enrollments keep their two reported values until
  the tablet is approved again.

- **D5 The Home Assistant surface follows the device, not a menu.** A tablet
  that asks for pairing appears as a **Discovered** card under Settings →
  Devices & services; confirming it (after comparing the code) approves it, and
  a profile is asked only when more than one exists. A tablet's own settings
  are entities on its device page: setting one stores a per-tablet override, the
  button "Use profile values" clears the overrides, and a `Profile` select
  (configuration category) moves the tablet to another profile. Deleting the
  device removes the tablet. The integration's gear (options) manages shared
  profiles only. This replaces the options-flow steps to approve, change,
  reset, move and remove a tablet. The criteria keep their meaning; only the
  place where each action happens changes (see the blueprint's decision log).
  *Implemented 2026-11-09 (UX stage 2, [#78](https://github.com/metaneutrons/njorun/issues/78)).*

- **D6 Live commands and telemetry on the Noise session.** Three encrypted
  message types, `command` (HA to tablet), `command_result` and `telemetry`
  (tablet to HA), give the integration the wake button, the backlight light and
  the diagnostic sensors of [#63](https://github.com/metaneutrons/njorun/issues/63).
  Commands (`wake`, `backlight`) are transient: never persisted, never replayed
  after a reconnect, expired after 10 s, and a repeated `command_id` is rejected.
  Telemetry carries firmware version, uptime, Wi-Fi signal, free heap, last reset
  reason and the backlight state, no secrets, on change and every 60 s. The
  entities are unavailable while no session is live and are gated by the `live`
  capability the tablet reports. *Separation:* the backlight light is a transient
  live state. It is not the persistent `brightness` setting of the profile. A
  light command never writes a setting or a revision; after a restart and after
  every newly applied configuration the display falls back to the `brightness`
  setting. The `brightness` number keeps editing the setting. Protocol in
  [002](../protocols/002-ha-management.md#live-commands-and-telemetry).

- **D7 Firmware distribution: GitHub Releases, relayed by Home Assistant.**
  A release job builds one set of artifacts per board and hardware revision: a
  merged factory image (bootloader, partition table, first application), the
  application image for the OTA slots, and a manifest with its Ed25519 signature.
  The manifest binds board ID, revisions, partition layout ID, coprocessor
  compatibility, channel, source SHA, size and SHA-256 of each image. The firmware
  carries two public keys (current and next) for rotation; the private key stays
  in CI secrets or offline. *Initial installation:* ESP Web Tools on a
  project-owned HTTPS page flashes the factory image over Web Serial, and the
  integration links to it; flashing from the HA frontend follows later. Whether
  esptool-js supports the ESP32-P4 is not verified and gates this path.
  *OTA:* the integration downloads manifest and image from GitHub, verifies
  signature, digest and board identity, caches the image and serves it from an
  authenticated endpoint with range support. The install command on the Noise
  session carries version, size, digest, the URL on HA and a single-use token
  that expires with the command. The tablet pulls the image from HA, streams it
  into the inactive slot, verifies signature and digest itself and reports
  progress, reboot and rollback. The tablet needs no Internet access and no TLS
  stack for GitHub; the transport may be plain HTTP in the LAN because integrity
  rests on the signature. Pushing the image through the Noise session is
  rejected: frames are limited to 4096 bytes of plaintext (about 2000 frames for
  8 MiB), the management channel would be blocked and a resume is awkward.
  *Before the first public release:* decide the slot size (the CI measures 4.07 %
  headroom, [#81](https://github.com/metaneutrons/njorun/issues/81)) and whether
  to enable secure boot and flash encryption, because both need a USB reflash
  afterwards. The mechanism is built under
  [M3 (#6)](https://github.com/metaneutrons/njorun/issues/6).

## Slices

| Slice | Outcome | Depends on | Criteria |
| --- | --- | --- | --- |
| [S2](https://github.com/metaneutrons/njorun/issues/59) Device identity | Static X25519 key and tablet ID generated on the tablet from the hardware RNG, stored in NVS, wiped by factory reset; persistence documented | D1–D4 accepted | A1, A3 |
| [S3](https://github.com/metaneutrons/njorun/issues/60) Shared protocol core | Session and configuration logic shared by the host client and the firmware, transport-independent; Snow builds for the P4; internal-RAM cost measured | [#59](https://github.com/metaneutrons/njorun/issues/59) | A1, A4 |
| [S4](https://github.com/metaneutrons/njorun/issues/61) Pairing and session on the D1001 | Announcement, comparison code, administrator approval, Noise session over the firmware's HA WebSocket, reconnect with a fresh handshake, rejection of revoked bindings | [#60](https://github.com/metaneutrons/njorun/issues/60) | A1, A3 |
| [S5](https://github.com/metaneutrons/njorun/issues/62) Applied settings | Rotation, theme, language, brightness, idle timeout and brightness, screensaver, home view, favorites, room and entity order, exclusions applied, persisted and acknowledged; revision checks; cached settings when offline; local editors removed | [#61](https://github.com/metaneutrons/njorun/issues/61) | A2, A4–A7, A11, A12 |
| [S6](https://github.com/metaneutrons/njorun/issues/63) Integration surface and logging | Native entities per tablet (light, selects, numbers, wake button, diagnostics), remote-logging option, firmware takes the sink from its configuration, helper removed; installation path documented | [#62](https://github.com/metaneutrons/njorun/issues/62) | A1, A2, A17 |
| [S7](https://github.com/metaneutrons/njorun/issues/64) Hardware qualification | Evidence on a D1001 against a disposable HA for the criteria above, recorded in #5 against an exact plan revision | [#63](https://github.com/metaneutrons/njorun/issues/63) | as listed |

## Risks

- Internal RAM: TLS, the WebSocket and the Noise session share about 67 KB.
  S3 measures before S4 builds on it; the fallback is moving buffers to PSRAM.
- Flash: 9 % headroom in the OTA slot. S3 records the size cost of Snow.
- The integration's `noiseprotocol` responder has no independent security
  review (see [002-ha-management](../protocols/002-ha-management.md)).
- Pairing UX: D3 relies on the administrator comparing the code.
