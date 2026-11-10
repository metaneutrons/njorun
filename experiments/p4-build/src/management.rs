//! Pairing with the Njörun integration and the encrypted management session.
//!
//! Runs on the Home Assistant worker's WebSocket. Without a stored binding the tablet
//! announces itself (`njorun/announce`) and shows the comparison code until an administrator
//! approves it in Home Assistant; then it stores the binding and opens a Noise session. With a
//! binding it opens the session directly, on every connection with a fresh handshake.
//! Configuration events are decrypted here; applying them follows in #62.

use std::time::{Duration, Instant};

use njorun_app_core::identity::TabletIdentity;
use njorun_app_core::live::{
    result_message, Backlight, Command, CommandGate, Reason, ResetReason, Telemetry, TelemetryPacer,
};
use njorun_app_core::settings::{accept_configuration, AppliedSettings, Configuration, Rejection};
use njorun_management::{
    announce_request, live_parts, parse_announce, public_key, session_start_request, Announcement,
    Handshake, Session,
};
use serde_json::{json, Value};

use crate::{board, store};

const REQUEST_TIMEOUT_MS: u32 = 15_000;
const REPLY_BUFFER_BYTES: usize = 8 * 1024;
const ANNOUNCE_EVERY: Duration = Duration::from_secs(10);
const CONFLICT_RETRY: Duration = Duration::from_secs(60);
const SESSION_RETRY: Duration = Duration::from_secs(30);
/// Retry while Home Assistant reloads the integration.
const RELOAD_RETRY: Duration = Duration::from_secs(3);
/// How often to check whether the integration was installed meanwhile.
const INSTALL_CHECK: Duration = Duration::from_secs(60);

/// How often the telemetry pacer is consulted.
const TELEMETRY_CHECK: Duration = Duration::from_secs(1);

/// Live commands and telemetry of one session (decision D6). It starts empty with every new
/// session, so no command or telemetry state survives a reconnect.
struct Live {
    offered: Vec<&'static str>,
    gate: CommandGate,
    pacer: TelemetryPacer,
    started: Instant,
    next_check: Instant,
    /// What the display shows now, as the UI last reported it.
    backlight: Backlight,
    reset_reason: ResetReason,
}

impl Live {
    fn new(offered: Vec<&'static str>, backlight: Backlight) -> Self {
        Self {
            offered,
            gate: CommandGate::new(),
            pacer: TelemetryPacer::new(),
            started: Instant::now(),
            next_check: Instant::now(),
            backlight,
            reset_reason: ResetReason::from_board(&board::reset_reason()),
        }
    }

    fn telemetry(&self) -> Telemetry {
        Telemetry {
            firmware_version: concat!(env!("CARGO_PKG_VERSION"), "-d1001").to_owned(),
            uptime_s: u32::try_from(board::uptime_ms() / 1000).unwrap_or(u32::MAX),
            rssi_dbm: board::wifi_rssi(),
            free_heap: board::free_heap(),
            reset_reason: self.reset_reason,
            backlight: self.backlight,
        }
    }
}

/// What this firmware can do. The tablet announces it when pairing and repeats it in every
/// session confirmation (decision D8), so a capability added by an update reaches Home
/// Assistant without pairing again.
fn capabilities() -> Value {
    json!({
        "orientations": ["portrait", "landscape", "portrait_flipped", "landscape_flipped"],
        "brightness": true,
        "live": ["wake", "backlight", "telemetry"],
    })
}

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
    Active(Session, Live),
}

pub(crate) struct Link {
    identity: TabletIdentity,
    public: [u8; 32],
    state: State,
    next_try: Instant,
    reported: Option<Status>,
    /// What the tablet last stored and applied from Home Assistant.
    applied: AppliedSettings,
    /// The operation whose acknowledgement waits for the UI to show the new settings.
    awaiting_ack: Option<(String, u64)>,
}

/// What an event of the management subscription means for the caller.
pub(crate) enum Handled {
    /// Not a management event: pass it on.
    NotMine,
    /// Consumed (decoded, ignored or the session ended).
    Consumed,
    /// A new configuration was accepted, stored and must now be applied by the UI; call
    /// `acknowledge` once it is.
    Settings(Box<AppliedSettings>),
    /// A live command that passed every check; the UI carries it out and the caller then
    /// reports the outcome with `command_done`.
    Live(Command),
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
            applied: store::applied_settings().unwrap_or_default(),
            awaiting_ack: None,
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
        let request = announce_request(
            self.identity.tablet_id(),
            &name,
            &self.public,
            &capabilities(),
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
            Ok((session, offered)) => {
                log::info!(target: "mgmt", "management session open, live parts: {offered:?}");
                let backlight = Backlight::from_setting(self.applied.settings.brightness);
                self.state = State::Active(session, Live::new(offered, backlight));
            }
            Err(CallError::Refused(_, message)) if message.contains("Unknown or revoked") => {
                log::warn!(target: "mgmt", "Home Assistant no longer knows this binding; pairing again");
                store::forget_binding();
                self.announce(next_id);
            }
            // The integration is reloading (for example after a tablet was removed): ask again
            // soon instead of waiting out the normal retry.
            Err(CallError::Refused(_, message)) if message.contains("not configured") => {
                log::info!(target: "mgmt", "the integration is reloading; trying again shortly");
                self.retry_later(RELOAD_RETRY);
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
    ) -> Result<(Session, Vec<&'static str>), CallError> {
        let local = |error: njorun_management::Error| CallError::Transport(error.to_string());
        let (_, start) = call(next_id, session_start_request(binding_id))?;
        let (handshake, request) =
            Handshake::begin(&start, binding_id, self.identity.private_key(), ha_public)
                .map_err(local)?;
        let (_, response) = call(next_id, request)?;
        let (mut session, subscribe) = handshake
            .finish(&response, Some(&capabilities()))
            .map_err(local)?;
        let (subscription_id, result) = call(next_id, subscribe)?;
        session.attach(subscription_id);
        // An integration without D6 offers no live part, so the tablet sends no live frames.
        let offered = live_parts(&result).map_err(local)?;
        Ok((session, offered))
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
        if matches!(self.state, State::Active(..)) || Instant::now() < self.next_try {
            return;
        }
        self.connect(next_id);
    }

    /// Takes an event of the management subscription.
    pub(crate) fn handle(&mut self, next_id: &mut u64, message: &[u8], id: Option<u64>) -> Handled {
        let State::Active(session, _) = &mut self.state else {
            return Handled::NotMine;
        };
        if id.is_none() || id != session.subscription_id() {
            return Handled::NotMine;
        }
        let Ok(event) = serde_json::from_slice::<Value>(message) else {
            return Handled::Consumed;
        };
        match session.decode(&event) {
            Ok(message) if message["type"] == "configuration" => self.configuration(&message),
            Ok(message) if message["type"] == "command" => self.command(next_id, &message),
            Ok(message) => {
                log::info!(target: "mgmt", "management message {} ignored", message["type"]);
                Handled::Consumed
            }
            Err(error) => {
                log::warn!(target: "mgmt", "management session ended: {error}");
                self.retry_later(Duration::from_secs(5));
                Handled::Consumed
            }
        }
    }

    fn configuration(&mut self, message: &Value) -> Handled {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let Configuration {
            operation_id,
            candidate,
            log_sink,
        } = match accept_configuration(&self.applied, message, now) {
            Ok(accepted) => accepted,
            Err(rejection) => {
                let why = match rejection {
                    Rejection::Malformed => "malformed",
                    Rejection::Expired => "expired",
                    Rejection::Stale => "older than the applied revision",
                    Rejection::Invalid => "contains values the tablet does not support",
                };
                log::warn!(target: "mgmt", "configuration revision {} refused: {why}", message["revision"]);
                return Handled::Consumed;
            }
        };
        if !store::save_applied_settings(&candidate) {
            // Not stored, so not acknowledged: HA keeps showing the last applied state.
            return Handled::Consumed;
        }
        if let Some(sink) = &log_sink {
            // Best effort and independent of the settings: a failed write only means the
            // next configuration carries it again.
            store::save_log_sink(sink);
            crate::logging::set_sink(Some(sink));
        }
        log::info!(target: "mgmt", "configuration revision {} stored", candidate.revision);
        self.awaiting_ack = Some((operation_id, candidate.revision));
        self.applied = candidate.clone();
        Handled::Settings(Box::new(candidate))
    }

    /// Reports the stored settings as applied, once the UI has shown them.
    pub(crate) fn acknowledge(&mut self, next_id: &mut u64, revision: u64) {
        let Some((operation_id, expected)) = self.awaiting_ack.take() else {
            return;
        };
        if expected != revision {
            self.awaiting_ack = Some((operation_id, expected));
            return;
        }
        let State::Active(session, _) = &mut self.state else {
            return;
        };
        let payload = json!({
            "type": "applied",
            "operation_id": operation_id,
            "revision": revision,
            "settings": self.applied.settings,
        });
        let result = session
            .frame_request(&payload)
            .map_err(|error| CallError::Transport(error.to_string()))
            .and_then(|request| call(next_id, request));
        match result {
            Ok(_) => {
                log::info!(target: "mgmt", "configuration revision {revision} applied and acknowledged")
            }
            Err(error) => {
                log::warn!(target: "mgmt", "acknowledging revision {revision} failed: {}", error.describe());
            }
        }
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs())
    }

    /// Checks a live command. A refused one is answered at once and nothing reaches the UI.
    fn command(&mut self, next_id: &mut u64, message: &Value) -> Handled {
        let State::Active(_, live) = &mut self.state else {
            return Handled::Consumed;
        };
        match live.gate.accept(message, Self::now_secs(), &live.offered) {
            Ok(command) => Handled::Live(command),
            Err(refusal) => {
                log::warn!(target: "mgmt", "live command refused: {:?}", refusal.reason);
                if let Some(result) = refusal.result() {
                    self.send_frame(next_id, &result);
                }
                Handled::Consumed
            }
        }
    }

    /// Reports the outcome of a live command that the UI carried out.
    pub(crate) fn command_done(&mut self, next_id: &mut u64, command_id: &str, ok: bool) {
        let outcome = if ok { Ok(()) } else { Err(Reason::Failed) };
        self.send_frame(next_id, &result_message(command_id, outcome));
    }

    /// Takes what the display shows now; the next telemetry carries it.
    pub(crate) fn set_backlight(&mut self, backlight: Backlight) {
        if let State::Active(_, live) = &mut self.state {
            live.backlight = backlight;
        }
    }

    /// Sends telemetry when it is due (on change and as a heartbeat, see the protocol).
    pub(crate) fn report(&mut self, next_id: &mut u64) {
        let State::Active(_, live) = &mut self.state else {
            return;
        };
        if !live.offered.contains(&"telemetry") || Instant::now() < live.next_check {
            return;
        }
        live.next_check = Instant::now() + TELEMETRY_CHECK;
        let telemetry = live.telemetry();
        let now_ms = u64::try_from(live.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if !live.pacer.due(now_ms, &telemetry) {
            return;
        }
        let Some(message) = telemetry.message() else {
            log::warn!(target: "mgmt", "telemetry out of range, not sent");
            return;
        };
        self.send_frame(next_id, &message);
    }

    /// Encrypts and sends one tablet message; a failure ends nothing, the next one retries.
    fn send_frame(&mut self, next_id: &mut u64, payload: &Value) {
        let State::Active(session, _) = &mut self.state else {
            return;
        };
        let result = session
            .frame_request(payload)
            .map_err(|error| CallError::Transport(error.to_string()))
            .and_then(|request| call(next_id, request));
        if let Err(error) = result {
            log::warn!(target: "mgmt", "sending {} failed: {}", payload["type"], error.describe());
        }
    }

    /// The status, when it changed since the last call.
    pub(crate) fn changed_status(&mut self) -> Option<Status> {
        let status = match &self.state {
            State::NotInstalled => Status::NotInstalled,
            State::Waiting(code) => Status::Waiting(code.clone()),
            State::Conflict => Status::Conflict,
            State::Disconnected => Status::Disconnected,
            State::Active(..) => Status::Connected,
        };
        if self.reported.as_ref() == Some(&status) {
            return None;
        }
        self.reported = Some(status.clone());
        Some(status)
    }
}
