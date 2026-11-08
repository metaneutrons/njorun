//! Pairing with the Njörun integration and the encrypted management session.
//!
//! Runs on the Home Assistant worker's WebSocket. Without a stored binding the tablet
//! announces itself (`njorun/announce`) and shows the comparison code until an administrator
//! approves it in Home Assistant; then it stores the binding and opens a Noise session. With a
//! binding it opens the session directly, on every connection with a fresh handshake.
//! Configuration events are decrypted here; applying them follows in #62.

use std::time::{Duration, Instant};

use njorun_app_core::identity::TabletIdentity;
use njorun_management::{
    announce_request, parse_announce, public_key, session_start_request, Announcement, Handshake,
    Session,
};
use serde_json::{json, Value};

use crate::{board, store};

const REQUEST_TIMEOUT_MS: u32 = 15_000;
const REPLY_BUFFER_BYTES: usize = 8 * 1024;
const ANNOUNCE_EVERY: Duration = Duration::from_secs(10);
const CONFLICT_RETRY: Duration = Duration::from_secs(60);
const SESSION_RETRY: Duration = Duration::from_secs(30);
/// How often to check whether the integration was installed meanwhile.
const INSTALL_CHECK: Duration = Duration::from_secs(60);

/// What the dashboard shows about the link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    /// The Njörun integration is not installed in Home Assistant.
    NotInstalled,
    /// Waiting for an administrator; the code to compare.
    Waiting(String),
    /// Another tablet key is paired under this tablet's ID.
    Conflict,
    /// Paired, but the session is not up (retrying).
    Disconnected,
    Connected,
}

enum State {
    NotInstalled,
    Waiting(String),
    Conflict,
    Disconnected,
    Active(Session),
}

pub(crate) struct Link {
    identity: TabletIdentity,
    public: [u8; 32],
    state: State,
    next_try: Instant,
    reported: Option<Status>,
}

enum CallError {
    /// Home Assistant answered with an error: its code and message.
    Refused(String, String),
    Transport(String),
}

impl CallError {
    fn describe(&self) -> String {
        match self {
            Self::Refused(code, message) => format!("{code}: {message}"),
            Self::Transport(reason) => reason.clone(),
        }
    }
}

/// Sends one request with a fresh id and returns that id and the `result`.
fn call(next_id: &mut u64, mut message: Value) -> Result<(u64, Value), CallError> {
    let id = *next_id;
    *next_id += 1;
    message["id"] = json!(id);
    let mut buffer = vec![0_u8; REPLY_BUFFER_BYTES];
    let length = board::ha_request(&message.to_string(), &mut buffer, REQUEST_TIMEOUT_MS)
        .map_err(|code| CallError::Transport(format!("request failed: {code}")))?;
    let reply: Value = serde_json::from_slice(&buffer[..length])
        .map_err(|_| CallError::Transport("unreadable reply".to_owned()))?;
    if reply["success"] == true {
        return Ok((id, reply["result"].clone()));
    }
    Err(CallError::Refused(
        reply["error"]["code"]
            .as_str()
            .unwrap_or("error")
            .to_owned(),
        reply["error"]["message"].as_str().unwrap_or("").to_owned(),
    ))
}

impl Link {
    /// Opens the link on a fresh HA connection; `None` without a tablet identity.
    pub(crate) fn open(next_id: &mut u64) -> Option<Self> {
        let identity = store::identity()?;
        let public = match public_key(identity.private_key()) {
            Ok(public) => public,
            Err(error) => {
                log::error!(target: "mgmt", "tablet public key unavailable: {error}");
                return None;
            }
        };
        let mut link = Self {
            identity,
            public,
            state: State::Disconnected,
            next_try: Instant::now(),
            reported: None,
        };
        link.connect(next_id);
        Some(link)
    }

    fn connect(&mut self, next_id: &mut u64) {
        if let Some((binding_id, ha_public)) = store::binding() {
            self.start_session(next_id, &binding_id, &ha_public);
        } else {
            self.announce(next_id);
        }
    }

    fn announce(&mut self, next_id: &mut u64) {
        let name = format!("Njörun D1001 {}", &self.identity.tablet_id()[7..13]);
        let capabilities = json!({"orientations": ["portrait", "landscape"], "brightness": true});
        let request = announce_request(
            self.identity.tablet_id(),
            &name,
            &self.public,
            &capabilities,
        );
        let result = match call(next_id, request) {
            Ok((_, result)) => result,
            Err(CallError::Refused(code, _)) if code == "unknown_command" => {
                if !matches!(self.state, State::NotInstalled) {
                    log::info!(target: "mgmt", "Njörun integration is not installed in Home Assistant");
                }
                self.state = State::NotInstalled;
                self.next_try = Instant::now() + INSTALL_CHECK;
                return;
            }
            Err(error) => {
                log::warn!(target: "mgmt", "announcement failed: {}", error.describe());
                self.retry_later(SESSION_RETRY);
                return;
            }
        };
        match parse_announce(&result, &self.public) {
            Ok(Announcement::Pending { code }) => {
                if !matches!(&self.state, State::Waiting(shown) if *shown == code) {
                    log::info!(target: "mgmt", "waiting for approval in Home Assistant, code {code}");
                }
                self.state = State::Waiting(code);
                self.next_try = Instant::now() + ANNOUNCE_EVERY;
            }
            Ok(Announcement::Approved {
                binding_id,
                ha_public,
            }) => {
                log::info!(target: "mgmt", "approved in Home Assistant as binding {binding_id}");
                store::save_binding(&binding_id, &ha_public);
                self.start_session(next_id, &binding_id, &ha_public);
            }
            Ok(Announcement::Conflict) => {
                log::warn!(target: "mgmt",
                    "another key is paired as {}; remove or replace that tablet in Home Assistant",
                    self.identity.tablet_id()
                );
                self.state = State::Conflict;
                self.next_try = Instant::now() + CONFLICT_RETRY;
            }
            Err(error) => {
                log::warn!(target: "mgmt", "unexpected announcement answer: {error}");
                self.retry_later(SESSION_RETRY);
            }
        }
    }

    fn start_session(&mut self, next_id: &mut u64, binding_id: &str, ha_public: &[u8; 32]) {
        match self.handshake(next_id, binding_id, ha_public) {
            Ok(session) => {
                log::info!(target: "mgmt", "management session open");
                self.state = State::Active(session);
            }
            Err(CallError::Refused(_, message)) if message.contains("Unknown or revoked") => {
                log::warn!(target: "mgmt", "Home Assistant no longer knows this binding; pairing again");
                store::forget_binding();
                self.announce(next_id);
            }
            Err(error) => {
                log::warn!(target: "mgmt", "management session failed: {}", error.describe());
                self.retry_later(SESSION_RETRY);
            }
        }
    }

    fn handshake(
        &self,
        next_id: &mut u64,
        binding_id: &str,
        ha_public: &[u8; 32],
    ) -> Result<Session, CallError> {
        let local = |error: njorun_management::Error| CallError::Transport(error.to_string());
        let (_, start) = call(next_id, session_start_request(binding_id))?;
        let (handshake, request) =
            Handshake::begin(&start, binding_id, self.identity.private_key(), ha_public)
                .map_err(local)?;
        let (_, response) = call(next_id, request)?;
        let (mut session, subscribe) = handshake.finish(&response).map_err(local)?;
        let (subscription_id, _) = call(next_id, subscribe)?;
        session.attach(subscription_id);
        Ok(session)
    }

    fn retry_later(&mut self, after: Duration) {
        if !matches!(self.state, State::Waiting(_) | State::Conflict) {
            self.state = State::Disconnected;
        }
        self.next_try = Instant::now() + after;
    }

    /// Re-announces while waiting, checks for a newly installed integration and retries a lost
    /// session. Cheap when nothing is due.
    pub(crate) fn tick(&mut self, next_id: &mut u64) {
        if matches!(self.state, State::Active(_)) || Instant::now() < self.next_try {
            return;
        }
        self.connect(next_id);
    }

    /// Takes an event of the management subscription; `false` for any other message.
    pub(crate) fn handle(&mut self, message: &[u8], id: Option<u64>) -> bool {
        let State::Active(session) = &mut self.state else {
            return false;
        };
        if id.is_none() || id != session.subscription_id() {
            return false;
        }
        let Ok(event) = serde_json::from_slice::<Value>(message) else {
            return true;
        };
        match session.decode(&event) {
            Ok(message) if message["type"] == "configuration" => {
                log::info!(target: "mgmt",
                    "configuration revision {} received; applying it arrives with #62",
                    message["revision"]
                );
            }
            Ok(message) => {
                log::info!(target: "mgmt", "management message {} ignored", message["type"]);
            }
            Err(error) => {
                log::warn!(target: "mgmt", "management session ended: {error}");
                self.retry_later(Duration::from_secs(5));
            }
        }
        true
    }

    /// The status, when it changed since the last call.
    pub(crate) fn changed_status(&mut self) -> Option<Status> {
        let status = match &self.state {
            State::NotInstalled => Status::NotInstalled,
            State::Waiting(code) => Status::Waiting(code.clone()),
            State::Conflict => Status::Conflict,
            State::Disconnected => Status::Disconnected,
            State::Active(_) => Status::Connected,
        };
        if self.reported.as_ref() == Some(&status) {
            return None;
        }
        self.reported = Some(status.clone());
        Some(status)
    }
}
