# ESP32-P4 Rust / ESP-IDF 6.1 D1001 build

This isolated, nested Cargo workspace compiles and links an ESP32-P4 Rust ELF
through ESP-IDF 6.1. It depends on the repository's `njorun-app-core` package
and includes the first reTerminal D1001 board adapter: display power sequencing,
JD9365 MIPI-DSI initialization, a native 800×1280 RGB565 framebuffer, backlight
PWM and a diagnostic frame rendered from Rust. The adapter also selects the
D1001's 32 MiB flash configuration and two 8 MiB OTA application slots with
ESP-IDF boot rollback enabled.

This is compile and link evidence only until the generated image has been run
on a physical D1001. Touch, orientation, the ESP32-C6 network transport and the
first-start application are not implemented by this target yet.

## Pins

- ESP-IDF v6.1, commit `fff9895c82d744c7237be8847347bdd1b07c6643`.
- esp-rs `esp-idf` monorepo commit `64f829959cfe2771b1168d2790939fcc050372ab`
  (`esp-idf-sys` 0.38.1).
- The firmware package is version `0.1.0` and licensed
  `GPL-3.0-or-later`. It sets `unsafe_code = "deny"`; the only allowed unsafe
  Rust is the local board module that validates and wraps generated C bindings.
- `njorun-app-core` path dependency version `0.1.0`; Rust dependencies are
  locked in this experiment's `Cargo.lock`.
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
- The ESP-IDF component lock pins `waveshare/esp_lcd_jd9365_8` 2.0.0 and its
  transitive registry dependencies. That third-party component retains its own
  license and is downloaded into the ignored build tree; it is not copied into
  this repository.
- `partitions.d1001.csv` contains `otadata`, `ota_0` and `ota_1`; both
  application slots are 8 MiB. A partition-layout change requires a complete
  USB factory flash rather than an application-only OTA.
- Darwin uses Python 3.14.10; Linux uses Ubuntu Python 3.12.x. Both create the
  ESP-IDF 6.1 Python environment with the pinned core constraints file, SHA-256
  `9500db94c8ef72a523b1ecfb4f9ca5b9f5c069d6cbe179728806ae0c1b0396ac`.

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

## Artifact limits

The Cargo output is the Rust application's ELF. `espflash save-image` can turn
it, the ESP-IDF bootloader and `partitions.d1001.csv` into a merged D1001 factory
image. Do not treat a generated image as hardware evidence until its exact hash,
serial log and observed display behavior are recorded. [qualification.md](qualification.md)
separates historical compile evidence from the current unqualified board target.
