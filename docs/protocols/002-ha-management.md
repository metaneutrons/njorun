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
log_sink, expires_at}` event payloads inside `{frame: "…"}`. Expiry is integer Unix
seconds. `log_sink` is the installation-wide remote logging option (decision D1): empty
for off, else an `http(s)` URL of a Grafana Loki server without credentials, query or
fragment, at most 256 characters. It is not part of `settings` and not acknowledged; the
tablet stores it and applies it at once. A message without `log_sink` (an older
integration) leaves the stored value as it is. Changing the option gives every tablet a
new revision, so it is sent again.
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
waiting tablet on the **Discovered** card the integration starts for it
(Settings → Devices & services), choosing a profile when more than one exists
and confirming that the six-digit code on the tablet matches the one shown.
The card disappears when the tablet is approved or stops announcing. Approval runs the normal enrollment (`njorun/enroll` semantics)
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

## Live commands and telemetry

Decision D6 of [plan 003](../plans/003-d1001-ha-management.md). Three more
encrypted message types use the existing session. `command` travels on the
subscription like a configuration event; `command_result` and `telemetry` travel
in `njorun/session_frame` like `applied`. They are live state: nothing here is
persisted by HA, acknowledged by a revision or replayed after a reconnect.
The [fixture](../../protocol/fixtures/live-messages.json) pins accepted and
rejected messages for the Rust and Python tests.

| Message | From | Fields |
| --- | --- | --- |
| `command` | HA | `command_id`, `name`, `value`, `expires_at` |
| `command_result` | tablet | `command_id`, `ok`, `reason` |
| `telemetry` | tablet | `firmware_version`, `uptime_s`, `rssi_dbm`, `free_heap`, `reset_reason`, `backlight` |

Each message has exactly these fields plus `type`; others are rejected.

**Commands.** `command_id` follows the operation ID rules and is a fresh UUID hex
from HA. `expires_at` is integer Unix seconds, ten seconds after sending. `name`
is `wake` (`value` is `null`: leave idle or screensaver as a touch would, without
a touch) or `backlight` (`value` is `{on, brightness}`, `on` boolean, `brightness`
an optional integer from 0 to 100, where an absent value keeps the current level;
`on: false` is the manual-off of the blueprint). The tablet validates the whole
message before acting. It answers every command with one `command_result`:
`ok: true` with `reason: null`, or `ok: false` with `reason` one of `expired`,
`duplicate`, `unsupported`, `invalid` or `failed`. A command whose `command_id`
the tablet has seen on this session (it keeps the last 16) is `duplicate` and
does nothing. A command after `expires_at` is `expired` and does nothing. A
command that does not parse far enough to yield a `command_id` is dropped.
`ok: true` means the tablet carried it out; the resulting state arrives as
telemetry.

HA sends a command only on a live session and never queues one. Without a
session the entity service fails at once. HA accepts a `command_result` only for
a command it sent on that session and has not yet resolved, within 15 seconds;
any other result is a protocol error that closes the session like a bad
acknowledgement. When the session ends, unresolved commands fail. A reconnect
does not resend them. At most four commands wait for a result per session. After
`ok: true` for `backlight`, HA shows the commanded state until the next telemetry
replaces it.

**Telemetry.** `firmware_version` is a string of at most 32 characters,
`uptime_s` and `free_heap` (bytes) are integers from 0 to 4294967295, `rssi_dbm`
is an integer from -127 to 0 or `null` without a Wi-Fi link, `reset_reason` is
one of `power_on`, `software`, `panic`, `watchdog`, `brownout`, `deep_sleep`,
`external` or `unknown`, and `backlight` is `{on, brightness}` with the actual
level from 0 to 100. Telemetry contains no identifiers, addresses, keys, tokens
or log text. A message is at most 512 bytes of JSON. The tablet sends one right
after the session is confirmed, whenever the backlight changes (also after a
command, a touch wake or an idle change), when the signal moves by 6 dB or more,
and otherwise every 60 seconds; at most one message every 5 seconds. HA keeps
only the latest telemetry in memory and discards it with the session.

**Gating and negotiation.** The tablet's capabilities carry an optional `live`
list of the supported parts: `wake`, `backlight` and `telemetry` (`backlight`
implies a brightness actuator). A missing list means none; existing enrollments
keep it empty until the tablet is approved again, like D4. HA answers
`njorun/session_subscribe` with `{session_id, live}`, where `live` lists the
parts that the tablet reported and this integration implements. An integration
without D6 omits `live`, which means none. The tablet sends no `telemetry` and
`command_result` unless `live` lists the part, and answers a command for another
part with `unsupported`. This keeps a newer tablet from sending frames that an
older integration would treat as a violation and close the session for. HA
creates the Wake now button, the Backlight light and the diagnostic sensors only
for the parts in `live` and sends only those commands.

**Separation from settings.** The backlight command changes the live display
state and nothing else. It does not write the `brightness` setting, a profile,
an override or a revision, and the configuration state is not acknowledged by it.
After a restart and after the tablet applies any configuration, the display
returns to the `brightness` setting and the next telemetry reports it.

## Firmware install

Decision D7 of [plan 003](../plans/003-d1001-ha-management.md). HA has already
downloaded, verified and cached a signed image; the tablet pulls it from HA and
checks signature and digest itself. This section is the install protocol on the
Noise session. The download endpoint, the update entity and the manifest format
are separate. Four encrypted message types
use the existing session: `install` travels on the subscription like a
configuration event, the other three in `njorun/session_frame` like `applied`.
Like live commands they are transient: an `install` is never persisted by the
tablet before it is accepted, never queued by HA and never repeated after a
reconnect. The [fixture](../../protocol/fixtures/install-messages.json) pins
accepted and rejected shapes for the Rust and Python tests.

| Message | From | Fields |
| --- | --- | --- |
| `install` | HA | `install_id`, `version`, `channel`, `size`, `sha256`, `url`, `token`, `expires_at` |
| `install_result` | tablet | `install_id`, `state`, `reason` |
| `install_progress` | tablet | `install_id`, `stage`, `done`, `total` |
| `install_outcome` | tablet | `install_id`, `state`, `version` |

Each message has exactly these fields plus `type`; others are rejected.

**Install.** `install_id` follows the operation ID rules and is a fresh UUID hex
from HA. `version` is the manifest version (at most 32 characters, no control
characters), `channel` is `stable` or `beta`, `size` is the byte size of the
application image (an integer from 1 to 4294967295) and `sha256` its digest as
64 lowercase hex digits, both as signed in the manifest. `url` is an absolute
`http(s)` URL of at most 512 characters without userinfo, query on the
authority or fragment, and `token` is a single-use download token of 32 to 128
characters from `A-Z a-z 0-9 _ -` (HA creates 32 random bytes as 43 characters
of unpadded URL-safe base64). `expires_at` is integer Unix seconds, five
minutes after sending; the token expires with it. The tablet sends the token
only as `Authorization: Bearer` header, never in the URL, and never logs it.

**Checks and answer.** The tablet answers every well-formed `install` with one
`install_result`. It checks in this order and rejects at the first failure with
`state: "rejected"` and a `reason`:

| Order | `reason` | Condition |
| --- | --- | --- |
| 1 | `malformed` | A field is missing, unknown or out of range, or `url` is not on the origin of the HA connection (the token must not go to another host). |
| 2 | `duplicate` | The `install_id` was seen on this session (the last 16 are kept). |
| 3 | `expired` | `expires_at` has passed. |
| 4 | `busy` | Another install is in progress. |
| 5 | `incompatible` | The session did not negotiate `ota`, the version is not `major.minor.patch` with an optional suffix, or it is below the firmware's anti-rollback floor. A deliberate downgrade above the floor is allowed. |
| 6 | `too_large` | `size` exceeds the 8 MiB OTA slot. |

Otherwise the answer is `state: "accepted"` with `reason: null` and the install
starts. A message that does not yield an `install_id` is dropped without an
answer. The ID of every well-formed install is remembered, also when it was
rejected, so HA retries with a fresh ID. Board ID, partition layout and
signature are not part of the message: the tablet checks them in the signed
manifest and the image header after the download. An install that fails after
`accepted` and before the reboot (download error, bad digest, bad signature,
wrong board, write error) leaves the running firmware untouched and ends with
`install_outcome` `rolled_back` and the running version, as the image never
became the running one.

**Progress.** After `accepted` the tablet sends `install_progress` with `stage`
`download`, `verify`, `write` or `reboot` in this order (a stage may be
skipped on failure but never repeated or reversed). `total` is the image size
and `done` the bytes handled in this stage, from 0 to `total`; `reboot` has
`done` equal `total`. The tablet sends at most one message per second and stage,
plus the first message of each stage and the end of a stage (`done` equal
`total`). HA ignores progress that arrives faster than that, drops progress for
an unknown or finished install and ends the session for a malformed message,
an unknown stage or a stage that moves backwards. Progress is not persisted.

**Outcome.** After the reboot the tablet reconciles: it reports `installed`
when the running image has the requested version and the new boot was
confirmed by the health check, and `rolled_back` otherwise, including an
unconfirmed boot and an install that failed before the reboot. It sends
exactly one `install_outcome` with `state` and the version that runs now, on the
first session after the boot (or on the current session when the install failed
without a reboot), and then forgets the install ID. The firmware persists the ID
and the requested version before it reboots for this.

**Session rules in HA.** HA sends an `install` only on a live session that
negotiated `ota`, never queues it and never resends it. At most one install per
tablet is open. HA accepts an `install_result` only for an `install` it sent on
that session and has not yet resolved, within 15 seconds; an unexpected result
is a protocol error that closes the session. `install_progress` and
`install_outcome` are accepted for an install that HA sent to this tablet and the
tablet accepted, until 30 minutes after `expires_at` (the reboot window), also
on a later session. An outcome for an unknown install (after an HA restart or
after the window) is logged and ignored; HA then reconciles from the version in
telemetry. Nothing is stored as applied on the HA side until the tablet reports
`installed` or `rolled_back`; `accepted` and progress are live state only. A
session that ends while an install is waiting for its result fails it; one that
ends after `accepted` keeps it open for the reboot window.

**Negotiation.** The tablet's capabilities carry an optional
`ota: {board}` with the board ID it runs on. A missing `ota` means no install
support; existing enrollments keep it empty until the tablet is approved again,
like D4 and D6. HA answers `njorun/session_subscribe` with `{session_id, live,
ota}`, where `ota` is `true` when the tablet reported the capability and this
integration implements D7. An integration without D7 omits `ota`, which means
`false`. The tablet answers an `install` on a session without `ota` with
`incompatible` and sends none of the tablet messages; HA sends an `install`
only to a tablet whose reported board matches the board in the manifest.

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
