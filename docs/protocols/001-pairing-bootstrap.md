# Pairing and recovery protocol candidate

Status: M0 host prototype, not a qualified tablet transport.
Related criteria: M0-A7/A8 in the
[blueprint](../plans/001-embedded-ha-blueprint.md).
This specification resolves the protocol candidate and states the remaining
bootstrap and hardware gates. It does not certify a secure product.

## Trust boundary

The trusted endpoints are HA's integration and the tablet application. The
browser is a relay after bootstrap. The first enrollment additionally trusts
the administrator's browser session to HA and the physically attached USB
device. A compromised administrator browser, HA server, firmware, or physical
USB path during initial key installation is outside this candidate's protection.

The expected HA public key must arrive through an authenticated source.
An HTTPS installer page with a signed firmware image establishes only the
installer/image origin. It cannot authenticate an HA key received through an
actively modified HTTP page.

For the initial candidate, use an authenticated HTTPS HA administrator session
and a physically selected USB tablet to exchange the public static keys.
A deployment starting from an HTTP HA URL must obtain a trusted HA identity
through an authenticated HTTPS endpoint or a separately reviewed out-of-band
channel before activation. No prototype treats an HTTP-fetched key as trusted.
The approved HTTP entry flow remains an open M0 qualification gate until this
bootstrap is demonstrated in the intended UX.

## Initial enrollment ceremony

1. The administrator selects Install a new tablet, board and desired profile
   in HA. HA authorizes the operation and creates a pending binding.
2. The installer verifies the signed factory image and flashes the explicitly
   selected USB target. Reopen its application serial interface after reboot.
3. The tablet creates a persistent static keypair using its platform CSPRNG.
   HA creates a per-binding static keypair. Private keys stay at their endpoint.
4. Through the authenticated bootstrap path, install HA's public key on that
   physically attached tablet and register the tablet public key on the pending
   HA binding. Bind this exchange to the administrator-approved pending record.
   A serial number or claimed device identifier is not sufficient.
5. Complete the pinned-peer handshake below and acknowledge the pending binding
   over the encrypted channel. Mark it active only after durable storage and
   mutual confirmation. An interrupted enrollment remains retryable. Activation
   must not silently replace an already enrolled identity.
6. Configure connection details through the authenticated channel. HA dashboard
   OAuth authorization remains a separate user/session authorization step.

A production implementation needs an atomic persistence adapter, expiring pending
records, an enrollment journal and explicit replacement authorization. The
current host crate tests the handshake/channel boundary only.

## Cryptographic candidate

Use the standard Noise IK pattern with X25519, ChaCha20-Poly1305 and SHA-256,
implemented by the pinned Snow library. Tablet is initiator and pins HA's
responder public key. HA validates the decrypted initiator static key against
the approved tablet binding before granting a management channel. Both ends
use fresh ephemeral keys for every session.

The fixed protocol/version prologue and a 32-byte connection context bind the
exchange to its expected session. Transport messages are permitted only after
the handshake completes and the expected peer has been checked. Handshake
payloads carry no configuration. The responder must not mutate configuration
on receipt of a replayed first handshake message. `is_established()` reports only
local Noise transport availability, not mutual confirmation or peer liveness.
HA must receive an authenticated initiator transport confirmation before any
enrollment activation or management effect. Delivery of the second handshake
message alone is insufficient.

The production session coordinator must issue a fresh unpredictable 32-byte
context per connection, bind it to the pending/active device record, expire it
and consume it once. The host library binds the provided bytes but does not
create or track them. Even with a fresh context, application confirmation is
required; the first IK flight alone cannot establish freshness.

The prototype consumes caller-provided static key bytes and trusted public-key
pins. This explicit interface keeps the unimplemented trust ceremony visible.
A successful cryptographic handshake does not prove that the initial pins
came from an authenticated source.
[Noise specification](https://noiseprotocol.org/noise.html)
[Snow 0.10 API](https://docs.rs/snow/0.10.0/snow/)

## Framing and application responsibilities

The host prototype accepts complete bounded messages. The production USB
adapter must turn its byte stream into length-prefixed frames with a bounded
buffer and timeout, reject oversized/truncated frames and demultiplex diagnostics
without interpreting console output as commands. WebSocket management can carry
the same complete protocol messages over the existing HA connection.

Encrypted transport uses ordered Noise counters. Failed handshake authentication,
malformed handshake messages and transport authentication failures (including replay)
close the affected endpoint. A reconnect starts a new handshake. Local API calls in
the wrong phase and size-limit rejection return an error without advancing state
or consuming a nonce. The framing adapter must enforce its own close policy for
invalid wire ordering and oversized frames.
Plaintext configuration messages are bounded to 4096 bytes in this experiment.
Large data must use a separately specified bounded transfer protocol.

The management envelope must contain an operation ID, expiry, expected settings
revision and allowlisted operation. Apply/deduplicate only after authentication,
authorization and capability validation. These application semantics remain
covered by the blueprint and are not implemented by opaque encrypted byte frames.

## Persistence, recovery and limits

Persist the per-device binding and static key material independently of shared
configuration profiles. Restore the same public-key pins after ordinary restart.
Generate fresh ephemeral keys and counters, never restore an old transport cipher
state. Key storage confidentiality and crash consistency need the P4 NVS adapter
and a separately qualified security profile.

Existing-trust USB recovery uses the same pinned-peer handshake. A missing or
different peer key requires explicit fresh provisioning. Reset/replacement
creates fresh credentials and revokes the old binding. A wrong PIN does not
authorize recovery; HA administrator authorization plus device proof does.

Required counter-probes include wrong HA key, wrong tablet key, different
connection context, handshake tampering, replay, malformed/oversized frames,
attempted transport before handshake completion and session closure after error.
Device CSPRNG, NVS restart/power-loss handling, physical USB framing, HA-side
interoperability and the HTTP bootstrap remain unqualified by host tests.

## Local execution

The workspace's pinned toolchain and lockfile define the host experiment.
Run the commands documented in [CONTRIBUTING](../../CONTRIBUTING.md).
The prototype has no firmware flashing or live-HA side effects.
