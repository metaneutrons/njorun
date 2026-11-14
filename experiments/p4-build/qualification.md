# ESP32-P4 Rust / ESP-IDF 6.1 compile qualification

Date: 2026-11-06

## Scope and result

The isolated firmware workspace compiles and links for `riscv32imafc-esp-espidf`
with ESP-IDF v6.1 for the ESP32-P4 target. It depends on the repository's
`njorun-app-core` at `crates/app-core` version `0.1.0` and compiles a
project-owned D1001 adapter invoked through generated bindings from a narrowly
scoped unsafe Rust module. The embedded Rust state machine covers Wi-Fi
selection and password entry, Home Assistant discovery and manual URL entry,
pairing by announcement to the Njörun integration (since 2026-11-11, plan 004; the
earlier PKCE browser authorization through a local callback is gone), a token
refresh and an authenticated WebSocket area-registry request before constructing
the room view. The package is version `0.1.0` and licensed `GPL-3.0-or-later`. Cargo and
ESP-IDF component lockfiles fix the Rust and managed-component dependencies.

The 2026-11-06 macOS build compiled the D1001 display and touch adapters plus
the complete in-memory commissioning path described above. The C adapter uses
ESP-Hosted SDIO for Wi-Fi, mDNS for `_home-assistant._tcp`, the ESP-IDF HTTP
client with the full certificate bundle for token exchange and the official
ESP WebSocket client for Home Assistant authentication and area retrieval. No
firmware was flashed and no physical display, touch, radio, callback, TLS or
WebSocket behavior was observed. The target remains portrait-only, keeps the
Wi-Fi credentials, HA session and language in plaintext NVS (see
[device persistence](../../docs/device-persistence.md); restore on boot is not yet
observed on hardware) and does not yet retrieve entities or an Unassigned room. This is compile evidence, not hardware acceptance.

`espflash` 4.5.0 converted the final ELF, generated ESP-IDF bootloader and
`partitions.d1001.csv` into an unpadded merged 32 MiB factory image targeting
`ota_0`. The application occupies 5,974,640 of 8,388,608 bytes (71.22%); the
merged image is 6,105,712 bytes with SHA-256
`23e811c9b615121793b1ae326750e816dd0921ba2c6b2a9a5ed60429187f2925`.
Image generation and size fit are not evidence that the image boots.

The historical Linux evidence below applies to the earlier app-core-only target.
It is retained as provenance but does not qualify the current D1001 sources;
CI must produce new clean-build evidence for the changed target.

## Current Linux app-core build evidence

| Host | Final application ELF | Size | SHA-256 | Result |
| --- | --- | ---: | --- | --- |
| Ubuntu 24.04 ARM64 Docker | `target/build/p4-build/linux/cargo/riscv32imafc-esp-espidf/release/njorun-p4-smoke` | 1,040,860 bytes | `279477cdefc34f4be8a4cb1eee88acb0e7853356d96d8ffedab3d88939e3c6c4` | two clean compiles and links passed; byte-identical |

The Linux artifact is ELF32 little-endian RISC-V `ET_EXEC`, `EM_RISCV`, flags
`0x3` (RVC and single-float ABI), statically linked and not stripped. The two
Linux `build.sh --clean` invocations took 2m 19s and 2m 10s after host packages,
toolchain and Cargo caches were ready. Both embedded app version `0.1.0`. The
byte comparison covered all 1,040,860 bytes and found zero differences.

The 12-file experiment/core-source snapshot was hashed before and after the
build pair; both checks produced
`764bb107f0fd04306c0675348921e0926ed24ec363e72de56e70e2440e5bc14c`. The root
`Cargo.toml` is also a build input because `crates/app-core/Cargo.toml`
inherits its edition, license and Rust lints. Its modification time was
2026-11-05 02:13:23 CET, before the Linux builds, and it was unchanged during
the pair. With that manifest included, the current 13-file input aggregate is
`805ef9f88b1c7ef555bd5ccfc1dfa22bc6fca091c5633005b71539ca2f6dac59`.
Integration tests are excluded because Cargo builds `njorun-app-core` here as
a library dependency. Recreate the 13-file hash from the repository root with:

```sh
python3 - <<'PY'
from hashlib import sha256
from pathlib import Path

files = [
    "Cargo.toml",
    "experiments/p4-build/Cargo.toml",
    "experiments/p4-build/Cargo.lock",
    "experiments/p4-build/build.rs",
    "experiments/p4-build/build.sh",
    "experiments/p4-build/rust-toolchain.toml",
    "experiments/p4-build/.cargo/config.toml",
    "experiments/p4-build/sdkconfig.defaults",
    "experiments/p4-build/src/main.rs",
    "crates/app-core/Cargo.toml",
]
files.extend(str(path) for path in sorted(Path("crates/app-core/src").rglob("*.rs")))
aggregate = sha256()
for name in sorted(files):
    content_hash = sha256(Path(name).read_bytes()).hexdigest()
    aggregate.update(name.encode("utf-8"))
    aggregate.update(bytes([0]))
    aggregate.update(content_hash.encode("ascii"))
    aggregate.update(bytes([10]))
print(f"{len(files)} inputs {aggregate.hexdigest()}")
PY
```

Before the version pin, two clean builds selected different implicit ESP-IDF
project versions: the first used enclosing Git description `364e9dc-dirty`,
the second fell back to `1`. Exactly 13 bytes differed in `.flash.appdesc`; all
other ELF bytes matched. ESP-IDF v6.1's `project.cmake` normally checks
`version.txt`, a `project(VERSION ...)` argument, Git description, then `1`.
The pinned Kconfig options `CONFIG_APP_PROJECT_VER_FROM_CONFIG=y` and
`CONFIG_APP_PROJECT_VER="0.1.0"` now override that discovery. The current pair
has `0.1.0` in `.flash.appdesc` and is byte-identical despite repository Git
metadata changes.

The Ubuntu build ran in an isolated `ubuntu:24.04` ARM64 container on the
macOS ARM64 host. It is Linux-host build evidence, not a GitHub Actions run and
not evidence for the x86_64 runner itself. The Linux path used Ubuntu
Clang/libclang 18.1.3 packages to bind the pinned ESP-IDF headers. An earlier
macOS ARM64 app-core build before the project-version pin produced a
1,036,888-byte ELF with SHA-256
`effc7e2d1722e78ea2adfada8d2ace17d2096c3ca226858b6a585751825f02bf`; it was not
rerun with the current defaults and is not part of this qualification. No
cross-host byte-identity claim is made.

## Pinned inputs

| Input | Pin |
| --- | --- |
| Rust | `nightly-2026-09-18`, `rustc 1.100.0-nightly (330d31712 2026-09-17)`, commit `330d317121e16b5db8e5adc63595910528ff2ee7`, component `rust-src` |
| Rust target | `riscv32imafc-esp-espidf`; build-std `std,panic_abort` |
| Firmware manifest | version `0.1.0`; license `GPL-3.0-or-later`; Rust lint `unsafe_code = "deny"` with a scoped board FFI allowance |
| ESP-IDF | tag `v6.1`, commit `fff9895c82d744c7237be8847347bdd1b07c6643` |
| esp-rs `esp-idf` monorepo | commit `64f829959cfe2771b1168d2790939fcc050372ab`; `esp-idf-sys 0.38.1` |
| linker metadata helper | `embuild 0.33.7`; `build.rs` calls `embuild::espidf::sysenv::output()` |
| ESP-IDF Python constraints | SHA-256 `192e8c82a7890164a4e8fe6fafe8423a47f6042c84b5f6a500a003640ae4d32b` |
| `ldproxy` | 0.3.5 |
| ESP-IDF build options | `CONFIG_APP_REPRODUCIBLE_BUILD=y`; `CONFIG_APP_PROJECT_VER_FROM_CONFIG=y`; `CONFIG_APP_PROJECT_VER="0.1.0"` in `sdkconfig.defaults` |
| Rust dependencies | experiment `Cargo.lock` |
| ESP-IDF managed components | `components_esp32p4.lock`; `waveshare/esp_lcd_jd9365_8` 2.0.0, `espressif/esp_hosted` 2.13.1, `espressif/esp_wifi_remote` 1.6.5, `espressif/esp_websocket_client` 1.8.0, `espressif/mdns` 1.11.2, Seeed GSL3670 at commit `5074d3b2f45626b261298e305aaf792036febc5a`, `espressif/esp_lcd_touch` 1.2.1 and `espressif/esp_io_expander` 1.2.1 plus locked transitive dependencies |
| D1001 layout | 32 MiB flash; `ota_0` and `ota_1` are 8 MiB each; boot rollback enabled |

Slint 1.18.1's embedded-glyph pass keeps the characters to embed in a randomly
seeded `HashSet` and the imported fonts in a `HashMap`, so glyph indices and
font order, and with them the ELF, differ between clean builds. The experiment
therefore builds against the vendored `vendor/i-slint-compiler`, which applies
upstream slint-ui/slint#13738 (stable order). See `vendor/README.md`. Remove the
vendored copy and the `[patch.crates-io]` entry when a pinned Slint release
includes the fix. An earlier workaround in `build.sh` sorted only the
characters and did not cover several bundled fonts; it was removed.
Two consecutive clean Darwin builds with that workaround produced
byte-identical 12,008,892-byte commissioning ELFs with SHA-256
`60283c0499af99bcaf1d56b50fb2902d0eb56db17360e2263ab20339fdf66d25`.
The earlier display-and-touch-only head produced byte-identical 6,523,400-byte
ELFs with SHA-256
`2307d7a8a7609d38e452d18e4c2c762081dd42a125f91064d61e67533955363c`.

Darwin reuses the tested Espressif tools in `~/.espressif/tools` (or the path
in `ESP_IDF_SHARED_TOOLS`) and its installed host libclang 20.1.1. The Linux
branch downloads only the required ESP-IDF tools into the ignored Linux output
tree. It invokes `idf_tools.py` from the pinned ESP-IDF checkout, explicitly
selects each tool release below, and lets that pinned `tools/tools.json`
metadata verify archive SHA-256 before extraction. The official metadata is
[at the exact ESP-IDF commit](https://github.com/espressif/esp-idf/blob/fff9895c82d744c7237be8847347bdd1b07c6643/tools/tools.json).

| Tool | Selected release | Ubuntu x86_64 archive SHA-256 | Ubuntu ARM64 archive SHA-256 |
| --- | --- | --- | --- |
| RISC-V GCC | `esp-15.2.0_20251204` | `ace5aae6afe98f754947be043d40173e2e22ace57754b11a394b7238eefa01cf` | `90cccb3ef035f016836dd7c292528b27333a716d42b9361a68005d178c0f70bf` |
| CMake | `4.0.3` | `585ae9e013107bc8e7c7c9ce872cbdcbdff569e675b07ef57aacfb88c886faac` | `391da1544ef50ac31300841caaf11db4de3976cdc4468643272e44b3f4644713` |
| Ninja | `1.12.1` | `6f98805688d19672bd699fbbfa2c2cf0fc054ac3df1f0e6a47664d963d530255` | `5c25c6570b0155e95fce5918cb95f1ad9870df5768653afe128db822301a05a1` |
| ESP ROM ELF data | `20241011` | `921f000164a421c7628fbfee55b173384aafaa51883adc65cd27bf9b0af9e9a9` | same archive |

For Ubuntu 24.04 bindgen, the host Clang/libclang version is 18.1.3; the
validated package version was `1:18.1.3-1ubuntu1`. Using the runner's system
libclang avoids the optional Espressif Clang archives, which occupied 2.8 GB
when briefly evaluated. The cross compiler, CMake, Ninja, ROM data and their
hashes still come from the pinned ESP-IDF metadata.

## Historical standalone smoke milestone

Before linking `njorun-app-core`, the smoke binary only printed a line and
slept. Its reproducible ELF was 1,019,296 bytes with SHA-256
`138a7803baee03c1c20d187baa9abb551c82a83bb4b14fbe0b9b2a3a7fe5e7b7`; three
clean runs after enabling `CONFIG_APP_REPRODUCIBLE_BUILD` returned that same
hash. This hash is retained as the standalone milestone, not the current
app-core artifact.

The initial builds before enabling that IDF option had hashes
`20915731024b5bb8f2adadf9f0b849a8eb6637dc9e2a11745959f712af0ba679`,
`ed64ca0d1b7f31d6b02e9cb803561d5494b4422ca64aed8be63af84fdfd14f48`, and
`8019ebe4900165782001ee8ce2494fcc51b7af1e941bbf4c874328e0f127750b`. Comparing
the preserved second and third ELFs found exactly three differing bytes in
`.flash.appdesc`: the build clock `02:27:05` versus `02:28:49`. ESP-IDF's
reproducible build option removes those date/time bytes and maps source paths
to stable prefixes.

## Reproduction

### macOS

With the selected Espressif compiler, CMake, Ninja, ROM data and host libclang
already present in `~/.espressif/tools`, run this from the repository root:

```sh
./experiments/p4-build/build.sh --clean
```

### Ubuntu 24.04 GitHub-hosted runner

At revision `84849f53ddb105d1cc889da659c54b7c64a48782`, the x86_64 runner
compared two clean P4 ELFs with `cmp`. Both had SHA-256
`a61ddb13c598bf7fa42b4941de8fecd23660fd8584d92e19c7947f456316a6b8`.
This verifies repeatability on that host. The earlier ARM64 measurements above
refer to the inputs used before fixing CMake's Python selection on Ubuntu x86_64.
See the [successful P4 job](https://github.com/metaneutrons/njorun/actions/runs/41977188586/job/127078787420).

The build script requires `rustup`, `cargo`, Python 3.12, a C/C++ build host,
and Clang/libclang 18.1.3. GitHub's Ubuntu 24.04 image inventory lists Python
3.12, Rustup/Cargo and Clang 18.1.3; verify the current image using the
[official software inventory](https://github.com/actions/runner-images/blob/main/images/ubuntu/Ubuntu2404-Readme.md).
Install the Python venv and pinned libclang development packages if the runner
image does not already expose them:

```sh
sudo apt-get update
sudo apt-get install --yes --no-install-recommends \
  python3.12-venv python3.12-dev \
  clang-18=1:18.1.3-1ubuntu1 \
  libclang-18-dev=1:18.1.3-1ubuntu1
./experiments/p4-build/build.sh --clean
```

The script keeps `CARGO_HOME`, `RUSTUP_HOME`, the pinned Rust toolchain, Cargo
target, ESP-IDF tool archives/tools, and Python environment under the ignored
`target/build/p4-build/linux` directory. Rustup and Cargo executables must be
on `PATH`; no global Rust toolchain install is needed. IDF tool directories
under Linux's tools root are `.../esp-idf-tools/tools/...`; this is separate
from the tested Darwin EIM layout, and neither path overwrites the other.

The pinned ESP-IDF archives total about 453 MB on x86_64 and 443 MB on ARM64.
The completed Linux output tree measured 4.9 GB, with the ESP-IDF checkout
adding 734 MB. Plan for at least 7 GB of free workspace disk before downloads
and check the runner's actual free space. GitHub lists 14 GB SSD storage for
standard Ubuntu 24.04 runners in its [runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
On the validated ARM64 Docker host, the latest two clean compilations took
2m 19s and 2m 10s with dependencies and archives cached; first-run download and
environment setup time depends on network throughput.

The Cargo command used by the script is:

```sh
cargo +nightly-2026-09-18 build --locked --release \
  -Zbuild-std=std,panic_abort --target riscv32imafc-esp-espidf
```

`--clean` removes only the host's experiment Cargo output and generated
`sdkconfig`; it retains the ESP-IDF checkout and downloaded tool cache. All
project build output stays under the ignored `target/build/p4-build` directory.
