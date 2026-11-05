# Host simulator and HA integration vertical slice

This delivery targets the hardware-independent portion of the
[embedded blueprint](001-embedded-ha-blueprint.md), tracked by existing issues
[#2](https://github.com/metaneutrons/njorun/issues/2),
[#3](https://github.com/metaneutrons/njorun/issues/3),
[#4](https://github.com/metaneutrons/njorun/issues/4) and
[#5](https://github.com/metaneutrons/njorun/issues/5).
It does not waive their physical-board acceptance criteria.

## Required outcome

1. Enroll a native Rust desktop simulator and establish a mutually authenticated
   management session with the development HA integration.
2. Assign multiple independent simulator identities to shared profiles, configure
   them in HA, and distinguish desired settings from acknowledged applied state.
3. Render real HA rooms/entities and operate a light through standard HA APIs.
4. Demonstrate disconnect, restart and recovery without replaying actions,
   impersonating another tablet or losing profile inheritance.
5. Establish a pinned, repeatable P4 cross-build alongside the host work, recording
   exact artifacts and limitations without claiming that they boot on D1001.

## Implementation boundaries

The Rust core owns dashboard state and interaction semantics independently of
networking, UI or ESP-IDF. Slint is the native simulator candidate; simulator
screens are a visual review artifact, not physical renderer qualification.
The integration owns profiles, assignments, per-tablet overrides and device
entities. HA's existing authenticated WebSocket carries the standard dashboard
API and a versioned Njörun management extension. Device proof uses the existing
Noise candidate; an HA account alone cannot claim a tablet identity.

A disposable local HA instance with real HA entities is the integration-test
target. It must not depend on, mutate or disclose credentials from an existing
household installation. The development enrollment handoff transfers public
keys through a trusted local process; it must remain labelled as a simulator
fixture and must not be presented as the browser/USB trust ceremony.

## Qualification

Record executable evidence for every required outcome: protocol interoperability
and negative authentication tests; two tablets sharing a profile and a third
isolated profile; HA UI/settings operations and acknowledged simulator state;
real HA state and service calls; process/server restart and invalidated binding
recovery; and clean P4 cross-build identities. Use the same application paths in
the interactive simulator and automated tests. A headless protocol client alone
is insufficient evidence of a native dashboard.

Host success leaves display/touch/C6 drivers, hardware timing and memory,
physical USB, NVS durability, camera behavior and power-loss OTA rollback open.
Initial browser-based enrollment, TLS/HTTP bootstrap and consumer QR login keep
their original blueprint requirements and need separate end-to-end evidence.

## Qualification status (2026-11-05)

### Verified locally

The disposable HA scenario passed using the coverage-instrumented simulator:
three independent identities enrolled and acknowledged initial revisions 4, 5,
and 6; two tablets inherited one shared profile while the third retained its
isolated revision and brightness. Per-tablet overrides persisted. The scenario
operated `light.njorun_test_light`, a real HA template-light entity, and verified
its state. Both a simulator process restart and a graceful HA stop/start restored
management authentication, dashboard readiness and the expected settings.
The HA trust store's actual file mode was `0600`. Container, named volume and
host fixture cleanup all completed. The sanitized run report is generated at
`target/qualification/vertical-slice.json`.

The test initially failed because Mac temporary-directory bind mounts did not
expose the intended configuration to Docker. Named-volume storage with explicit
file copying fixed that input problem. A separate post-restart readiness error
used the already-completed onboarding endpoint; it now waits for authenticated
Core `RUNNING` and separately verifies management/dashboard recovery. These
corrections preserve the original acceptance predicates. The callback auth API
probe also passes with named-volume cleanup; see the
[host experiment record](../research/002-host-experiments.md).

Native Slint qualification produced 48 software-rendered PNG frames at
800×1280, 1280×800, 600×1024 and 1024×600, covering themes, room selection,
entity tiles, details, Favorites and Mixed views. Pointer/timer tests cover short
taps, hold/details without an action, release consumption, movement cancellation,
consumed wake touches, full command queues and delayed settings acknowledgements.
These fixture-based checks cover native UI mechanics; live HA-to-Slint evidence
is recorded separately below.

The extended scenario also passes the live check: a fourth enrolled tablet
renders the actual HA Living Room, dispatches one short Slint pointer tap, and
observes `light.njorun_test_light` change from off to on. HA independently
confirms the result. The same run passes native HA options-flow profile editing,
number/select overrides, simulator and HA restart, and integration disable/re-enable
on unchanged simulator processes. It verifies that offline actions are not
replayed, a wrong pinned peer cannot activate pending trust, the correct peer can
subsequently activate it, and revocation closes management and rejects subsequent
session establishment. Every simulator shuts down gracefully; temporary HA,
volume and credential fixtures are removed.

The final clean coverage-instrumented run used simulator SHA-256
`efe205b7383d756b463b3cda04c829840692c960bd589b5e61c24c5d87f83c9b`,
live UI probe SHA-256
`c3dc5ad4f5d94055f5771adc45e6c49939acef35aa6402b6259e24764b00be0e`,
and HA image
`ghcr.io/home-assistant/home-assistant@sha256:c6c1ca9931036bf3a2c4cc088d8db25dfea3b33935a5ecc83c40349462e2f69e`.
The root independently inspected the scenario report and actual rendered frame.
`bash scripts/ci/qualify-host.sh` passed from clean coverage output with
93.36% application line coverage (2,725 lines, 181 uncovered), retaining the
90% floor and excluding only test bodies. The README's normal-build commands
also passed separately. Subsequent source cleanup only removed trailing blank
lines; executable statements were unchanged.

A restart race found during that work is fixed: when HA initially rejects the
management extension, the runtime retries a fresh Noise handshake every 15
seconds without dropping an otherwise healthy dashboard. The disable/re-enable
test proves recovery after management becomes available, independently of
startup timing. Deterministic Python regressions also verify that an ACK waiting
behind removal/revocation cannot restore obsolete applied state and that JSON
floating-point revisions are rejected.

The application core's display policy covers the blueprint's 300-second default,
dim clock, disabled idle, first-touch wake, motion activity and manual-off
precedence. Runtime regressions cover atomic configuration validation, malformed
operation IDs, persistence before acknowledgement eligibility, and reconciliation
of a visible replacement whose directory sync failed. CLI tests verify private
identity creation, refusing identity replacement and invalid startup arguments.
The HA client bounds deferred events by both count and aggregate payload bytes;
real WebSocket tests cover overflow and releasing that budget after consumption.
Strict workspace Clippy and Python lint/model tests pass locally.

The [P4 experiment](../../experiments/p4-build/qualification.md) now links the
shared application core. Two clean Ubuntu ARM64 builds produced identical
1,040,860-byte ELFs, SHA-256
`279477cdefc34f4be8a4cb1eee88acb0e7853356d96d8ffedab3d88939e3c6c4`.
The root independently checked the final ELF hash, RISC-V machine identifier
and the documented 13-file source-input hash.
The smoke app version is explicitly pinned to `0.1.0`; implicit Git-description
version discovery had caused a 13-byte descriptor difference before that fix.
CI includes the same two-build comparison on Ubuntu x86_64; execution on that
runner remains pending. No hardware runtime evidence is inferred.

### Remaining qualification

- Actual Linux CI execution, review, and synchronization of issue execution state.

### Explicit implementation limits

The simulator uses package-local `unsafe_code = "deny"` because Slint's generated
bindings require scoped unsafe allowances. Core, pairing and transport retain
workspace `forbid`. This does not qualify an embedded renderer.

`cargo deny` passes with three individually documented temporary maintenance
[exceptions](../research/003-host-dependency-exceptions.md) in the Slint 1.18.1
Linux backend's zbus 5 dependencies: RUSTSEC-2026-0370, 0371 and 0372. Migration
conditions and review deadlines remain active; the exceptions are maintenance
debt, not upstream fixes.

The simulator clock is explicitly UTC. Update policy is stored but no updater
runs in the simulator. Hardware display, touch, camera/motion, USB/NVS and OTA
rollback remain outside this host qualification. The development public-key
handoff is not the consumer browser/USB enrollment ceremony.
