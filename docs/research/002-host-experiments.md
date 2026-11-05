# M0 host experiments

Date: 2026-11-05. Scope: partial evidence for M0-A3 and M0-A8.
The [blueprint](../plans/001-embedded-ha-blueprint.md) remains the acceptance
authority. Neither criterion is accepted by host results alone.

## Pairing candidate

The [protocol candidate](../protocols/001-pairing-bootstrap.md) uses Snow 0.10.0
and Noise IK with X25519, ChaCha20-Poly1305 and SHA-256. Tests supply trusted
static pins and exercise complete messages on the host. The implementation is
original project code; no Vellum or EspControl code was copied.

Rust 1.99.1 is pinned for the host workspace, selected from the current
[stable distribution manifest](https://static.rust-lang.org/dist/channel-rust-stable.toml).
Cargo.lock records dependency versions. This host toolchain pin does not qualify
the separate ESP-IDF target compiler or Snow's embedded random-number provider.
The crate declares no optional features and forbids unsafe code in its own source.

The pinned Snow default feature set includes ring through its std feature;
getrandom 0.2 and 0.3 coexist. The workspace documents this Clippy exception and
cargo-deny still reports the duplicate. ISC is explicitly allowed for ring and
untrusted; no advisory exceptions were added.

A separate read-only review found no blocker in pin enforcement, bounds or
cryptographic failure closure. It identified the distinction between local
responder transport readiness and initiator liveness: replayed IK first flights
must not trigger side effects. The protocol requires an authenticated initiator
confirmation and a fresh, single-use context managed by the future coordinator.
This review is not an independent cryptographic audit.

## Recorded host results

On the development Mac, Rust 1.99.1 and the committed lockfile passed:

- 16 public-API integration tests covering successful bidirectional messages,
  wrong peer pins/context, tampered handshake messages, malformed responses,
  transport tampering/truncation/replay, cross-session replay, size limits,
  phase misuse and recreated endpoints using the same static keys.
- Library coverage: 221/234 lines (94.44%), with integration test files excluded;
  26/26 functions executed. This is line coverage, not a branch-coverage claim.
- Formatting, strict Clippy, strict rustdoc and cargo-deny. The only dependency
  warning is the documented duplicate getrandom versions.
- Seven CI classifier tests, including documentation-only skips, executable and
  unknown inputs, and unavailable/missing comparison-base fallback.
- Workflow lint, hook validation and repository checks. The read-only GitHub
  repository audit recorded 18 passes and no failures; platform limitations remain
  unchanged (personal-account merge queue and non-provider secret patterns).

See [CONTRIBUTING](../../CONTRIBUTING.md) for the exact host commands. Host tests
have no live-HA or USB side effects.

## Remaining qualification

The host test's restart simulation recreates endpoints from the same static key
fixtures. It is not evidence of NVS durability, secure storage, or recovery after
power loss. Hardware CSPRNG and memory/CPU costs remain unmeasured.

Initial peer enrollment, authenticated HTTP-to-HTTPS handoff, HA-side protocol
interoperability, serial framing, durable confirmation and authorization of
management operations remain open. A correct encrypted channel cannot repair
an unauthenticated initial public-key exchange.

No ESP32 USB port was present during the local devserial inventory. No tablet
was flashed. M0-A0/A1/A2/A5/A6/A7 and the physical portions of A8 remain pending.
