# Pairing without a browser login on the tablet

This plan replaces the sign-in step of the tablet's first start. Today the
tablet shows a QR code, an administrator signs in to Home Assistant on a phone,
and the tablet gets that user's token. After this plan the tablet only needs the
Home Assistant URL. It announces itself, an administrator approves it in Home
Assistant, and Home Assistant hands it its own access. It follows decision D3 of
[plan 003](003-d1001-ha-management.md) (interim pairing by comparison code) and
changes how the tablet gets its Home Assistant access; it does not replace the
browser/USB ceremony of [protocol 001](../protocols/001-pairing-bootstrap.md).

## Baseline (2026-11-11)

- The tablet finds Home Assistant by mDNS or takes a URL typed on it, signs in
  through the browser (PKCE, a local callback on the tablet, QR code), keeps the
  refresh token in NVS and announces itself with `njorun/announce`. That command
  needs a signed-in Home Assistant user.
- The announcement becomes a **Discovered** card (`integration_discovery`). An
  administrator compares the six-digit code, picks a profile and approves. The
  flow ends with an abort; tablets are records inside the single integration
  entry, not entries of their own.
- The session (`njorun/start_session`, `njorun/session_frame`) and the dashboard
  both use the tablet's Home Assistant token over the WebSocket API.

## Outcome

An administrator installs the integration once. A tablet needs a network and the
Home Assistant URL, and then shows its ID and what to do in Home Assistant. The
administrator approves it on the Discovered card, with a shared profile or a new
profile for that tablet only. The tablet starts working without any sign-in on
it. When the URL cannot be used, the tablet says why. Removing the tablet in
Home Assistant removes its access.

## Decisions for review

- **P1 The tablet stays the initiator.** It announces itself to the URL it was
  given. Home Assistant does not look for tablets. This works across VLANs, needs
  no server on the tablet and lets the tablet test the URL before pairing. A
  second path, "add a tablet by its address" from Home Assistant, is not part of
  this plan.
- **P2 An announcement endpoint without sign-in.** `POST /api/njorun/announce`
  of the integration takes the same fields as `njorun/announce` and returns the
  same results. Nothing it receives is trusted before an administrator compares
  the code. Limits: the body size of an unauthenticated view (64 KiB in Home
  Assistant), at most 16 waiting tablets for at most ten minutes (as today), and
  a rate limit per client address. `request.remote` is the client address behind
  a trusted proxy. The endpoint is always on (no pairing window); a pairing
  window can be added later if the network needs it. The WebSocket command
  `njorun/announce` stays for tablets that announce that way and uses the same
  code.
- **P3 Home Assistant creates the tablet's access at approval.** One user per
  tablet, named after it (for example `Tablet Kitchen`), in the group
  `system-users`, `local_only`, with one refresh token of the normal type and a
  fixed `client_id`. The tablet refreshes its access token with the existing
  refresh flow. A normal user is necessary: Home Assistant refuses a normal
  refresh token for a system user, and a system user cannot carry a `client_id`
  (`AuthManager.async_create_refresh_token`). Removing the tablet removes the
  user and with it the refresh token.
- **P4 The grant is encrypted to the tablet's key.** The refresh token travels in
  the `approved` answer of the announcement endpoint as one Noise `N` message
  (`Noise_N_25519_ChaChaPoly_SHA256`) that Home Assistant writes to the tablet's
  announced public key. The announcement endpoint is public, so the grant must be
  unreadable for everybody but the tablet. The grant holds the refresh token and
  its `client_id`; the tablet keeps using the URL it announced to. The layout is
  fixed under "Messages" and pinned by
  [grant.json](../../protocol/fixtures/grant.json).
- **P5 The comparison code stays as it is.** It covers the tablet's key, and the
  grant is sealed to that same key. An attacker who changes the key in transit
  changes the code the administrator sees, so the administrator notices. This
  protects the tablet's key, not the channel. The tablet checks the certificate of
  an HTTPS URL as it does now; an HTTP URL is not safe against a man in the
  middle and the existing rule for HTTP URLs stays.
- **P6 Re-delivery.** Home Assistant answers the same announcement with the grant
  until the first confirmed session of the binding, so a lost answer costs
  nothing. After that the endpoint answers `approved` without a grant. If the
  token has become invalid (user removed, token expired after a long time
  offline), a tablet that announces again with the same key and says so
  (`need_grant`) gets a new grant. Home Assistant never revokes a working token
  because of an announcement, so someone who knows the public key can cause new
  tokens at most when there is no valid one.
- **P7 Profiles.** The approval step offers the existing shared profiles and a
  third choice, a new profile for this tablet only, named after it. Overrides on
  one tablet stay as they are.

## Messages

`POST /api/njorun/announce`, `Content-Type: application/json`. Request fields:
`protocol` (1), `tablet_id`, `name`, `tablet_public_key` (base64, 32 bytes),
`capabilities` (as in `njorun/announce`) the optional `need_grant` (boolean,
default false) and the optional `theme` (`light`, `dark` or `automatic`, plan
[006](006-network-and-theme.md) N1). The answer is JSON:

| Status | Body | Meaning |
| --- | --- | --- |
| 200 | `{"status": "pending", "code": "123456"}` | Waiting for approval. The tablet repeats the request every ten seconds. |
| 200 | `{"status": "approved", "binding_id", "ha_public_key"}` plus `"grant"` | Approved. `grant` is present until the first confirmed session of the binding, and when `need_grant` was true and no valid token exists. |
| 200 | `{"status": "conflict"}` | Another key is paired under this tablet ID. |
| 400 | `{"error": "invalid", "detail": ...}` | A field is missing or malformed. |
| 404 | none | The integration is not installed (Home Assistant's own answer). |
| 429 | `{"error": "rate_limited"}` with `Retry-After` | Too many requests from this address. |
| 503 | `{"error": "too_many_waiting"}` or `{"error": "unloading"}` | The limit of waiting tablets is reached, or the integration is unloading. |

`grant` is the base64 of one Noise message of the pattern
`Noise_N_25519_ChaChaPoly_SHA256`. Home Assistant is the initiator and uses a
fresh ephemeral key per message; the tablet is the responder with its static key.
The prologue is the bytes `njorun/grant`, `0x01` and the binding ID in UTF-8. The
payload is the JSON `{"refresh_token": ..., "client_id": ...}`. The tablet
refreshes with `client_id` exactly as given.

## What the tablet shows

After the URL step the tablet tests the endpoint and then shows one of:

- **Waiting for approval:** its name, its ID as it appears in Home Assistant, the
  six-digit code and where to approve it (Settings, Devices & services,
  Discovered). The existing approval page, without the sign-in steps.
- **A reason it cannot reach Home Assistant:** name not found, no route, no
  answer (firewall), certificate not accepted, or "the Njörun integration is not
  installed" when the address answers 404. The texts of `problems.rs` are the
  base. The tablet tries again and keeps the reason on screen.

The QR code, the PKCE session and the local callback listener are removed from
the first start.

## Slices

- **S1 Integration.** The announcement endpoint with its limits, the shared
  announce code, the approval step with the profile choice (shared or new), user
  and refresh token creation, the sealed grant, re-delivery, removal of the user
  with the tablet, tests (Python), the protocol text in
  [002](../protocols/002-ha-management.md) and a fixture for the grant and the
  endpoint messages.
- **S2 Firmware.** The URL step without sign-in, the announcement over HTTP(S),
  opening the grant (Noise `N`), storing the refresh token and `client_id` in the
  existing session record, the URL test and its error screens, removal of the QR
  and PKCE screens, tests on the host (Rust). Needs the fixture of S1.
- **S3 Qualification.** The simulator and the integration test play the whole
  path (URL, announcement, approval, grant, session). On the tablet: pairing, a
  lost answer, an invalid URL, a removed tablet.

S1 and S2 follow the same message fixture, so they can be built in parallel.

## Verified and not verified

Read in the Home Assistant source (branch `dev`, 2026-11-11):

- `async_create_user(name, group_ids, local_only)`; `async_create_refresh_token`
  refuses a normal token without `client_id`, a `client_id` on a system user and a
  normal token for a system user; a normal token expires after
  `REFRESH_TOKEN_EXPIRATION` and the expiry is extended on use
  (`async_log_refresh_token_usage`).
- `POST /auth/token` with `refresh_token` needs a `client_id` equal to the token's
  and checks `local_only` with `request.remote` (`async_user_not_allowed_do_auth`).
- A `client_id` must be an `http(s)` URL with a host name; a plain name is refused.
- `HomeAssistantView` with `requires_auth = False` is the way to an endpoint
  without sign-in; unauthenticated views are limited to 64 KiB.
- `request.remote` is the address from `X-Forwarded-For` when the peer is in
  `trusted_proxies`, which the household instance sets.
- The WebSocket commands the dashboard needs (`get_states`, `subscribe_entities`,
  `call_service`) require no administrator; reads are filtered by the user's
  policy.

Not verified, to be checked while building S1:

- The value of `REFRESH_TOKEN_EXPIRATION` and what happens to a token after its
  expiry (`auth/const.py`, `auth/__init__.py`).
- Whether `call_service` checks the user's permissions and where (`core.py`,
  `helpers/service.py`). The group `system-users` has the policy for all entities.
- Whether the Python `noiseprotocol` package supports the one-way pattern `N`; if
  it does not, the grant uses X25519, HKDF and ChaCha20-Poly1305 with the same
  message layout instead.
- Whether an integration that creates users and tokens meets the Home Assistant
  quality rules; nothing in the files read forbids it.

## Risks

- The endpoint is the first one of the integration that anybody on the network
  can call. The limits above keep it from filling Discovered cards; the code
  comparison keeps it from pairing anything unattended.
- Tablets that are paired today keep their token and need no change. A tablet
  with the new firmware that was paired the old way keeps working. The old
  sign-in path in the firmware is removed only in S2, after the new one is
  qualified.
- The refresh token sits in NVS in plaintext, as before (see
  [device persistence](../device-persistence.md)).
