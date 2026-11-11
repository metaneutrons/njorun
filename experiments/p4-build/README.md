# ESP32-P4 Rust / ESP-IDF 6.1 D1001 build

This isolated, nested Cargo workspace compiles and links an ESP32-P4 Rust ELF
through ESP-IDF 6.1. It depends on the repository's `njorun-app-core` package
and includes the first reTerminal D1001 board adapter: display power sequencing,
JD9365 MIPI-DSI initialization, a native 800×1280 RGB565 framebuffer, backlight
PWM, GSL3670 touch polling and Slint software rendering of the shared first-start
UI. The adapter also selects the D1001's 32 MiB flash configuration and two
14 MiB OTA application slots with ESP-IDF boot rollback enabled.

This is compile and link evidence only until the generated image has been run
on a physical D1001. The target includes a bounded ESP32-C6 scan/connect adapter
over ESP-Hosted SDIO, `_home-assistant._tcp` discovery, manual URL entry, pairing
by announcement to the Njörun integration (no sign-in on the tablet: an
administrator approves it in Home Assistant, which sends the tablet its own
access, see [plan 004](../../docs/plans/004-tablet-first-pairing.md)), a token
refresh over the ESP-IDF certificate bundle and an authenticated Home Assistant
WebSocket request for the area registry. The shared UI drives that state machine
from the scanned Wi-Fi list through the room list and dispatches the primary touch
point to Slint. No real radio, touch, TLS or WebSocket behavior is qualified yet. The current board target is portrait-only, saves the Wi-Fi credentials, the
Home Assistant refresh token and the language to NVS in plaintext (see
[device persistence](../../docs/device-persistence.md)) and does not load entity
counts or the Unassigned room.

## Pins

- ESP-IDF v6.1, commit `fff9895c82d744c7237be8847347bdd1b07c6643`.
- esp-rs `esp-idf` monorepo commit `64f829959cfe2771b1168d2790939fcc050372ab`
  (`esp-idf-sys` 0.38.1).
- The firmware package is version `0.1.0` and licensed
  `GPL-3.0-or-later`. It sets `unsafe_code = "deny"`; the only allowed unsafe
  Rust is the local board module that validates and wraps generated C bindings.
- `njorun-app-core` path dependency version `0.1.0`; Rust dependencies are
  locked in this experiment's `Cargo.lock`.
- Slint 1.18.1 uses its MCU software renderer without Slint's `std` feature.
  The build pre-rasterizes UI images and glyphs, and the generated component is
  compiled from the same `crates/simulator/ui/main.slint` source as the host
  simulator.
- Rust `nightly-2026-09-18`, with `rust-src`; `std` and `panic_abort` are built
  for `riscv32imafc-esp-espidf` using `-Zbuild-std`.
- ESP-IDF GCC `esp-15.2.0_20251204`, CMake 4.0.3, Ninja 1.12.1 and ROM symbols
  20241011. Linux installs exact releases from ESP-IDF v6.1's pinned
  `tools/tools.json`; its official metadata SHA-verifies downloaded archives.
- Darwin uses installed host libclang 20.1.1. Ubuntu 24.04 uses Clang/libclang
  18.1.3 (`1:18.1.3-1ubuntu1`) for bindgen.
- `ldproxy` 0.3.5. The binary's `build.rs` forwards ESP-IDF linker metadata
  with `embuild::espidf::sysenv::output()`.
- `sdkconfig.defaults` enables ESP-IDF's `CONFIG_APP_REPRODUCIBLE_BUILD` to
  remove build time and path data from the ELF. It pins
  `CONFIG_APP_PROJECT_VER_FROM_CONFIG=y` and
  `CONFIG_APP_PROJECT_VER="0.1.0"`, so the app descriptor is independent of
  the enclosing repository's Git description.
- The ESP-IDF component lock pins `waveshare/esp_lcd_jd9365_8` 2.0.0,
  `espressif/esp_hosted` 2.13.1, `espressif/esp_wifi_remote` 1.6.5,
  `espressif/esp_websocket_client` 1.8.0, `espressif/mdns` 1.11.2 and their
  transitive registry dependencies. The GSL3670 component is pinned to Seeed's
  D1001 repository commit `5074d3b2f45626b261298e305aaf792036febc5a` and uses
  `espressif/esp_lcd_touch` 1.2.1 plus `espressif/esp_io_expander` 1.2.1. Those
  third-party components retain their own licenses and are downloaded into the
  ignored build tree; they are not copied into this repository.
- The board adapter replaces Seeed's PCA9535 reset defaults with a local generic
  expander shim that preserves the power-hold and unrelated peripheral rails.
  It also replaces the driver's unsafe 44-byte read into a 24-byte local buffer
  with an eight-byte, single-point poll before any runtime input is accepted.
- Slint 1.18.1 passes a randomly seeded character-set order into embedded bitmap
  glyph generation. `build.sh` applies a two-site, SHA-256-guarded ordering fix
  inside this experiment's isolated Cargo source cache. The build refuses an
  unknown original or patched compiler source; remove the workaround after an
  upstream release provides deterministic embedded glyphs.
- `partitions.d1001.csv` contains `otadata`, `ota_0` and `ota_1`; both
  application slots are 14 MiB, with a 64 KiB `nvs` and an unused reserve at the end. A partition-layout change requires a complete
  USB factory flash rather than an application-only OTA.
- Darwin uses Python 3.14.10; Linux uses Ubuntu Python 3.12.x. Both create the
  ESP-IDF 6.1 Python environment with the pinned core constraints file, SHA-256
  `192e8c82a7890164a4e8fe6fafe8423a47f6042c84b5f6a500a003640ae4d32b`.

## Build on macOS

With the pinned Espressif tools and host libclang already installed under
`~/.espressif/tools`, run from the repository root:

```sh
./experiments/p4-build/build.sh --clean
```

Set `ESP_IDF_SHARED_TOOLS` if the preinstalled tools live elsewhere. The Mac
path reuses the already tested EIM tools and keeps its outputs under
`target/build/p4-build`.

## Build on Ubuntu 24.04

The Linux branch installs only the RISC-V GCC, CMake, Ninja and ROM ELF packages
from the exact ESP-IDF v6.1 `tools/tools.json` metadata. It puts the downloads,
extracted tools, Python environment, Rustup toolchain/cache and Cargo output
under `target/build/p4-build/linux`. Rustup and Cargo executables must be on
`PATH`; no global Rust toolchain installation is required. The pinned ESP-IDF
metadata includes per-platform archive SHA-256 values, and `idf_tools.py`
verifies each selected archive before extraction.

GitHub's Ubuntu 24.04 runner includes Python 3.12, Rustup/Cargo and Clang 18.1.3
in its [software inventory](https://github.com/actions/runner-images/blob/main/images/ubuntu/Ubuntu2404-Readme.md).
Ensure Python venv and libclang development packages are present, then run:

```sh
sudo apt-get update
sudo apt-get install --yes --no-install-recommends \
  python3.12-venv python3.12-dev \
  clang-18=1:18.1.3-1ubuntu1 \
  libclang-18-dev=1:18.1.3-1ubuntu1
./experiments/p4-build/build.sh --clean
```

The current Linux qualification used two clean builds in an Ubuntu 24.04 ARM64
Docker container; both produced the same 1,040,860-byte ELF, SHA-256
`279477cdefc34f4be8a4cb1eee88acb0e7853356d96d8ffedab3d88939e3c6c4`. This is
not a GitHub Actions run. The clean build commands took 2m 19s and 2m 10s with
the toolchain and Cargo caches populated. Its ignored output tree measured
4.9 GB, plus 734 MB for the ESP-IDF checkout. The official ESP-IDF tool
archives total about 453 MB on x86_64 or 443 MB on ARM64. Reserve at least 7 GB
of free workspace disk and account for network-dependent first-run setup.

Two clean macOS builds of the current commissioning target are byte-identical:
the ELF is 12,008,892 bytes with SHA-256
`60283c0499af99bcaf1d56b50fb2902d0eb56db17360e2263ab20339fdf66d25`.
This proves repeatable compilation on that host, not physical behavior.

## OTA slot size check

CI fails when the application image no longer fits an OTA slot. The ELF size
cannot be used for this because its segments overlap, so
`scripts/ci/check-image-size.py` runs `espflash save-image` on the built ELF and
measures the result. That image is byte-identical to the part of the merged
image that `scripts/flash-app.sh` writes at the `ota_0` offset. The budget is the
smaller of the `ota_0` and `ota_1` sizes in `partitions.d1001.csv`, so the
partition table stays the only place that defines it. The script prints image
size, budget and headroom and exits non-zero when the headroom is negative.

espflash 4.7.0 is installed for CI from `scripts/ci/firmware-tools.json` by
`scripts/ci/install-tools.py`, which verifies the SHA-256 of the release zip.
To run the check locally after `build.sh`, pass the ELF of your host:

```sh
python3 scripts/ci/check-image-size.py \
  --elf target/build/p4-build/cargo/riscv32imafc-esp-espidf/release/njorun-p4-smoke
```

On the Darwin build of 9 November 2026 the image was 7,964,672 bytes. Against the
former 8 MiB slot that left 5.05% of headroom; the 14 MiB slots (since 11 November
2026) leave about 46%.

## Artifact limits

The Cargo output is the Rust application's ELF. `espflash save-image` can turn
it, the ESP-IDF bootloader and `partitions.d1001.csv` into a merged D1001 factory
image. Do not treat a generated image as hardware evidence until its exact hash,
serial log and observed display behavior are recorded. [qualification.md](qualification.md)
separates historical compile evidence from the current unqualified board target.
