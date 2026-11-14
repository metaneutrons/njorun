# Entity completeness and media controls

Decision state: implementation authorized by Fabian on 2026-11-14. Media is the first
feature priority. GitHub tracking/publication has not been requested; this repository
plan records the requirements and local verification evidence.

## Outcome

Close the entity-support audit's defects and missing operations. The panel is a daily
control surface; device setup, automation editing and reusable presets remain in HA.
Every recognized entity must expose controls appropriate to its actual capabilities,
with authoritative feedback and readable unavailable/unknown/pending/error states.
The expanded inventory includes all 45 HA 2026.11 entity platforms and the audited
helper domains, including entities that exist in states but not the entity registry.

## Design

Shared core control descriptors and validated action planning own capabilities, labels,
input constraints and service mappings. Simulator and P4 consume the same descriptors;
neither frontend invents service names or controls from domain alone. Specialized media
presentation uses the same actions and validation as the general detail controls.
Response-producing actions and image resources are bounded and represented explicitly.
Never label an acknowledged request as an observed state change. Never replay actions
after reconnect. Unknown is distinct from unavailable.

HA owns configuration and optional presets. Extended services use named, typed fields,
not a raw JSON editor. Unsupported provider capabilities are omitted or explained.
Provider-side APIs and actual hardware are required for audio/video execution; where a
provider cannot implement an operation, this is a reported limitation, not a simulated
success. Such limitations do not count as completed feature acceptance.

## Delivery and acceptance

### M1 — correctness foundation

- M1-A1: Unknown unused scene/script/button/input_button entities remain actionable after
  initial and repeated snapshots in both frontend ordering paths.
- M1-A2: Numbers validate bounds and steps, preserve precision, and confirm only the
  requested value at the entity's precision. Selects validate current option membership.
- M1-A3: Capabilities and input constraints are enforced again at dispatch; stale,
  unavailable and already-pending actions cannot be sent.
- M1-A4: Camera errors cannot leave an old image presented as current. P4 and simulator
  have explicit image/loading/error/age behavior. Attribute transport uses a shared
  inventory; overflow and unsupported choices are explained.

### M2 — media first

Depends on M1. State-aware play/pause/stop/power, previous/next, volume/mute, seeking,
progress/duration, artwork, source/sound mode, shuffle/repeat, media browsing/search,
play/enqueue/clear queue and grouping are capability-aware and reachable. The dashboard
has an explicit playback action. Queries display useful results. Provider limitations
are represented without fabricated controls or playback success.

### M3 — existing domain completeness

Depends on M1. Light color/temperature/effects/transitions/flash; fan presets/direction/
oscillation; lock codes/open/state handling; alarm code formats and arming requirements;
script stop/parameters/running information; automation trigger options; precise number
entry; translated/contextual sensor and binary-sensor presentation and history.

### M4 — missing domains

Depends on M1. Climate, cover, humidifier, water heater, valve, vacuum, mower, siren and
remote controls; weather, calendar, to-do, presence, events and images; text/date/time,
counter/timer/schedule and update controls; voice, notification, AI and specialized
platforms. Every audited domain requires a documented supported interaction, suitable
state display, negative capability tests and reachable frontend coverage. Listing a
domain or showing its raw state alone is not completion of its interactive features.

### M5 — qualification

Focused core and native UI tests during implementation; then workspace tests, Clippy,
HA tests/lint, ESP32-P4 build, and macOS render/interaction review across both modes,
families, orientations and relevant scales. Test real disposable HA services and response
queries. Physical device/provider validation is recorded separately and never inferred
from compilation, fixtures or synthetic acknowledgements.

## Compatibility and risks

Preserve existing settings, identities and legacy action keys. Expanded serialized
domains require coordinated firmware updates for persisted caches; unknown cached
domains must not cause destructive reset. Bounded models, images, query results and
queues protect embedded memory. No deployment, paid service use, household device
mutation or publication is included in local qualification.

## Verification evidence

Implementation and local qualification completed on 2026-11-14:

- The shared catalog recognizes 60 domains, covering all 45 HA 2026.11 entity
  platforms plus the audited helper domains. Catalog, capability, dispatch and native
  renderer tests exercise both positive and negative paths.
- `cargo test --workspace --all-features --locked`, Clippy with warnings denied,
  both format checks, warning-free Rustdoc and both dependency audits pass. The exact
  CI host qualification reports 90.35% line coverage against the unchanged 90% gate.
- The native entity-control renderer test passed five consecutive runs. The full
  visual qualification produced 477 review frames across themes, color families,
  orientations, states and control variants.
- 257 integration and release tests pass. Repository links, Ruff and whitespace checks
  pass.
- A disposable HA 2026.11.0 vertical slice passed, including a real Slint-originated
  light action, restart recovery, non-replay of an offline request and profile settings.
  The disposable media probe confirmed transport, volume, mute, playback, browsing and
  search payloads against HA's live schemas.
- The optimized ESP32-P4 cross-build passes with the shared catalog and controls.

Physical-device interaction and installed provider behavior remain separate acceptance
work. The automated native macOS renderer and interaction suite passes; direct desktop
inspection was unavailable during this qualification because the Mac session was locked.
