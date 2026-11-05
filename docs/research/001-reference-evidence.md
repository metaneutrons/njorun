# Reference evidence review

Research date: 2026-11-05. This is a source and documentation review for
[blueprint 001](../plans/001-embedded-ha-blueprint.md). It records inspected
implementations and bounded next experiments. No Njörun firmware was built,
flashed or measured during this review. Upstream test evidence has a different
scope from qualification on Fabian's D1001.

## Scope and supplied references

All seven supplied references are accounted for below. Local Vellum was inspected
read-only. Remote source claims use pinned revisions where available. Vendor
pages describe advertised hardware and still require checking the actual unit.

| Supplied reference | Research role and limitation |
| --- | --- |
| `~/Source/Vellum` | D1001 platform and device lifecycle source. Local revision recorded below. The checkout is an engineering reference outside this repository. |
| [Seeed D1001 product](https://www.seeedstudio.com/reTerminal-D1001-p-6729.html) | Hardware identity, supplemented by vendor wiki and BSP source. |
| [HA Companion documentation](https://companion.home-assistant.io/) | User-facing authentication and app behavior, supplemented by app implementation. |
| [HA iOS](https://github.com/home-assistant/iOS) | Browser authorization, session and dashboard implementation. |
| [HA Android](https://github.com/home-assistant/android) | Equivalent lifecycle and implementation boundaries. |
| [EspControl](https://github.com/jtenniswood/espcontrol) | Embedded UI, HA transport, board profiles and browser installation. Previously omitted from the blueprint inventory. |
| [Guition listing](https://de.aliexpress.com/item/1005010023056143.html) | Requested JC1060P470C-I-W-Y variant. Listing could not be retrieved in this review; manufacturer documentation and code for related boards do not prove the delivered revision. |

## Platform and renderer evidence

### D1001 hardware and vendor baseline

Seeed's wiki specifies 32 MB flash and PSRAM, 800 × 1280 MIPI-DSI display,
GSL3670 touch and SC2356 MIPI-CSI camera. The BSP at
[5074d3b2](https://github.com/Seeed-Studio/reTerminal-D1001/tree/5074d3b2f45626b261298e305aaf792036febc5a)
implements JD9365 panel creation and GSL3670 touch initialization. Its
[board header](https://github.com/Seeed-Studio/reTerminal-D1001/blob/5074d3b2f45626b261298e305aaf792036febc5a/components/esp32_p4_re_terminal_d1001/include/esp32_p4_re_terminal_d1001.h)
sets the touch/camera I2C bus to SCL 38 / SDA 37, touch IRQ 16 and reset through
expander bit 12. The
[BSP implementation](https://github.com/Seeed-Studio/reTerminal-D1001/blob/5074d3b2f45626b261298e305aaf792036febc5a/components/esp32_p4_re_terminal_d1001/src/esp32_p4_re_terminal_d1001.c)
is the source for power sequencing, scanout and input, rather than inventing
drivers from product descriptions.

The [vendor README](https://github.com/Seeed-Studio/reTerminal-D1001/blob/5074d3b2f45626b261298e305aaf792036febc5a/README.md)
uses IDF 5.4.2. Its BSP depends on LVGL 8/9 and esp_lvgl_port 2.x. The factory
manifest includes ESP-Hosted 2.x, esp_wifi_remote 1.x and esp_video 1.2.x.
These ranges are not a demonstrated IDF 6.1 combination. Start from the relevant
driver paths and resolve an explicit component set. Camera driver availability
supports feasibility research, but says nothing about motion detection quality
or simultaneous camera/display/network resource use.
[Vendor wiki](https://wiki.seeedstudio.com/getting_started_with_reterminal_d1001/)

### Rust and ESP-IDF

The latest-release endpoint for espressif/esp-idf reports v6.1, published
2026-08-27, as of this review. Keep the accepted latest-stable policy and pin
the resolved version during bootstrap.
[ESP-IDF v6.1](https://github.com/espressif/esp-idf/releases/tag/v6.1)

The maintained bindings are in the
[esp-rs/esp-idf monorepo at 64f82995](https://github.com/esp-rs/esp-idf/tree/64f829959cfe2771b1168d2790939fcc050372ab).
The old sys and svc repositories are archived. Use the monorepo and its crate
versions when researching compatibility. Its README explicitly notes missing
hardware-in-loop tests. Its CI matrix covers the P4's riscv32imafc-esp-espidf
target with IDF 6.1. The relevant job passed on 2026-11-04 at this exact revision,
although the overall workflow failed on other matrix entries. This establishes
upstream compilation evidence, not D1001 runtime support.
[CI matrix](https://github.com/esp-rs/esp-idf/blob/64f829959cfe2771b1168d2790939fcc050372ab/.github/workflows/ci.yml)
[Passed P4-target job](https://github.com/esp-rs/esp-idf/actions/runs/41830408540/job/126575840031)

### Slint and LVGL boundary

At [Slint b6b1b86c](https://github.com/slint-ui/slint/tree/b6b1b86c8c57a74400dfedb0fa98b501f7a642e5),
the documented ESP-IDF integration is a C++ component and says it has been tested
on S3. The platform source nevertheless contains MIPI-DSI synchronization,
touch polling fallback and framebuffer handling. This is a useful integration
reference, not proof of a Rust-first D1001 application backend.
[ESP-IDF guide](https://github.com/slint-ui/slint/blob/b6b1b86c8c57a74400dfedb0fa98b501f7a642e5/docs/cpp/src/content/docs/mcu/esp-idf.mdx)
[Platform source](https://github.com/slint-ui/slint/blob/b6b1b86c8c57a74400dfedb0fa98b501f7a642e5/api/cpp/esp-idf/slint/src/slint-esp.cpp)

The virtual keyboard has a Rust example and Slint component. Reuse remains
plausible, with password/URL entry, focus, layout and rotation to qualify on
hardware. Slint's license inventory distinguishes framework licensing from
MIT examples and separately licensed assets. Resolve the exact dependency
license set before importing.
[Keyboard](https://github.com/slint-ui/slint/tree/b6b1b86c8c57a74400dfedb0fa98b501f7a642e5/examples/virtual_keyboard)
[License inventory](https://github.com/slint-ui/slint/blob/b6b1b86c8c57a74400dfedb0fa98b501f7a642e5/LICENSE.md)

Retain Slint as a candidate. Use Seeed's LVGL implementation as the reference
baseline for known board integration. The choice requires a bounded comparison
of Rust integration effort, rendering/input correctness, memory and frame timing.
Neither framework wins from source inspection alone.

## Browser installation and trust boundary

ESP Web Tools at
[b027c3c9](https://github.com/esphome/esp-web-tools/tree/b027c3c939dd660e1f7abb7efd6404839453e759)
checks Web Serial availability and secure context. Its manifest supports P4.
The flash path selects a build by chip family and serial type, fetches image
parts, optionally erases and writes them through esptool-js. The inspected path
does not implement Njörun's signed-release verification or distinguish two
different board models with the same P4. Add those checks before any erase/write,
and flash the exact verified bytes instead of fetching mutable URLs again.
[Install button](https://github.com/esphome/esp-web-tools/blob/b027c3c939dd660e1f7abb7efd6404839453e759/src/install-button.ts)
[Flash path](https://github.com/esphome/esp-web-tools/blob/b027c3c939dd660e1f7abb7efd6404839453e759/src/flash.ts)

HA can fetch release artifacts server-side to avoid cross-origin browser
downloads. Plain-HTTP HA needs a secure-context installer tab for USB.
Firmware signatures authenticate image bytes. They do not authenticate an HA
key delivered for the first time through an HTTP page. The bootstrap needs
an independently authenticated HA identity or an explicit out-of-band
verification ceremony. Merely relaying encrypted messages and checking window
origins does not solve first-pairing impersonation. This is an unresolved
security design gate, not a reason to silently drop the requested HTTP flow.
[ESP Web Tools embedding requirements](https://esphome.github.io/esp-web-tools/)

The custom provisioning protocol starts after flashing and reconnecting to the
application serial interface. ROM flashing support does not establish that this
interface, framing, authorization or recovery protocol already exists.

## OTA evidence

ESP-IDF 6.1 documents two OTA application slots plus otadata, pending-image
verification, explicit application confirmation and boot rollback. Bootloader
and partition-table updates have different power-loss properties. Retain the
MVP application-only OTA boundary. Signing, board eligibility and safe settings
migration remain application responsibilities. Runtime health confirmation
must be qualified through faults and power interruption on the board.
[ESP-IDF P4 OTA](https://docs.espressif.com/projects/esp-idf/en/v6.1/esp32p4/api-reference/system/ota.html)

## Guition manufacturer evidence

The manufacturer's JC1060P470C_I_W_Y PDF lists 1024 × 600, JD9165, 16 MB flash,
32 MB PSRAM and capacitive touch for the C variant. It does not establish the
exact touch/radio wiring needed by our board profile. Its prose describes
integrated wireless in a way that must not be read as P4 having its own radio.
The exact unit and coprocessor wiring must be verified.
[Manufacturer PDF](https://www.guition.com/hr/icms/upload/fb081940d6fc11f09850077a33e1404f/FTPData/UEditor/file/2026121/1768961093646/JC1060P470C_I_W_Y%20Specifications-EN.pdf)

## Vellum implementation findings

Inspected local revision `f5075a36d6e477ef724ad0091c48999de655ea16`.
Paths below are relative to the supplied Vellum checkout. They are reproducible
source pointers, not public links or evidence of tests run in this review.

| Concern | Inspected path | Finding and Njörun consequence |
| --- | --- | --- |
| Display and radio | `firmware/components/vellum_display/panel_lcd.c:160`, `firmware/sdkconfig.defaults.p4:49` | JD9365 MIPI-DSI and ESP-Hosted/SDIO provide concrete board precedent. The inspected firmware has touch pin definitions but no operational touch path. Use Seeed for touch. |
| USB authorization | `firmware/components/vellum_serial/vellum_serial.c:82`, `:411`; `src/lib/provisioning/usb-authorization.ts:21` | One-shot 16-byte challenge, 120-second lifetime and HMAC over an exact payload digest authorize a particular mutation. Adapt the bounded operation/replay-defense pattern. |
| USB confidentiality | `vellum_serial.c:245`, `:344` | SSID/password and initial provisioning token are transported as strings. The USB authorization mechanism does not encrypt the provisioning payload. It does not satisfy Njörun's encrypted-channel requirement as-is. |
| Persistent key | `firmware/components/secure_channel/secure_channel.c:20`, `:90`; `src/lib/auth/index.ts:150` | X25519 keypair persisted in NVS, HKDF-SHA256 and AES-GCM token delivery. This is token bootstrap rather than a general authenticated session protocol. Normal requests use HTTPS and a bearer token. |
| At-rest protection | `firmware/components/nvs_manager/include/nvs_manager.h:7`; `firmware/sdkconfig.defaults.p4` | Key storage is software-exportable. Inspected D1001 defaults do not establish hardware-protected or encrypted storage. Do not promise physical-extraction resistance from persistence alone. |
| OTA | `firmware/partitions.d1001.csv`; `firmware/components/ota_manager/ota_manager.c:789` | Two 8 MiB slots, model guard, digest and Ed25519 validation supply a useful implementation reference. Their sizes are not portable to the Guition 16 MB layout. |
| Boot health | `firmware/main/main.c:1321`, `:1361` | Vellum confirms pending firmware after successful render involving server connectivity. Njörun's accepted offline-safe health check requires different confirmation criteria. |
| Reset | `firmware/main/main.c:861`; `firmware/components/nvs_manager/nvs_manager.c:308` | Explicit two-step physical reset and failed-storage handling illustrate recovery states. Adapt to the agreed USB erase flow. |
| Tests and provenance | `firmware/host_test/test_secure_channel.c`, `firmware/host_test/README.md` | Host vectors are useful contracts but do not prove hardware behavior. AGPL notices require an explicit reuse basis before code import into the GPL project. |

## Home Assistant implementation findings

Reviewed iOS at `4cde0ca7f292cf91e5966ef74d81f33133c2e12e`, Android at
`7e2652d6ddc90df5537da6377bb06351509936eb`, Companion documentation at
`e7b9dfca5d3f99a3ce66bac1c0a67e689bc6c633` and Core at
`60885589072eb0e9892a0078ffb83120fce65207`.

Both apps construct an authorization-code URL, capture the app callback and
exchange the code for access/refresh tokens. They provide implementation patterns
for refresh and invalid-session handling. Their app URI callbacks cannot simply
be reused as tablet callbacks.
[iOS onboarding](https://github.com/home-assistant/iOS/blob/4cde0ca7f292cf91e5966ef74d81f33133c2e12e/Sources/App/Onboarding/API/OnboardingAuthDetails.swift)
[iOS token manager](https://github.com/home-assistant/iOS/blob/4cde0ca7f292cf91e5966ef74d81f33133c2e12e/Sources/HANetworking/Sources/TokenManager.swift)
[Android onboarding](https://github.com/home-assistant/android/blob/7e2652d6ddc90df5537da6377bb06351509936eb/app/src/main/kotlin/io/homeassistant/companion/android/onboarding/connection/ConnectionViewModel.kt)
[Android token handling](https://github.com/home-assistant/android/blob/7e2652d6ddc90df5537da6377bb06351509936eb/common/src/main/kotlin/io/homeassistant/companion/android/common/data/authentication/impl/AuthenticationRepositoryImpl.kt)

Core's redirect validator accepts matching scheme/netloc and allows local IP
client identifiers. It separately permits the official app callbacks. This
supports testing a tablet-local client-ID/callback origin. It does not establish
phone reachability, DNS behavior or the security of an HTTP callback. The
reviewed Core token endpoint implements S256 PKCE verification. Use PKCE and
state binding in the experiment and record the minimum supported HA version
rather than assuming older releases implement the same checks.
[Redirect validation](https://github.com/home-assistant/core/blob/60885589072eb0e9892a0078ffb83120fce65207/homeassistant/components/auth/indieauth.py)
[Token endpoint](https://github.com/home-assistant/core/blob/60885589072eb0e9892a0078ffb83120fce65207/homeassistant/components/auth/__init__.py)

Core's WebSocket authentication validates an access token and resolves an HA
user/session. It does not bind the socket to our hardware identity. Njörun's
integration must authorize its device proof separately and scope management
operations to the enrolled tablet. Android reconnects and restores subscriptions;
use that lifecycle as a behavioral reference.
[Core WebSocket authentication](https://github.com/home-assistant/core/blob/60885589072eb0e9892a0078ffb83120fce65207/homeassistant/components/websocket_api/auth.py)
[Android WebSocket implementation](https://github.com/home-assistant/android/blob/7e2652d6ddc90df5537da6377bb06351509936eb/common/src/main/kotlin/io/homeassistant/companion/android/common/data/websocket/impl/WebSocketCoreImpl.kt)

The Companion apps' main HA dashboard is hosted in a web view. They remain
references for onboarding and interaction quality, while native tiles require
Njörun's own presentation of HA entities.
[iOS frontend](https://github.com/home-assistant/iOS/blob/4cde0ca7f292cf91e5966ef74d81f33133c2e12e/Sources/App/Frontend/WebView/WebViewController/WebViewController.swift)
[Android frontend](https://github.com/home-assistant/android/blob/7e2652d6ddc90df5537da6377bb06351509936eb/app/src/main/kotlin/io/homeassistant/companion/android/frontend/FrontendScreen.kt)

The inspected Companion QR documentation concerns tag scanning. The inspected
onboarding paths use code exchange and provide no USB trust protocol. Our QR
should encode an authorization entry point with a defined callback, without
claiming an existing numeric device-code grant.
[Companion QR/tag behavior](https://github.com/home-assistant/companion.home-assistant/blob/e7b9dfca5d3f99a3ce66bac1c0a67e689bc6c633/docs/integrations/universal-links.md)

HA profile UI, registry replacement and backup restoration were not established
by the Companion review. They remain integration implementation tasks requiring
Core API review and tests against the chosen supported release.

## EspControl implementation findings

Reviewed `738840a02514703feb8affeb94ad8802fd506cfe`.
The project is an ESPHome/LVGL implementation with board-specific YAML and
custom C++ components. It provides concrete native-panel patterns, while its
transport and configuration architecture differ from Njörun's HA WebSocket
integration.
[Source tree](https://github.com/jtenniswood/espcontrol/tree/738840a02514703feb8affeb94ad8802fd506cfe)

The JC1060P470 family has separate original and V2 profiles. The V2 source
explicitly cites the C_I_W_Y schematic and new-panel vendor demo. It configures
GT911 touch on I2C GPIO7/8, touch reset 22 and IRQ 21, and JD9165 display reset
GPIO0. The original profile uses GPIO5 for display reset. The V2 SDIO config
records a 10 MHz clock after observed interference at 20 MHz on a particular
P4 rev 1.3 unit. These are upstream code/comment findings, not independent
measurements or a guarantee that Fabian's unit is this revision.
[V2 board](https://github.com/jtenniswood/espcontrol/blob/738840a02514703feb8affeb94ad8802fd506cfe/devices/guition-esp32-p4-jc1060p470-v2/device/device.yaml)
[V2 radio](https://github.com/jtenniswood/espcontrol/blob/738840a02514703feb8affeb94ad8802fd506cfe/devices/guition-esp32-p4-jc1060p470-v2/device/network_coprocessor.yaml)
[Original variant identification](https://github.com/jtenniswood/espcontrol/blob/738840a02514703feb8affeb94ad8802fd506cfe/devices/guition-esp32-p4-jc1060p470/README.md)

The board profile binds LVGL to the display and touchscreen and exposes
ESPHome's native API. The custom HA action path sends through the ESPHome API
server. Its main UI therefore does not provide a reusable standalone Rust HA
WebSocket client. Study entity behavior and board separation without inheriting
an incompatible transport architecture.
[HA action path](https://github.com/jtenniswood/espcontrol/blob/738840a02514703feb8affeb94ad8802fd506cfe/components/espcontrol/button_grid_ha.h)

Browser installation and a panel-hosted configuration UI are documented.
Njörun intentionally locates configuration in HA, so adopt interaction lessons
rather than the panel web-server architecture. The current source license is
PolyForm Noncommercial 1.0.0. Treat this as reference material and resolve
permission/provenance before any code or asset import.
[Installation guide](https://github.com/jtenniswood/espcontrol/blob/738840a02514703feb8affeb94ad8802fd506cfe/docs/getting-started/install.md)
[License](https://github.com/jtenniswood/espcontrol/blob/738840a02514703feb8affeb94ad8802fd506cfe/LICENSE)

Additional inspected UI references include a shared tile with icon, label and
separate short/long-press paths, MDI glyph subsets and a browser installer with
explicit board selection. These help test our two-line tiles and gesture policy.
[Tile widget](https://github.com/jtenniswood/espcontrol/blob/738840a02514703feb8affeb94ad8802fd506cfe/common/device/button_widget.yaml)
[Font configuration](https://github.com/jtenniswood/espcontrol/blob/738840a02514703feb8affeb94ad8802fd506cfe/devices/guition-esp32-p4-jc1060p470/device/fonts.yaml)
[Installer](https://github.com/jtenniswood/espcontrol/blob/738840a02514703feb8affeb94ad8802fd506cfe/docs/.vitepress/theme/components/EspInstallSelector.vue)

EspControl also carries a D1001 MIPI display model, without a complete D1001
device profile in the inspected device/build catalog. Its vendored mipi_dsi
subtree declares separate licensing for C++ and Python files. Evaluate the
original upstream provenance per file rather than applying the root license
to every subtree.
[D1001 model](https://github.com/jtenniswood/espcontrol/blob/738840a02514703feb8affeb94ad8802fd506cfe/components/mipi_dsi/models/seeed.py)
[Subtree license](https://github.com/jtenniswood/espcontrol/blob/738840a02514703feb8affeb94ad8802fd506cfe/components/mipi_dsi/LICENSE)

## Ordered qualification experiments

| Order | Requirement and evidence | Smallest remaining experiment | Blueprint mapping |
| --- | --- | --- | --- |
| 1 | Secure first pairing, browser USB and persisted identity | Specify peer authentication and recovery transcripts. Demonstrate both browser entry paths with a substituted peer, replay, interrupted provisioning and reboot. Keep an explicit decision gate for the HTTP bootstrap. | M0-A7/A8 |
| 2 | Standard HA browser authorization | Real HA instance plus phone browser, local callback, state/nonce binding, token refresh and revoked session. Exercise HTTP/HTTPS and multicast-disabled discovery. | M0-A3 |
| 3 | Rust/IDF/P4/C6 compatibility | Pin the passing upstream target/version candidate and vendor components, boot the actual D1001, then establish verified HTTPS and WebSocket traffic over its C6. | M0-A0/A1 |
| 4 | Responsive native UI | Compare the Slint Rust backend candidate against the vendor LVGL baseline. Render representative tiles and upstream keyboard, map GSL3670 touch in both orientations, record frame time and RAM during network load. | M0-A2/A5/A6 |
| 5 | Reliable application OTA | Sign two board-specific builds, exercise valid/invalid images, early crash, power cuts and settings compatibility while keeping boot confirmation independent of HA reachability. | M3-A1/A3/A4 |
| 6 | Optional motion wake | Capture low-rate camera frames while UI/network remain active, measure resource use and false wakes under lighting changes. | M1-A6 |
| 7 | Shared HA profiles and lifecycle | Minimal custom integration with two logical tablets. Exercise inherited/overridden config, reconnect, replacement, backup restoration and USB recovery. Verify actual registry semantics before promising automation continuity across capabilities. | M2H-A10 through A16 |
| 8 | Second-board portability | Identify the delivered Guition revision and compare its schematic with EspControl V1/V2 profiles. Confirm flash layout, touch, radio and display behavior before a board-specific build. | M0-A4 / M3B |

These experiments test combinations that documentation cannot establish. Their
order is a recommendation derived from current risk and dependencies. Product
requirements remain the blueprint's authority.

## Research limitations

This review covers the supplied references and selected relevant upstream source
paths. It is not an exhaustive security audit or hardware qualification.
Vendor pages, repository HEADs and available CI runs can change; pinned source
links identify the reviewed inputs. No dependency code was imported into Njörun.
The AliExpress listing remains inaccessible. Published device claims and code
comments are attributed evidence and still require validation on the intended
board revision. Implementation of the HA profile/backup/registry lifecycle
needs a supported HA version and explicit integration tests.
