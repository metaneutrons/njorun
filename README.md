# Njörun

Njörun is a planned touchscreen companion for Home Assistant. The project is
in M0 feasibility work. A host-only Rust pairing prototype is available;
firmware, the Home Assistant integration and product releases are not yet available.

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

## Host experiment

The [pairing protocol candidate](docs/protocols/001-pairing-bootstrap.md) tests
mutual authentication and encrypted messages with pre-established peer keys.
It does not implement first enrollment, USB transport or persistent device storage.
See [CONTRIBUTING](CONTRIBUTING.md) for reproducible checks and the
[reference evidence](docs/research/001-reference-evidence.md) for feasibility findings.

## License

Njörun is copyright © 2026 Fabian Schmieder and is licensed under the GNU
General Public License, version 3 or (at your option) any later version
(GPL-3.0-or-later). See [LICENSE](LICENSE) for the complete license text.
