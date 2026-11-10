//! The Njörun management session (protocol 1) without any transport.
//!
//! The tablet opens it on Home Assistant's authenticated WebSocket: `njorun/session_start`,
//! a Noise IK handshake through `njorun/session_handshake`, an encrypted confirmation in
//! `njorun/session_subscribe`, then encrypted events from HA and `njorun/session_frame`
//! acknowledgements from the tablet. This crate builds those requests (without the HA `id`,
//! which the transport assigns), checks HA's results and events, and keeps the Noise state.
//! The host client and the firmware each bring their own WebSocket.
//!
//! See `docs/protocols/002-ha-management.md`.

use std::fmt::{Display, Formatter};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use njorun_pairing::Initiator;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub use njorun_pairing::public_key;

/// Management protocol version.
pub const PROTOCOL: u64 = 1;

const MAX_ID_BYTES: usize = 128;
const MAX_KEY_TEXT: usize = 44;
const MAX_FRAME_TEXT: usize = 5500;
const CLOSE_REASONS: [&str; 4] = ["expired", "revoked", "removed", "unloaded"];

/// Why a session step failed. Messages never contain keys or frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// HA's answer or event does not have the expected shape.
    Protocol,
    /// Key, handshake or frame authentication failed; the session cannot continue.
    Trust,
    /// A frame or message exceeds its size limit.
    Limit,
    /// HA closed the session (expired, revoked, removed or unloaded).
    Closed,
}

impl Display for Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Protocol => "unexpected management message",
            Self::Trust => "management authentication failed",
            Self::Limit => "management message too large",
            Self::Closed => "management session closed by Home Assistant",
        })
    }
}

impl std::error::Error for Error {}

fn identifier(value: &Value) -> Result<&str, Error> {
    value
        .as_str()
        .filter(|text| {
            !text.is_empty() && text.len() <= MAX_ID_BYTES && !text.chars().any(char::is_control)
        })
        .ok_or(Error::Protocol)
}

/// Decodes a standard-padded base64 32-byte key or context.
///
/// # Errors
/// [`Error::Trust`] for anything but exactly 32 bytes.
pub fn decode_key(text: &str) -> Result<[u8; 32], Error> {
    if text.len() > MAX_KEY_TEXT {
        return Err(Error::Trust);
    }
    STANDARD
        .decode(text)
        .map_err(|_| Error::Trust)?
        .try_into()
        .map_err(|_| Error::Trust)
}

/// Encodes a 32-byte key as HA expects it.
#[must_use]
pub fn encode_key(key: &[u8; 32]) -> String {
    STANDARD.encode(key)
}

fn decode_frame(value: &Value) -> Result<Vec<u8>, Error> {
    let text = value.as_str().ok_or(Error::Protocol)?;
    if text.len() > MAX_FRAME_TEXT {
        return Err(Error::Limit);
    }
    STANDARD.decode(text).map_err(|_| Error::Protocol)
}

/// The six digits the tablet shows while it waits for approval; Home Assistant shows the same
/// digits next to the announced tablet. Derived from the tablet's public key only.
#[must_use]
pub fn comparison_code(tablet_public: &[u8; 32]) -> String {
    let digest = Sha256::new()
        .chain_update(b"njorun/compare")
        .chain_update([1_u8])
        .chain_update(tablet_public)
        .finalize();
    let value = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{value:06}")
}

/// The `njorun/announce` request: the tablet asks to be paired, or for its approved binding.
#[must_use]
pub fn announce_request(
    tablet_id: &str,
    name: &str,
    tablet_public: &[u8; 32],
    capabilities: &Value,
) -> Value {
    json!({
        "type": "njorun/announce",
        "protocol": PROTOCOL,
        "tablet_id": tablet_id,
        "name": name,
        "tablet_public_key": encode_key(tablet_public),
        "capabilities": capabilities,
    })
}

/// Home Assistant's answer to an announcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Announcement {
    /// Waiting for an administrator, who sees this code next to the tablet.
    Pending { code: String },
    /// Approved: the binding to start sessions with and the HA key to pin.
    Approved {
        binding_id: String,
        ha_public: [u8; 32],
    },
    /// Another key is paired under this tablet ID.
    Conflict,
}

/// Reads the `njorun/announce` result. A pending code must match the tablet's own code, so a
/// tampered answer cannot make the tablet show a code that belongs to another key.
///
/// # Errors
/// [`Error::Protocol`] for an unknown status or a code that does not match; [`Error::Trust`]
/// for a malformed HA key.
pub fn parse_announce(result: &Value, tablet_public: &[u8; 32]) -> Result<Announcement, Error> {
    match result["status"].as_str() {
        Some("pending") => {
            let code = result["code"].as_str().ok_or(Error::Protocol)?;
            let own = comparison_code(tablet_public);
            if code != own {
                return Err(Error::Protocol);
            }
            Ok(Announcement::Pending { code: own })
        }
        Some("approved") => Ok(Announcement::Approved {
            binding_id: identifier(&result["binding_id"])?.to_owned(),
            ha_public: decode_key(result["ha_public_key"].as_str().ok_or(Error::Protocol)?)?,
        }),
        Some("conflict") => Ok(Announcement::Conflict),
        _ => Err(Error::Protocol),
    }
}

/// The `njorun/session_start` request for an enrolled binding.
#[must_use]
pub fn session_start_request(binding_id: &str) -> Value {
    json!({"type": "njorun/session_start", "protocol": PROTOCOL, "binding_id": binding_id})
}

/// The live parts of decision D6 that a session can negotiate.
pub const LIVE_PARTS: [&str; 3] = ["wake", "backlight", "telemetry"];

/// Reads the `live` list of an `njorun/session_subscribe` result.
///
/// It names the parts that the tablet reported and the integration implements. A missing list
/// (an integration without D6) means none. Unknown names are ignored so that a newer
/// integration can offer more.
///
/// # Errors
/// [`Error::Protocol`] when `live` is present but not a list of at most eight strings.
pub fn live_parts(subscribe_result: &Value) -> Result<Vec<&'static str>, Error> {
    let Some(list) = subscribe_result.get("live") else {
        return Ok(Vec::new());
    };
    let list = list
        .as_array()
        .filter(|list| list.len() <= 8)
        .ok_or(Error::Protocol)?;
    let mut parts = Vec::new();
    for item in list {
        let name = item.as_str().ok_or(Error::Protocol)?;
        if let Some(known) = LIVE_PARTS.iter().find(|known| **known == name)
            && !parts.contains(known)
        {
            parts.push(*known);
        }
    }
    Ok(parts)
}

/// Reads the `ota` flag of an `njorun/session_subscribe` result (decision D7).
///
/// It is `true` when the tablet reported an `ota` capability and the integration implements the
/// install protocol. A missing flag (an integration without D7) means `false`, so a newer
/// tablet never expects an `install` from an older integration and an older integration never
/// receives an `install_result` it would close the session for.
///
/// # Errors
/// [`Error::Protocol`] when `ota` is present but not a boolean.
pub fn ota_offered(subscribe_result: &Value) -> Result<bool, Error> {
    subscribe_result
        .get("ota")
        .map_or(Ok(false), |flag| flag.as_bool().ok_or(Error::Protocol))
}

/// A session between `session_start` and the handshake response.
pub struct Handshake {
    noise: Initiator,
    session_id: String,
    binding_id: String,
}

impl Handshake {
    /// Reads HA's `session_start` result and returns the `session_handshake` request.
    ///
    /// # Errors
    /// [`Error::Protocol`] for a malformed result, [`Error::Trust`] for bad keys or context.
    pub fn begin(
        start_result: &Value,
        binding_id: &str,
        tablet_private: &[u8; 32],
        ha_public: &[u8; 32],
    ) -> Result<(Self, Value), Error> {
        let session_id = identifier(&start_result["session_id"])?.to_owned();
        let context = decode_key(start_result["context"].as_str().ok_or(Error::Protocol)?)?;
        let mut noise =
            Initiator::new(tablet_private, ha_public, &context).map_err(|_| Error::Trust)?;
        let first = noise.start().map_err(|_| Error::Trust)?;
        let request = json!({
            "type": "njorun/session_handshake",
            "session_id": session_id,
            "frame": STANDARD.encode(first),
        });
        Ok((
            Self {
                noise,
                session_id,
                binding_id: binding_id.to_owned(),
            },
            request,
        ))
    }

    /// Completes the handshake from HA's `session_handshake` result and returns the
    /// `session_subscribe` request carrying the encrypted confirmation.
    ///
    /// # Errors
    /// [`Error::Trust`] when HA's response does not prove the pinned key.
    pub fn finish(mut self, handshake_result: &Value) -> Result<(Session, Value), Error> {
        let response = decode_frame(&handshake_result["frame"])?;
        self.noise.finish(&response).map_err(|_| Error::Trust)?;
        let confirm = json!({
            "type": "confirm",
            "protocol": PROTOCOL,
            "binding_id": self.binding_id,
            "session_id": self.session_id,
        });
        let frame = self
            .noise
            .seal_frame(confirm.to_string().as_bytes())
            .map_err(|_| Error::Trust)?;
        let request = json!({
            "type": "njorun/session_subscribe",
            "session_id": self.session_id,
            "frame": STANDARD.encode(frame),
        });
        Ok((
            Session {
                noise: self.noise,
                id: self.session_id,
                subscription: None,
                terminal: None,
            },
            request,
        ))
    }
}

/// An established session.
pub struct Session {
    noise: Initiator,
    id: String,
    subscription: Option<u64>,
    terminal: Option<Error>,
}

impl Session {
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.id
    }

    /// The HA message ID of the `session_subscribe` request; events carry it as their `id`.
    pub const fn attach(&mut self, subscription_id: u64) {
        self.subscription = Some(subscription_id);
    }

    #[must_use]
    pub const fn subscription_id(&self) -> Option<u64> {
        self.subscription
    }

    /// Decrypts one subscription event. A failed authentication or a close ends the session:
    /// every later call returns the same error.
    ///
    /// # Errors
    /// [`Error::Protocol`] for another subscription or malformed content, [`Error::Trust`] for
    /// tampering or replay, [`Error::Closed`] when HA closed the session.
    pub fn decode(&mut self, event: &Value) -> Result<Value, Error> {
        if let Some(error) = self.terminal {
            return Err(error);
        }
        if self.subscription.is_none()
            || event["id"].as_u64() != self.subscription
            || event["type"] != "event"
        {
            return Err(Error::Protocol);
        }
        let frame = decode_frame(&event["event"]["frame"])?;
        let Ok(plaintext) = self.noise.open_frame(&frame) else {
            self.terminal = Some(Error::Trust);
            return Err(Error::Trust);
        };
        let message: Value = serde_json::from_slice(&plaintext).map_err(|_| Error::Protocol)?;
        if message["type"] == "closed" {
            let valid = message["protocol"] == PROTOCOL
                && message["session_id"] == self.id.as_str()
                && message["reason"]
                    .as_str()
                    .is_some_and(|reason| CLOSE_REASONS.contains(&reason))
                && message.as_object().is_some_and(|object| object.len() == 4);
            if !valid {
                return Err(Error::Protocol);
            }
            self.terminal = Some(Error::Closed);
            return Err(Error::Closed);
        }
        Ok(message)
    }

    /// Encrypts `payload` into a `session_frame` request, such as an `applied` acknowledgement.
    ///
    /// # Errors
    /// [`Error::Limit`] for an oversize payload; the terminal error after a failed session.
    pub fn frame_request(&mut self, payload: &Value) -> Result<Value, Error> {
        if let Some(error) = self.terminal {
            return Err(error);
        }
        let frame = self
            .noise
            .seal_frame(payload.to_string().as_bytes())
            .map_err(|error| match error {
                njorun_pairing::PairingError::FrameTooLarge => Error::Limit,
                _ => Error::Trust,
            })?;
        Ok(json!({
            "type": "njorun/session_frame",
            "session_id": self.id,
            "frame": STANDARD.encode(frame),
        }))
    }
}
