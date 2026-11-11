//! Announcing without a Home Assistant account (plan 004): the request body, Home Assistant's
//! answer and the grant that carries the tablet's access.
//!
//! The tablet posts its announcement to `POST /api/njorun/announce` and repeats it while it
//! waits. No transport lives here: the firmware brings the HTTP client and passes the status
//! and body. See `docs/plans/004-tablet-first-pairing.md` and
//! `docs/protocols/002-ha-management.md`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use njorun_pairing::open_one_way;
use serde_json::{Value, json};

use crate::{Announcement, Error, PROTOCOL, comparison_code, encode_key};

const PROLOGUE_PREFIX: &[u8] = b"njorun/grant\x01";
const MAX_TOKEN_BYTES: usize = 8192;
const MAX_CLIENT_ID_BYTES: usize = 256;

/// The Home Assistant access that an approved tablet receives.
#[derive(Clone, PartialEq, Eq)]
pub struct Grant {
    pub refresh_token: String,
    pub client_id: String,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The token is a credential and never reaches a log.
        formatter
            .debug_struct("Grant")
            .field("refresh_token", &"<redacted>")
            .field("client_id", &self.client_id)
            .finish()
    }
}

/// What the announcement endpoint answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// Waiting for an administrator, who sees this code next to the tablet.
    Pending { code: String },
    /// Approved. `grant` is present until the tablet's first confirmed session, and when the
    /// tablet asked for one and none was valid.
    Approved {
        binding_id: String,
        ha_public: [u8; 32],
        grant: Option<Grant>,
    },
    /// Another key is paired under this tablet ID.
    Conflict,
    /// The address answers 404: Home Assistant runs, the integration is not installed.
    NotInstalled,
    /// Too many requests from this address; the tablet waits longer before the next one.
    RateLimited,
    /// Home Assistant refused to take another waiting tablet, or the integration is unloading.
    Busy,
    /// Home Assistant rejected the request itself (status 400).
    Rejected,
}

/// The JSON body of an announcement.
#[must_use]
pub fn announce_body(
    tablet_id: &str,
    name: &str,
    tablet_public: &[u8; 32],
    capabilities: &Value,
    need_grant: bool,
) -> String {
    let mut body = json!({
        "protocol": PROTOCOL,
        "tablet_id": tablet_id,
        "name": name,
        "tablet_public_key": encode_key(tablet_public),
        "capabilities": capabilities,
    });
    if need_grant {
        body["need_grant"] = json!(true);
    }
    body.to_string()
}

/// Opens a grant sealed to this tablet's key for this binding.
///
/// # Errors
/// [`Error::Trust`] when the message does not open with this key and binding;
/// [`Error::Protocol`] when the payload is not exactly a refresh token and a client ID of
/// sensible size.
pub fn open_grant(
    tablet_private: &[u8; 32],
    binding_id: &str,
    grant: &str,
) -> Result<Grant, Error> {
    let message = STANDARD.decode(grant).map_err(|_| Error::Protocol)?;
    let mut prologue = PROLOGUE_PREFIX.to_vec();
    prologue.extend_from_slice(binding_id.as_bytes());
    let plaintext = open_one_way(tablet_private, &prologue, &message).map_err(|_| Error::Trust)?;
    let payload: Value = serde_json::from_slice(&plaintext).map_err(|_| Error::Protocol)?;
    let object = payload.as_object().ok_or(Error::Protocol)?;
    if object.len() != 2 {
        return Err(Error::Protocol);
    }
    let refresh_token = object
        .get("refresh_token")
        .and_then(Value::as_str)
        .ok_or(Error::Protocol)?;
    let client_id = object
        .get("client_id")
        .and_then(Value::as_str)
        .ok_or(Error::Protocol)?;
    if refresh_token.is_empty()
        || refresh_token.len() > MAX_TOKEN_BYTES
        || client_id.is_empty()
        || client_id.len() > MAX_CLIENT_ID_BYTES
    {
        return Err(Error::Protocol);
    }
    Ok(Grant {
        refresh_token: refresh_token.to_owned(),
        client_id: client_id.to_owned(),
    })
}

/// Reads the answer of the announcement endpoint.
///
/// A pending code must match the tablet's own code, so a tampered answer cannot make the tablet
/// show a code that belongs to another key. A grant that does not open is an error, not a missing
/// grant.
///
/// # Errors
/// [`Error::Protocol`] for an unexpected status or body, a code that does not match, or a
/// malformed grant; [`Error::Trust`] for a malformed key or a grant that does not open.
pub fn parse_reply(
    status: u16,
    body: &[u8],
    tablet_public: &[u8; 32],
    tablet_private: &[u8; 32],
) -> Result<Reply, Error> {
    match status {
        404 => return Ok(Reply::NotInstalled),
        400 => return Ok(Reply::Rejected),
        429 => return Ok(Reply::RateLimited),
        503 => return Ok(Reply::Busy),
        200 => {}
        _ => return Err(Error::Protocol),
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| Error::Protocol)?;
    match crate::parse_announce(&value, tablet_public)? {
        Announcement::Pending { code } => Ok(Reply::Pending { code }),
        Announcement::Conflict => Ok(Reply::Conflict),
        Announcement::Approved {
            binding_id,
            ha_public,
        } => {
            let grant = match value.get("grant") {
                None | Some(Value::Null) => None,
                Some(Value::String(text)) => Some(open_grant(tablet_private, &binding_id, text)?),
                Some(_) => return Err(Error::Protocol),
            };
            Ok(Reply::Approved {
                binding_id,
                ha_public,
                grant,
            })
        }
    }
}

/// The six digits with a space in the middle, as Home Assistant shows them.
#[must_use]
pub fn display_code(code: &str) -> String {
    if code.len() == 6 {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_owned()
    }
}

/// The tablet's code for its own key, for tests and the first screen.
#[must_use]
pub fn own_code(tablet_public: &[u8; 32]) -> String {
    comparison_code(tablet_public)
}
