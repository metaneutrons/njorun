# HA management protocol v1 — simulator implementation

Extends the [Noise candidate](001-pairing-bootstrap.md). All commands travel on
HA's existing authenticated WebSocket. Its user authentication is necessary but
does not prove possession of a tablet's key. Static peer pins come from the
administrator-approved trusted enrollment handoff. The simulator qualification
uses a local development handoff, not a browser/USB bootstrap implementation.

## Enrollment and session

| Command | Required fields beyond HA `id`/`type` | Result |
| --- | --- | --- |
| `njorun/enroll` | `tablet_id`, `name`, `profile_id`, `tablet_public_key`, `capabilities`, `expected_revision` | Pending `binding_id`, `ha_public_key`, revision |
| `njorun/session_start` | `protocol: 1`, `binding_id` | Connection-bound `session_id`, fresh 32-byte `context`, expiry |
| `njorun/session_handshake` | `session_id`, `frame` | Noise responder `frame` |
| `njorun/session_subscribe` | `session_id`, encrypted confirmation `frame` | HA subscription and encrypted configuration events |
| `njorun/session_frame` | `session_id`, encrypted applied-ack `frame` | Processing result |

Public keys, contexts and complete Noise frames use standard padded base64.
Handshake plaintext is empty (96-byte first flight, 48-byte response).
The first encrypted message is exactly `{type: "confirm", protocol: 1,
binding_id, session_id}`. Only then can the pending binding activate and its
subscription receive configuration. Failed peer proof, stale contexts, other
connections, malformed messages and replay must be rejected. Reconnect always
performs a fresh handshake with retained static pins.
A tablet restarted by a power cycle or reset leaves its old socket open on HA's
side until it times out. `session_start` therefore does not refuse a tablet that
already has a session; at most three sessions per tablet may be between
`session_start` and their confirmation, the oldest being closed first. Only when
a new session's encrypted confirmation proves the tablet key does it replace the
tablet's older sessions, which are closed without a close event; a failed proof
leaves the live session untouched.

HA sends encrypted `{type: "configuration", operation_id, revision, settings,
expires_at}` event payloads inside `{frame: "…"}`. Expiry is integer Unix seconds.
The tablet independently validates supported values and revision, persists the
candidate, applies the UI/runtime change and returns encrypted
`{type: "applied", operation_id, revision, settings}`. Sending a desired setting
is not evidence that it was applied. Standard HA result IDs and operation IDs
serve different purposes and must not be confused.
Operation IDs must be nonempty strings of at most 128 UTF-8 bytes, without
control characters, and are validated before persistence. HA emits UUID hex IDs.
The host publishes the configuration-derived dashboard before its settings
event; the latter permits the UI acknowledgement after rendering both changes.
If atomic replacement succeeds but directory sync fails, the visible candidate
becomes the in-memory state, while no applied acknowledgement is emitted. A
subsequent durable retry is required before acknowledging that revision.

## Announcement and approval (interim pairing)

Until the browser/USB ceremony exists, a tablet pairs through decision D3 of
[plan 003](../plans/003-d1001-ha-management.md). It is interim and does not
satisfy M2H-A9 or A10.

| Command | Required fields beyond HA `id`/`type` | Result |
| --- | --- | --- |
| `njorun/announce` | `protocol: 1`, `tablet_id`, `name`, `tablet_public_key`, `capabilities` | `{status: "pending", code}`, `{status: "approved", binding_id, ha_public_key}` or `{status: "conflict"}` |

Any signed-in HA user may announce; it needs no administrator. The integration
keeps announcements in memory only (at most 16, ten minutes each), so the
tablet repeats it every ten seconds while waiting. An administrator approves a
waiting tablet in the integration's options ("Approve a waiting tablet"),
choosing a profile and confirming that the six-digit code on the tablet matches
the one listed. Approval runs the normal enrollment (`njorun/enroll` semantics)
with the announced key and capabilities. The next announcement returns
`approved` with the binding; the tablet stores it and starts a session, whose
Noise handshake proves it holds the announced key.

The comparison code is the first four bytes of
SHA-256(`njorun/compare` ‖ 0x01 ‖ tablet public key), read big-endian, modulo
1,000,000, written as six digits; [comparison-code.json](../../protocol/fixtures/comparison-code.json)
pins it for the Rust and Python tests. The tablet checks a `pending` code
against its own. The code covers the tablet key only, because HA's key for a
binding is created at approval. It protects against an announcement with
another key under the same tablet ID: its code differs from the one on the
tablet's screen. An announced ID that is paired with another key returns
`conflict`; the administrator removes or replaces that tablet first. Learning
a binding ID grants nothing without the tablet's private key.

While waiting, the D1001 shows a full-screen approval page in the style of the
first-run screens: the steps to take in Home Assistant, the six-digit code in
large type, a language switch and a waiting indicator. An ID conflict has its own
page with the steps to remove the old tablet. The page disappears when the
session opens. Without the integration, `njorun/announce` fails with
`unknown_command` and the dashboard keeps working; its status line says the
integration is missing.

## Configuration authority

`njorun/list`, `njorun/enroll` and `njorun/configure` require HA administrator
permission. `list` omits keys and session secrets. `configure` supports
`add_profile`, `edit_profile`, `delete_profile`, `set_overrides`, `assign`,
`remove_tablet` and `revoke`, with optimistic `expected_revision` validation.
Reassignment preserves overrides unless `clear_overrides` is explicitly true.
Standard HA entity services use HA's normal entity-control permissions and the
same validation/persistence path; they are not implicitly admin-only services.

Desired configuration, device-applied state and live availability are separate.
Profile edits affect all assigned tablets except overridden fields. Offline
members retain the desired revision and reconcile on reconnect. A disconnected
or revoked binding must not publish available entities or acknowledge settings.
A shared profile contains preferences, never a shared tablet credential.

The [default-settings fixture](../../protocol/fixtures/default-settings.json)
is checked by both Rust and Python tests. Configuration is bounded to 3072 JSON
bytes so the encrypted envelope fits the pairing library's 4096-byte plaintext
limit. The host state/registry limits are separate from eventual embedded RAM
qualification. Unsupported fields and stale revisions fail without mutation.

## Compatibility and limits

Initial integration target: Home Assistant 2026.11.0. The Python Noise responder
uses `noiseprotocol==0.3.1` with HA's cryptography dependency; this is a prototype
interop candidate with no independent security audit claimed. Its internal
remote-static accessor is isolated in the transport adapter and must be reviewed
before production qualification. The Rust initiator remains Snow 0.10.0.

Basic dashboard state/control uses standard HA APIs, independently of this
extension. Registry sequence is a fallback order; an existing Home dashboard can
have its own frontend strategy overrides, which are not implied by registry
iteration. Device area inheritance must follow parent devices safely.

## Host connection lifecycle

The host subscribes to state changes before fetching registry/state snapshots.
Before publishing the initial ready dashboard, it applies already-buffered
state events in order, retaining unrelated management events for their own
handler. State timestamps and persisted deletion timestamps prevent stale
updates from overwriting newer values or resurrecting deleted entities.
Deferred subscription events are bounded by both 256 events and 8 MiB of
serialized JSON payload. Consuming events releases the corresponding budget;
overflow aborts the request and causes the runtime to reconnect. The byte limit
is a payload bound, not an exact bound on the JSON allocator's memory overhead.

An HA JSON ping every 15 seconds checks the live socket; requests have a
15-second response deadline. If HA initially rejects the management extension,
each heartbeat retries a fresh Noise handshake while the dashboard remains
available. This covers integration startup/reload after HA's socket is ready;
it does not reuse failed session keys or replay home-control actions.
TCP addresses share a 15-second connection budget
following DNS resolution, and TLS/WebSocket handshakes have socket read/write
timeouts. System DNS resolution is still synchronous and is not covered by this
budget. Redirects are rejected before HA credentials are transmitted. These are
host transport policies, not measured ESP-IDF timing guarantees.

The shared [validation cases](../../protocol/fixtures/settings-cases.json) exercise
both Python and Rust against the same settings inputs, including Unicode control
characters, unknown fields, duplicate IDs and numeric/time boundaries. The
3072-byte settings bound uses compact UTF-8 JSON in both implementations.

An active responder may end management with encrypted
`{type: "closed", protocol: 1, session_id, reason}` on its existing subscription.
Reasons are `expired`, `revoked`, `removed` or `unloaded`. The client verifies
protocol, session identity and the exact message shape before treating it as an
authenticated close. It rejects further management sends on that session and
reconnects with retained static pins and HA credentials. A new session after
revocation still requires an active binding; closing is not permission to enroll.
An authentication/replay failure also makes the client session terminal. No
home-control action is replayed during this recovery.
