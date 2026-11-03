# Njörun

Njörun is a planned touchscreen companion for Home Assistant. The project is
currently at the planning stage: no firmware, Home Assistant integration, or
release has been implemented or published.

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
planning-stage checks and deferred implementation gates.

## License

Njörun is copyright © 2026 Fabian Schmieder and is licensed under the GNU
General Public License, version 3 or (at your option) any later version
(GPL-3.0-or-later). See [LICENSE](LICENSE) for the complete license text.
