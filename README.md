<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/wordmark-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="assets/brand/wordmark-light.svg">
  <img src="assets/brand/wordmark-light.svg" alt="Njörun" width="540">
</picture>

**Your home. Within reach.**

A native Home Assistant touchscreen companion, built with Rust for ESP32-P4.

[![CI](https://github.com/metaneutrons/njorun/actions/workflows/ci.yml/badge.svg)](https://github.com/metaneutrons/njorun/actions/workflows/ci.yml)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)

</div>

---

Njörun is a touchscreen companion for Home Assistant under development.
The repository contains a native Rust simulator, dashboard/application core,
and an experimental Home Assistant integration. End-to-end qualification is
in progress; no supported tablet firmware or product release is available.

The design targets a native Rust application on ESP32-P4, using ESP-IDF. The
Seeed reTerminal D1001 is the first hardware target; the Guition
JC1060P470C-I-W-Y is a later port. Planned capabilities include a native
touchscreen interface, a dedicated Home Assistant integration, and signed
firmware updates with automatic rollback.

## Project plan

The [embedded Home Assistant blueprint](docs/plans/001-embedded-ha-blueprint.md)
is the source of truth for scope, decisions, dependencies, and acceptance
criteria. Its linked epic and milestone issues record execution state and
evidence. [Repository standards](docs/repository-standards.md) describe the
current checks and deferred implementation gates.

## Host development

The [host implementation plan](docs/plans/002-host-vertical-slice.md) tracks simulator
enrollment, shared profiles, real HA controls and recovery. With Rust, uv and
Docker available, build the simulator and live UI probe, then run the disposable
HA scenario:

```sh
cargo build -p njorun-simulator --all-features --example live_ui --bin njorun-simulator --locked
uv run --locked python experiments/vertical-slice/qualify.py
```

The scenario creates its own temporary HA instance and does not use your
household installation. Enrollment, shared profiles, native HA configuration,
a real Slint light action, restart/reload recovery, peer rejection and revocation
pass locally. A clean combined run reaches 93.36% application line coverage.
Linux CI qualification remains pending; this is not release qualification. For the standalone simulator without qualification
helpers, use `cargo build -p njorun-simulator --locked`.

A separate [ESP32-P4 compile experiment](experiments/p4-build/README.md) has
produced byte-identical ELFs across two clean Linux builds with pinned Rust and
ESP-IDF inputs. See its [qualification evidence](experiments/p4-build/qualification.md).
It compiles and links a smoke program using the shared application core;
display, touch, networking and on-device
application behavior remain unqualified.

The [pairing protocol candidate](docs/protocols/001-pairing-bootstrap.md) tests
mutual authentication and encrypted messages with pre-established peer keys.
It does not implement first enrollment, USB transport or persistent device storage.
See [CONTRIBUTING](CONTRIBUTING.md) for reproducible checks and the
[reference evidence](docs/research/001-reference-evidence.md) for feasibility findings.

## License

Njörun is copyright © 2026 Fabian Schmieder and is licensed under the GNU
General Public License, version 3 or (at your option) any later version
(GPL-3.0-or-later). See [LICENSE](LICENSE) for the complete license text.
