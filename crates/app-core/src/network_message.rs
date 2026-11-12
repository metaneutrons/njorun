//! The `network` message from Home Assistant and the `network_result` the tablet answers with
//! (plan 006, decisions N2, N5 and N7).
//!
//! The tablet checks a `network` message the way it checks a configuration: shape, expiry,
//! revision and values. Unlike a configuration, a refusal is answered, so that Home Assistant is
//! never left guessing. A result never holds a password.
//!
//! See `docs/protocols/002-ha-management.md` and `protocol/fixtures/network-messages.json`.

use std::net::Ipv4Addr;

use serde_json::{Map, Value, json};

use crate::network::{EthernetConfig, Interface, Mode, NetworkConfig, Problem, WifiConfig};
use crate::trial::{Proof, Reason};

/// The longest operation identifier, as for the other messages.
pub const MAX_OPERATION_ID_BYTES: usize = 128;
/// The largest revision, so that it fits every JSON reader.
pub const MAX_REVISION: u64 = 4_294_967_295;
/// The largest encoded `network_result`.
pub const MAX_RESULT_BYTES: usize = 768;

/// A `network` message that passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub operation_id: String,
    pub revision: u64,
    pub config: NetworkConfig,
}

/// Why a `network` message was refused. Every one is answered with `rejected_values`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// Wrong shape, an unknown field or a value of the wrong type.
    Malformed,
    /// `expires_at` has passed.
    Expired,
    /// The revision is older than the last one the tablet finished.
    Stale,
    /// The values do not pass the validation of plan 006 N3.
    Invalid(Problem),
}

/// A refused message and what is known of it. The identifiers are present only when they could
/// be read, because without them the answer cannot be matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub rejection: Rejection,
    pub operation_id: Option<String>,
    pub revision: Option<u64>,
}

impl Refusal {
    /// The `network_result` that answers this refusal, or `None` when the message did not
    /// carry the identifiers to answer with.
    #[must_use]
    pub fn result(&self, state: &State) -> Option<Value> {
        Some(result_message(
            self.operation_id.as_deref()?,
            self.revision?,
            Outcome::Reverted(Reason::RejectedValues),
            state,
        ))
    }
}

fn operation_id(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|text| {
        !text.is_empty()
            && text.len() <= MAX_OPERATION_ID_BYTES
            && !text.chars().any(char::is_control)
    })
}

fn revision(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .filter(|revision| (1..=MAX_REVISION).contains(revision))
}

/// Checks a decrypted `network` message.
///
/// `last_revision` is the newest revision the tablet has finished (applied or reverted). The
/// same revision passes, so that a repeated message can be answered; the caller decides. `now`
/// is Unix seconds and `interfaces` are the ones the board has.
///
/// # Errors
/// The [`Refusal`] to answer with.
pub fn accept_network(
    message: &Value,
    now: u64,
    last_revision: u64,
    interfaces: &[Interface],
) -> Result<Request, Refusal> {
    let refuse = |rejection| Refusal {
        rejection,
        operation_id: operation_id(message.get("operation_id")).map(str::to_owned),
        revision: revision(message.get("revision")),
    };
    let Some(fields) = message.as_object() else {
        return Err(refuse(Rejection::Malformed));
    };
    let known = [
        "type",
        "operation_id",
        "revision",
        "expires_at",
        "wifi",
        "ethernet",
    ];
    let (Some(id), Some(revision), Some(expires_at)) = (
        operation_id(fields.get("operation_id")),
        revision(fields.get("revision")),
        fields.get("expires_at").and_then(Value::as_u64),
    ) else {
        return Err(refuse(Rejection::Malformed));
    };
    if fields.get("type").and_then(Value::as_str) != Some("network")
        || fields.keys().any(|key| !known.contains(&key.as_str()))
    {
        return Err(refuse(Rejection::Malformed));
    }
    if expires_at <= now {
        return Err(refuse(Rejection::Expired));
    }
    if revision < last_revision {
        return Err(refuse(Rejection::Stale));
    }
    let interface_fields: Map<String, Value> = fields
        .iter()
        .filter(|(key, _)| matches!(key.as_str(), "wifi" | "ethernet"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let config: NetworkConfig = serde_json::from_value(Value::Object(interface_fields))
        .map_err(|_| refuse(Rejection::Malformed))?;
    config
        .validate(interfaces)
        .map_err(|problem| refuse(Rejection::Invalid(problem)))?;
    Ok(Request {
        operation_id: id.to_owned(),
        revision,
        config,
    })
}

/// How an attempt ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The new values work and are stored.
    Applied(Proof),
    /// The tablet is back on the previous values.
    Reverted(Reason),
}

/// What one interface is doing now. Nothing secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceState {
    pub mode: Mode,
    /// The address in use, if the interface has one.
    pub address: Option<Ipv4Addr>,
    /// The network name, for Wi-Fi.
    pub ssid: Option<String>,
    /// Whether the session with Home Assistant runs over this interface.
    pub carries_session: bool,
}

/// The network state that is in force, per interface the board has.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub wifi: Option<InterfaceState>,
    pub ethernet: Option<InterfaceState>,
}

impl InterfaceState {
    fn to_value(&self) -> Value {
        let mut fields = Map::new();
        fields.insert(
            "mode".to_owned(),
            json!(match self.mode {
                Mode::Dhcp => "dhcp",
                Mode::Static => "static",
            }),
        );
        fields.insert(
            "address".to_owned(),
            self.address
                .map_or(Value::Null, |address| json!(address.to_string())),
        );
        if let Some(ssid) = &self.ssid {
            fields.insert("ssid".to_owned(), json!(ssid));
        }
        fields.insert("carries_session".to_owned(), json!(self.carries_session));
        Value::Object(fields)
    }
}

impl State {
    fn to_value(&self) -> Value {
        let mut fields = Map::new();
        if let Some(wifi) = &self.wifi {
            fields.insert("wifi".to_owned(), wifi.to_value());
        }
        if let Some(ethernet) = &self.ethernet {
            fields.insert("ethernet".to_owned(), ethernet.to_value());
        }
        Value::Object(fields)
    }
}

/// The `network_result` message.
#[must_use]
pub fn result_message(operation_id: &str, revision: u64, outcome: Outcome, state: &State) -> Value {
    let mut message = json!({
        "type": "network_result",
        "operation_id": operation_id,
        "revision": revision,
        "state": state.to_value(),
    });
    match outcome {
        Outcome::Applied(proof) => {
            message["result"] = json!("applied");
            message["proof"] = json!(proof.wire());
        }
        Outcome::Reverted(reason) => {
            message["result"] = json!("reverted");
            message["reason"] = json!(reason.wire());
        }
    }
    message
}

/// What to do with an accepted message (plan 006 N5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// The values are the ones in use already: answer `applied` without a trial. This makes a
    /// message that Home Assistant sends again, for example after its own restart, harmless.
    AlreadyApplied,
    /// The revision was finished before, with other values: refuse it.
    Stale,
    /// Try the values.
    Try,
}

fn same_wifi(wanted: &WifiConfig, current: &WifiConfig) -> bool {
    wanted.ssid == current.ssid
        && wanted.password.as_deref().unwrap_or_default()
            == current.password.as_deref().unwrap_or_default()
        && wanted.ip == current.ip
}

fn same_ethernet(wanted: &EthernetConfig, current: &EthernetConfig) -> bool {
    wanted.ip == current.ip
}

/// Decides what to do with `request`, given the newest finished revision and the values in use.
/// An interface the request does not mention is left as it is, so it never makes a difference.
#[must_use]
pub fn plan(request: &Request, last_revision: u64, current: &NetworkConfig) -> Plan {
    let wifi_same = request.config.wifi.as_ref().is_none_or(|wanted| {
        current
            .wifi
            .as_ref()
            .is_some_and(|current| same_wifi(wanted, current))
    });
    let ethernet_same = request.config.ethernet.as_ref().is_none_or(|wanted| {
        current
            .ethernet
            .as_ref()
            .is_some_and(|current| same_ethernet(wanted, current))
    });
    if wifi_same && ethernet_same {
        Plan::AlreadyApplied
    } else if request.revision == last_revision {
        Plan::Stale
    } else {
        Plan::Try
    }
}

/// A result that waits for a session to be reported on.
///
/// The tablet writes it to storage before it tries new values and replaces it with the outcome,
/// so a reset or a power loss during the trial still ends with a `reverted` result once Home
/// Assistant is reachable again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub operation_id: String,
    pub revision: u64,
    pub outcome: Outcome,
}

impl Pending {
    /// The text kept in storage.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut value = json!({
            "operation_id": self.operation_id,
            "revision": self.revision,
        });
        match self.outcome {
            Outcome::Applied(proof) => {
                value["result"] = json!("applied");
                value["proof"] = json!(proof.wire());
            }
            Outcome::Reverted(reason) => {
                value["result"] = json!("reverted");
                value["reason"] = json!(reason.wire());
            }
        }
        value.to_string()
    }

    /// Reads what [`encode`](Self::encode) wrote; `None` for anything else.
    #[must_use]
    pub fn decode(text: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(text).ok()?;
        let fields = value.as_object()?;
        let operation_id = operation_id(fields.get("operation_id"))?.to_owned();
        let revision = revision(fields.get("revision"))?;
        let outcome = match fields.get("result")?.as_str()? {
            "applied" if fields.len() == 4 => {
                Outcome::Applied(Proof::from_wire(fields.get("proof")?.as_str()?)?)
            }
            "reverted" if fields.len() == 4 => {
                Outcome::Reverted(Reason::from_wire(fields.get("reason")?.as_str()?)?)
            }
            _ => return None,
        };
        Some(Self {
            operation_id,
            revision,
            outcome,
        })
    }
}
