//! Live commands and telemetry of the management session (decision D6).
//!
//! `command` messages from Home Assistant are transient: they are checked here, never stored and
//! never repeated after a reconnect. `command_result` and `telemetry` are what the tablet sends
//! back. The backlight state is a live value; it is not the persistent `brightness` setting.
//!
//! See `docs/protocols/002-ha-management.md`.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Largest `command_id`, as for operation IDs.
pub const MAX_COMMAND_ID_BYTES: usize = 128;
/// Command IDs a session remembers to reject a repetition.
pub const REMEMBERED_COMMANDS: usize = 16;
/// Largest encoded telemetry message.
pub const MAX_TELEMETRY_BYTES: usize = 512;
/// Longest `firmware_version`.
pub const MAX_FIRMWARE_VERSION_CHARS: usize = 32;
/// The heartbeat of telemetry in milliseconds.
pub const HEARTBEAT_MS: u64 = 60_000;
/// Fewest milliseconds between two telemetry messages.
pub const MIN_INTERVAL_MS: u64 = 5_000;
/// Signal change in dB that counts as a change.
pub const RSSI_STEP_DB: i16 = 6;

fn valid_command_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_COMMAND_ID_BYTES && !id.chars().any(char::is_control)
}

/// The live backlight: what the display shows now, from 0 to 100.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Backlight {
    pub on: bool,
    pub brightness: u8,
}

impl Backlight {
    /// The display as the persistent `brightness` setting gives it: on at that level.
    #[must_use]
    pub const fn from_setting(brightness: u8) -> Self {
        Self {
            on: true,
            brightness,
        }
    }

    const fn is_valid(self) -> bool {
        self.brightness <= 100
    }

    /// The state after `command`. An absent brightness keeps the current level.
    #[must_use]
    pub fn after(self, on: bool, brightness: Option<u8>) -> Self {
        Self {
            on,
            brightness: brightness.unwrap_or(self.brightness),
        }
    }
}

impl Action {
    /// The live part that must be offered for this action.
    #[must_use]
    pub const fn part(self) -> &'static str {
        match self {
            Self::Wake => "wake",
            Self::Backlight { .. } => "backlight",
        }
    }
}

/// What a valid command asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Leave idle or the screensaver as a touch would, without a touch.
    Wake,
    /// Switch the backlight and optionally set its level.
    Backlight { on: bool, brightness: Option<u8> },
}

/// A command that passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub command_id: String,
    pub action: Action,
}

/// Why a command was not carried out. These are the `reason` values of `command_result`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Expired,
    Duplicate,
    Unsupported,
    Invalid,
    Failed,
}

impl Reason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::Duplicate => "duplicate",
            Self::Unsupported => "unsupported",
            Self::Invalid => "invalid",
            Self::Failed => "failed",
        }
    }
}

/// A refused command. `command_id` is `None` when the message did not yield one: it is then
/// dropped without an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub command_id: Option<String>,
    pub reason: Reason,
}

impl Refusal {
    /// The `command_result` to send, if the command had an ID.
    #[must_use]
    pub fn result(&self) -> Option<Value> {
        self.command_id
            .as_deref()
            .map(|id| result_message(id, Err(self.reason)))
    }
}

/// The `command_result` message: `Ok` for a carried out command.
#[must_use]
pub fn result_message(command_id: &str, outcome: Result<(), Reason>) -> Value {
    json!({
        "type": "command_result",
        "command_id": command_id,
        "ok": outcome.is_ok(),
        "reason": outcome.err().map(Reason::as_str),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    #[serde(rename = "type")]
    kind: String,
    command_id: String,
    name: String,
    value: Value,
    expires_at: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBacklight {
    on: bool,
    brightness: Option<u8>,
}

fn parse_command(message: &Value) -> Result<(String, Action, u64), Refusal> {
    let id = message["command_id"]
        .as_str()
        .filter(|id| valid_command_id(id))
        .map(str::to_owned);
    let refuse = |reason| Refusal {
        command_id: id.clone(),
        reason,
    };
    if id.is_none() || message["type"] != "command" {
        return Err(refuse(Reason::Invalid));
    }
    let raw: RawCommand =
        serde_json::from_value(message.clone()).map_err(|_| refuse(Reason::Invalid))?;
    debug_assert_eq!(raw.kind, "command");
    let action = match raw.name.as_str() {
        "wake" if raw.value.is_null() => Action::Wake,
        "backlight" => {
            let value: RawBacklight =
                serde_json::from_value(raw.value).map_err(|_| refuse(Reason::Invalid))?;
            if value.brightness.is_some_and(|level| level > 100) {
                return Err(refuse(Reason::Invalid));
            }
            Action::Backlight {
                on: value.on,
                brightness: value.brightness,
            }
        }
        "wake" => return Err(refuse(Reason::Invalid)),
        _ => return Err(refuse(Reason::Unsupported)),
    };
    Ok((raw.command_id, action, raw.expires_at))
}

/// Per-session checks of incoming commands: shape, repetition and expiry.
///
/// A new session starts with a new gate, so a command is never carried out on another session
/// than the one it was sent on.
#[derive(Debug, Default)]
pub struct CommandGate {
    seen: VecDeque<String>,
}

impl CommandGate {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks a decrypted `command` message. `now` is the current Unix time in seconds and
    /// `offered` the live parts the session negotiated; a command for another part is
    /// `Unsupported`.
    ///
    /// The ID of every well-formed command is remembered, also when it was expired, so a
    /// repetition is `Duplicate` whatever the first answer was.
    ///
    /// # Errors
    /// A [`Refusal`] with the reason for the `command_result`, or without an ID when the
    /// message is to be dropped.
    pub fn accept(
        &mut self,
        message: &Value,
        now: u64,
        offered: &[&str],
    ) -> Result<Command, Refusal> {
        let (command_id, action, expires_at) = parse_command(message)?;
        if self.seen.contains(&command_id) {
            return Err(Refusal {
                command_id: Some(command_id),
                reason: Reason::Duplicate,
            });
        }
        if self.seen.len() == REMEMBERED_COMMANDS {
            self.seen.pop_front();
        }
        self.seen.push_back(command_id.clone());
        if !offered.contains(&action.part()) {
            return Err(Refusal {
                command_id: Some(command_id),
                reason: Reason::Unsupported,
            });
        }
        if expires_at <= now {
            return Err(Refusal {
                command_id: Some(command_id),
                reason: Reason::Expired,
            });
        }
        Ok(Command { command_id, action })
    }
}

/// Why the tablet last restarted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetReason {
    PowerOn,
    Software,
    Panic,
    Watchdog,
    Brownout,
    DeepSleep,
    External,
    Unknown,
}

impl ResetReason {
    /// Maps the reason text of the D1001 board (`njorun_d1001_reset_reason`) to the closed set
    /// of the protocol.
    #[must_use]
    pub fn from_board(reason: &str) -> Self {
        match reason {
            "power-on" => Self::PowerOn,
            "software" => Self::Software,
            "panic" | "CPU lockup" => Self::Panic,
            "interrupt watchdog" | "task watchdog" | "other watchdog" => Self::Watchdog,
            "brownout" | "power glitch" => Self::Brownout,
            "deep sleep" => Self::DeepSleep,
            "external" => Self::External,
            _ => Self::Unknown,
        }
    }
}

/// The tablet's health, without secrets, addresses or log text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Telemetry {
    pub firmware_version: String,
    pub uptime_s: u32,
    /// Wi-Fi signal in dBm, `None` without a Wi-Fi link.
    pub rssi_dbm: Option<i16>,
    pub free_heap: u32,
    pub reset_reason: ResetReason,
    pub backlight: Backlight,
}

impl Telemetry {
    fn is_valid(&self) -> bool {
        let version = self.firmware_version.chars().count();
        (1..=MAX_FIRMWARE_VERSION_CHARS).contains(&version)
            && !self.firmware_version.chars().any(char::is_control)
            && self.rssi_dbm.is_none_or(|dbm| (-127..=0).contains(&dbm))
            && self.backlight.is_valid()
    }

    /// Reads a decrypted `telemetry` message.
    ///
    /// # Errors
    /// `None` for anything but exactly the specified fields and ranges.
    #[must_use]
    pub fn parse(message: &Value) -> Option<Self> {
        if message["type"] != "telemetry" || message.to_string().len() > MAX_TELEMETRY_BYTES {
            return None;
        }
        let mut body = message.as_object()?.clone();
        body.remove("type");
        let telemetry: Self = serde_json::from_value(Value::Object(body)).ok()?;
        telemetry.is_valid().then_some(telemetry)
    }

    /// The `telemetry` message to encrypt, or `None` when a field is out of range or the
    /// message would exceed its size bound.
    #[must_use]
    pub fn message(&self) -> Option<Value> {
        if !self.is_valid() {
            return None;
        }
        let mut message = serde_json::to_value(self).ok()?;
        message
            .as_object_mut()?
            .insert("type".into(), "telemetry".into());
        (message.to_string().len() <= MAX_TELEMETRY_BYTES).then_some(message)
    }
}

/// Decides when the tablet sends telemetry.
///
/// It sends right after the session opens, when the backlight, the firmware version or the reset
/// reason changes or the signal moves by [`RSSI_STEP_DB`] or more, and otherwise every
/// [`HEARTBEAT_MS`], never twice within [`MIN_INTERVAL_MS`].
#[derive(Debug, Default)]
pub struct TelemetryPacer {
    last: Option<(u64, Telemetry)>,
}

impl TelemetryPacer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether to send `current` at `now_ms` (a monotonic clock). When it returns `true`
    /// the caller sends and the message counts as sent.
    pub fn due(&mut self, now_ms: u64, current: &Telemetry) -> bool {
        let due = match &self.last {
            None => true,
            Some((sent_at, previous)) => {
                let elapsed = now_ms.saturating_sub(*sent_at);
                let changed = previous.backlight != current.backlight
                    || previous.firmware_version != current.firmware_version
                    || previous.reset_reason != current.reset_reason
                    || match (previous.rssi_dbm, current.rssi_dbm) {
                        (Some(old), Some(new)) => (old - new).abs() >= RSSI_STEP_DB,
                        (None, None) => false,
                        _ => true,
                    };
                elapsed >= HEARTBEAT_MS || (changed && elapsed >= MIN_INTERVAL_MS)
            }
        };
        if due {
            self.last = Some((now_ms, current.clone()));
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        Action, Backlight, Command, CommandGate, HEARTBEAT_MS, MIN_INTERVAL_MS, Reason,
        ResetReason, Telemetry, TelemetryPacer, result_message,
    };

    const NOW: u64 = 1_000_000_000;
    const OFFERED: [&str; 2] = ["wake", "backlight"];

    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../protocol/fixtures/live-messages.json"
        ))
        .expect("fixture")
    }

    fn cases(fixture: &Value, kind: &str, group: &str) -> Vec<Value> {
        fixture[kind][group].as_array().expect("cases").clone()
    }

    fn command(id: &str, expires_at: u64) -> Value {
        json!({"type": "command", "command_id": id, "name": "wake", "value": null,
               "expires_at": expires_at})
    }

    #[test]
    fn fixture_commands_are_accepted_and_refused() {
        let fixture = fixture();
        for message in cases(&fixture, "command", "valid") {
            let accepted = CommandGate::new().accept(&message, NOW, &OFFERED);
            assert!(accepted.is_ok(), "{message}");
        }
        for message in cases(&fixture, "command", "invalid") {
            let refused = CommandGate::new().accept(&message, NOW, &OFFERED);
            assert!(refused.is_err(), "{message}");
        }
    }

    #[test]
    fn fixture_limits_match_the_constants() {
        let limits = &fixture()["limits"];
        assert_eq!(limits["command_id_bytes"], super::MAX_COMMAND_ID_BYTES);
        assert_eq!(
            limits["firmware_version_chars"],
            super::MAX_FIRMWARE_VERSION_CHARS
        );
        assert_eq!(limits["telemetry_json_bytes"], super::MAX_TELEMETRY_BYTES);
    }

    #[test]
    fn commands_read_their_action() {
        let mut gate = CommandGate::new();
        let backlight = json!({"type": "command", "command_id": "b", "name": "backlight",
            "value": {"on": true, "brightness": 40}, "expires_at": NOW + 10});
        assert_eq!(
            gate.accept(&backlight, NOW, &OFFERED),
            Ok(Command {
                command_id: "b".into(),
                action: Action::Backlight {
                    on: true,
                    brightness: Some(40)
                }
            })
        );
        assert_eq!(
            gate.accept(&command("w", NOW + 10), NOW, &OFFERED)
                .map(|c| c.action),
            Ok(Action::Wake)
        );
    }

    #[test]
    fn a_repeated_id_is_a_duplicate_even_after_expiry() {
        let mut gate = CommandGate::new();
        assert!(gate.accept(&command("a", NOW + 10), NOW, &OFFERED).is_ok());
        let again = gate
            .accept(&command("a", NOW + 10), NOW, &OFFERED)
            .unwrap_err();
        assert_eq!(again.reason, Reason::Duplicate);
        assert_eq!(
            gate.accept(&command("old", NOW), NOW, &OFFERED)
                .unwrap_err()
                .reason,
            Reason::Expired
        );
        assert_eq!(
            gate.accept(&command("old", NOW + 10), NOW, &OFFERED)
                .unwrap_err()
                .reason,
            Reason::Duplicate
        );
    }

    #[test]
    fn only_the_last_sixteen_ids_are_remembered() {
        let mut gate = CommandGate::new();
        for index in 0..17 {
            assert!(
                gate.accept(&command(&format!("c{index}"), NOW + 10), NOW, &OFFERED)
                    .is_ok()
            );
        }
        assert!(gate.accept(&command("c0", NOW + 10), NOW, &OFFERED).is_ok());
        assert_eq!(
            gate.accept(&command("c16", NOW + 10), NOW, &OFFERED)
                .unwrap_err()
                .reason,
            Reason::Duplicate
        );
    }

    #[test]
    fn a_part_that_was_not_offered_is_unsupported() {
        let mut gate = CommandGate::new();
        let refusal = gate
            .accept(&command("w", NOW + 10), NOW, &["backlight"])
            .unwrap_err();
        assert_eq!(refusal.reason, Reason::Unsupported);
        assert_eq!(
            gate.accept(&command("n", NOW + 10), NOW, &[])
                .unwrap_err()
                .reason,
            Reason::Unsupported,
            "an integration without D6 offers nothing"
        );
    }

    #[test]
    fn refusals_answer_only_with_an_id() {
        let unknown = json!({"type": "command", "command_id": "u", "name": "reboot",
            "value": null, "expires_at": NOW + 10});
        let refusal = CommandGate::new()
            .accept(&unknown, NOW, &OFFERED)
            .unwrap_err();
        assert_eq!(refusal.reason, Reason::Unsupported);
        assert_eq!(
            refusal.result(),
            Some(
                json!({"type": "command_result", "command_id": "u", "ok": false,
                        "reason": "unsupported"})
            )
        );
        let no_id = json!({"type": "command", "name": "wake", "value": null});
        let dropped = CommandGate::new()
            .accept(&no_id, NOW, &OFFERED)
            .unwrap_err();
        assert_eq!(dropped.result(), None);
        let bad_value = json!({"type": "command", "command_id": "v", "name": "backlight",
            "value": {"on": true, "brightness": 101}, "expires_at": NOW + 10});
        assert_eq!(
            CommandGate::new()
                .accept(&bad_value, NOW, &OFFERED)
                .unwrap_err()
                .reason,
            Reason::Invalid
        );
    }

    #[test]
    fn results_match_the_fixture_shape() {
        let fixture = fixture();
        assert_eq!(
            result_message("c1", Ok(())),
            cases(&fixture, "command_result", "valid")[0]
        );
        for (index, reason) in [
            Reason::Expired,
            Reason::Duplicate,
            Reason::Unsupported,
            Reason::Invalid,
            Reason::Failed,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                result_message("c1", Err(reason)),
                cases(&fixture, "command_result", "valid")[index + 1]
            );
        }
        let names: Vec<_> = fixture["command_reasons"]
            .as_array()
            .expect("reasons")
            .iter()
            .map(|reason| reason.as_str().expect("text"))
            .collect();
        assert_eq!(
            names,
            ["expired", "duplicate", "unsupported", "invalid", "failed"]
        );
    }

    #[test]
    fn fixture_telemetry_is_accepted_and_refused() {
        let fixture = fixture();
        for message in cases(&fixture, "telemetry", "valid") {
            let parsed = Telemetry::parse(&message).unwrap_or_else(|| panic!("{message}"));
            assert_eq!(parsed.message(), Some(message), "round trip");
        }
        for message in cases(&fixture, "telemetry", "invalid") {
            assert_eq!(Telemetry::parse(&message), None, "{message}");
        }
    }

    #[test]
    fn reset_reasons_match_the_fixture() {
        let all = [
            ResetReason::PowerOn,
            ResetReason::Software,
            ResetReason::Panic,
            ResetReason::Watchdog,
            ResetReason::Brownout,
            ResetReason::DeepSleep,
            ResetReason::External,
            ResetReason::Unknown,
        ];
        let names: Vec<Value> = all
            .iter()
            .map(|reason| serde_json::to_value(reason).expect("serialize"))
            .collect();
        assert_eq!(Value::Array(names), fixture()["reset_reasons"]);
    }

    fn sample() -> Telemetry {
        Telemetry {
            firmware_version: "0.1.0".into(),
            uptime_s: 1,
            rssi_dbm: Some(-60),
            free_heap: 60_000,
            reset_reason: ResetReason::PowerOn,
            backlight: Backlight::from_setting(80),
        }
    }

    #[test]
    fn board_reset_reasons_map_into_the_protocol_set() {
        for (board, expected) in [
            ("power-on", ResetReason::PowerOn),
            ("software", ResetReason::Software),
            ("panic", ResetReason::Panic),
            ("CPU lockup", ResetReason::Panic),
            ("interrupt watchdog", ResetReason::Watchdog),
            ("task watchdog", ResetReason::Watchdog),
            ("other watchdog", ResetReason::Watchdog),
            ("brownout", ResetReason::Brownout),
            ("power glitch", ResetReason::Brownout),
            ("deep sleep", ResetReason::DeepSleep),
            ("external", ResetReason::External),
            ("USB", ResetReason::Unknown),
            ("", ResetReason::Unknown),
        ] {
            assert_eq!(ResetReason::from_board(board), expected, "{board}");
        }
    }

    #[test]
    fn telemetry_outside_its_ranges_is_not_built() {
        let mut telemetry = sample();
        telemetry.rssi_dbm = Some(5);
        assert_eq!(telemetry.message(), None);
        telemetry = sample();
        telemetry.firmware_version = "x".repeat(33);
        assert_eq!(telemetry.message(), None);
        telemetry = sample();
        telemetry.backlight.brightness = 101;
        assert_eq!(telemetry.message(), None);
        assert!(sample().message().is_some());
    }

    #[test]
    fn the_backlight_command_changes_the_live_state_only() {
        let from_setting = Backlight::from_setting(80);
        assert_eq!(
            from_setting.after(false, None),
            Backlight {
                on: false,
                brightness: 80
            }
        );
        assert_eq!(
            from_setting.after(true, Some(30)),
            Backlight {
                on: true,
                brightness: 30
            }
        );
    }

    #[test]
    fn telemetry_is_paced() {
        let mut pacer = TelemetryPacer::new();
        let mut telemetry = sample();
        assert!(pacer.due(0, &telemetry), "right after the session opens");
        assert!(!pacer.due(1_000, &telemetry));
        telemetry.backlight.on = false;
        assert!(
            !pacer.due(MIN_INTERVAL_MS - 1, &telemetry),
            "not twice within 5 s"
        );
        assert!(pacer.due(MIN_INTERVAL_MS, &telemetry), "a backlight change");
        telemetry.rssi_dbm = Some(-64);
        assert!(
            !pacer.due(MIN_INTERVAL_MS * 3, &telemetry),
            "a small signal change"
        );
        telemetry.rssi_dbm = Some(-66);
        assert!(
            pacer.due(MIN_INTERVAL_MS * 3, &telemetry),
            "a change of 6 dB"
        );
        telemetry.rssi_dbm = None;
        assert!(
            pacer.due(MIN_INTERVAL_MS * 5, &telemetry),
            "the link is gone"
        );
        assert!(!pacer.due(MIN_INTERVAL_MS * 5 + HEARTBEAT_MS - 1, &telemetry));
        assert!(
            pacer.due(MIN_INTERVAL_MS * 5 + HEARTBEAT_MS, &telemetry),
            "the heartbeat"
        );
    }
}
