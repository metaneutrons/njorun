//! Firmware install protocol of the management session (decision D7), host side.
//!
//! HA sends an `install` message for an image it has already downloaded, verified and cached.
//! The tablet decides here whether to accept it, answers with `install_result`, reports
//! `install_progress` while it pulls and writes the image and, after the reboot, one final
//! `install_outcome`. Nothing here downloads or writes an image; the firmware does that after
//! [`InstallGate::accept`] returned an [`Install`]. Like `command`, an `install` is transient:
//! never stored and never repeated after a reconnect.
//!
//! See `docs/protocols/002-ha-management.md`.

use std::collections::VecDeque;

use serde::Deserialize;
use serde_json::{Value, json};

/// Largest `install_id`, as for operation and command IDs.
pub const MAX_INSTALL_ID_BYTES: usize = 128;
/// Install IDs a session remembers to reject a repetition.
pub const REMEMBERED_INSTALLS: usize = 16;
/// Longest `version`.
pub const MAX_VERSION_CHARS: usize = 32;
/// Longest download `url`.
pub const MAX_URL_CHARS: usize = 512;
/// The smallest OTA slot this protocol was specified for (8 MiB). The tablet checks the size
/// against the slot it reads from its partition table.
pub const SLOT_BYTES: u32 = 8 * 1024 * 1024;
/// Fewest milliseconds between two progress messages of the same stage.
pub const PROGRESS_MIN_INTERVAL_MS: u64 = 1_000;

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_INSTALL_ID_BYTES && !id.chars().any(char::is_control)
}

fn plain(text: &str, limit: usize) -> bool {
    let chars = text.chars().count();
    (1..=limit).contains(&chars) && !text.chars().any(char::is_control)
}

/// The firmware channel of an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Stable,
    Beta,
}

/// Why an install was not accepted. These are the `reason` values of a rejected `install_result`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    Expired,
    Duplicate,
    Busy,
    Incompatible,
    TooLarge,
    Malformed,
}

impl Reject {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::Duplicate => "duplicate",
            Self::Busy => "busy",
            Self::Incompatible => "incompatible",
            Self::TooLarge => "too_large",
            Self::Malformed => "malformed",
        }
    }
}

/// An install that passed every check the tablet can do before downloading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Install {
    pub install_id: String,
    pub version: String,
    pub channel: Channel,
    pub size: u32,
    /// Lowercase hex SHA-256 of the application image.
    pub sha256: String,
    pub url: String,
    /// Single-use bearer token for the download. Never log it.
    pub token: String,
    pub expires_at: u64,
    /// HA allows a version below the running one for this install (a deliberate downgrade).
    pub downgrade: bool,
}

/// A refused install. `install_id` is `None` when the message did not yield one: it is then
/// dropped without an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub install_id: Option<String>,
    pub reason: Reject,
}

impl Refusal {
    /// The `install_result` to send, if the install had an ID.
    #[must_use]
    pub fn result(&self) -> Option<Value> {
        self.install_id
            .as_deref()
            .map(|id| result_message(id, Err(self.reason)))
    }
}

/// The `install_result` message: `Ok` for an accepted install.
#[must_use]
pub fn result_message(install_id: &str, outcome: Result<(), Reject>) -> Value {
    json!({
        "type": "install_result",
        "install_id": install_id,
        "state": if outcome.is_ok() { "accepted" } else { "rejected" },
        "reason": outcome.err().map(Reject::as_str),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(rename = "type")]
    kind: String,
    install_id: String,
    version: String,
    channel: Channel,
    size: u32,
    sha256: String,
    url: String,
    token: String,
    expires_at: u64,
    /// Optional; absent means `false`.
    #[serde(default)]
    downgrade: bool,
}

fn valid_url(url: &str) -> bool {
    if !plain(url, MAX_URL_CHARS) || url.chars().any(char::is_whitespace) || url.contains('#') {
        return false;
    }
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    !authority.is_empty() && !authority.contains(['?', '@'])
}

fn valid_token(token: &str) -> bool {
    (32..=128).contains(&token.len())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn valid_sha256(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Reads an `install` message: exactly the specified fields, in range.
///
/// # Errors
/// A [`Refusal`] (`Malformed`), without an ID when the message did not yield one.
pub fn parse(message: &Value) -> Result<Install, Refusal> {
    let id = message["install_id"]
        .as_str()
        .filter(|id| valid_id(id))
        .map(str::to_owned);
    let malformed = || Refusal {
        install_id: id.clone(),
        reason: Reject::Malformed,
    };
    if id.is_none() || message["type"] != "install" {
        return Err(malformed());
    }
    let raw: Raw = serde_json::from_value(message.clone()).map_err(|_| malformed())?;
    debug_assert_eq!(raw.kind, "install");
    if !plain(&raw.version, MAX_VERSION_CHARS)
        || raw.size == 0
        || !valid_sha256(&raw.sha256)
        || !valid_url(&raw.url)
        || !valid_token(&raw.token)
    {
        return Err(malformed());
    }
    Ok(Install {
        install_id: raw.install_id,
        version: raw.version,
        channel: raw.channel,
        size: raw.size,
        sha256: raw.sha256,
        url: raw.url,
        token: raw.token,
        expires_at: raw.expires_at,
        downgrade: raw.downgrade,
    })
}

/// `major.minor.patch` at the start of a version, ignoring a leading `v` and any
/// pre-release or build suffix.
#[must_use]
pub fn core_version(version: &str) -> Option<(u64, u64, u64)> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let end = version.find(['-', '+']).unwrap_or(version.len());
    let mut parts = version[..end].split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let triple = (next()?, next()?, next()?);
    parts.next().is_none().then_some(triple)
}

/// What the tablet knows when it checks an install.
#[derive(Clone, Copy, Debug)]
pub struct Context<'a> {
    /// Current Unix time in seconds.
    pub now: u64,
    /// Whether the session negotiated `ota`. Without it every install is `Incompatible`.
    pub offered: bool,
    /// Origin of the HA connection (`https://host:port`). The download must come from it, so a
    /// token never goes to another host.
    pub ha_origin: &'a str,
    /// The lowest version this firmware may be replaced with (anti-rollback floor), if any.
    /// A downgrade flag does not lower it.
    pub floor: Option<(u64, u64, u64)>,
    /// The version that runs now. A lower one is refused unless the install carries the
    /// downgrade flag.
    pub running: Option<(u64, u64, u64)>,
    /// Size of the inactive OTA slot in bytes, as read from the partition table.
    pub slot_bytes: u32,
}

/// Per-session checks of incoming installs and the single running install.
///
/// A new session starts with a new gate. An install that was accepted survives a reconnect in
/// the firmware (it persists the ID to report the outcome after the reboot), not here.
#[derive(Debug, Default)]
pub struct InstallGate {
    seen: VecDeque<String>,
    active: Option<String>,
}

impl InstallGate {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The ID of the install in progress, if any.
    #[must_use]
    pub fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// Checks a decrypted `install` message in this order: shape, repetition, expiry, another
    /// install in progress, compatibility, slot size. The ID of every well-formed install is
    /// remembered, so a repetition is `Duplicate` whatever the first answer was and HA retries
    /// with a fresh ID. On success the install becomes the active one until [`Self::finish`].
    ///
    /// # Errors
    /// A [`Refusal`] with the reason for the `install_result`, or without an ID when the
    /// message is to be dropped.
    pub fn accept(&mut self, message: &Value, context: &Context<'_>) -> Result<Install, Refusal> {
        let install = parse(message)?;
        let refuse = |reason| Refusal {
            install_id: Some(install.install_id.clone()),
            reason,
        };
        if self.seen.contains(&install.install_id) {
            return Err(refuse(Reject::Duplicate));
        }
        if self.seen.len() == REMEMBERED_INSTALLS {
            self.seen.pop_front();
        }
        self.seen.push_back(install.install_id.clone());
        if install.expires_at <= context.now {
            return Err(refuse(Reject::Expired));
        }
        if self.active.is_some() {
            return Err(refuse(Reject::Busy));
        }
        let same_origin = install
            .url
            .strip_prefix(context.ha_origin)
            .is_some_and(|rest| rest.starts_with('/'));
        if !same_origin {
            return Err(refuse(Reject::Malformed));
        }
        let version = core_version(&install.version);
        if !context.offered
            || version.is_none()
            || context.floor.is_some_and(|floor| version < Some(floor))
            || (!install.downgrade
                && context
                    .running
                    .is_some_and(|running| version < Some(running)))
        {
            return Err(refuse(Reject::Incompatible));
        }
        if install.size > context.slot_bytes {
            return Err(refuse(Reject::TooLarge));
        }
        self.active = Some(install.install_id.clone());
        Ok(install)
    }

    /// Ends the active install (failed, or about to reboot). A new install is accepted again.
    pub fn finish(&mut self) {
        self.active = None;
    }
}

/// The stage of an install. The order is the order of the flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Download,
    Verify,
    Write,
    Reboot,
}

impl Stage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Verify => "verify",
            Self::Write => "write",
            Self::Reboot => "reboot",
        }
    }
}

/// The `install_progress` message, or `None` when `total` is 0 or `done` exceeds it.
#[must_use]
pub fn progress_message(install_id: &str, stage: Stage, done: u32, total: u32) -> Option<Value> {
    (valid_id(install_id) && total > 0 && done <= total).then(|| {
        json!({
            "type": "install_progress",
            "install_id": install_id,
            "stage": stage.as_str(),
            "done": done,
            "total": total,
        })
    })
}

/// Decides when progress is sent: a new stage and the end of a stage always, otherwise at
/// most once per [`PROGRESS_MIN_INTERVAL_MS`].
#[derive(Debug, Default)]
pub struct ProgressPacer {
    last: Option<(u64, Stage)>,
}

impl ProgressPacer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether to send progress at `now_ms` (a monotonic clock). When it returns `true` the
    /// caller sends and the message counts as sent.
    pub fn due(&mut self, now_ms: u64, stage: Stage, done: u32, total: u32) -> bool {
        let due = match self.last {
            None => true,
            Some((sent_at, last_stage)) => {
                stage != last_stage
                    || done == total
                    || now_ms.saturating_sub(sent_at) >= PROGRESS_MIN_INTERVAL_MS
            }
        };
        if due {
            self.last = Some((now_ms, stage));
        }
        due
    }
}

/// How an install ended, as reconciled after the reboot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The new image runs and the boot was confirmed.
    Installed,
    /// The bootloader returned to the previous image.
    RolledBack,
}

impl Outcome {
    /// What the tablet reports after the reboot: it installed when the image that runs is the
    /// requested one and its boot was confirmed, and rolled back otherwise.
    #[must_use]
    pub fn reconcile(requested: &str, running: &str, boot_confirmed: bool) -> Self {
        if running == requested && boot_confirmed {
            Self::Installed
        } else {
            Self::RolledBack
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::RolledBack => "rolled_back",
        }
    }
}

/// The final `install_outcome` message with the version that runs now, or `None` when the
/// version is not a valid one.
#[must_use]
pub fn outcome_message(install_id: &str, outcome: Outcome, version: &str) -> Option<Value> {
    (valid_id(install_id) && plain(version, MAX_VERSION_CHARS)).then(|| {
        json!({
            "type": "install_outcome",
            "install_id": install_id,
            "state": outcome.as_str(),
            "version": version,
        })
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        Channel, Context, InstallGate, Outcome, ProgressPacer, Reject, SLOT_BYTES, Stage,
        core_version, outcome_message, parse, progress_message, result_message,
    };

    const NOW: u64 = 1_000_000_000;
    const ORIGIN: &str = "https://ha.local:8123";

    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../protocol/fixtures/install-messages.json"
        ))
        .expect("fixture")
    }

    fn cases(fixture: &Value, kind: &str, group: &str) -> Vec<Value> {
        fixture[kind][group].as_array().expect("cases").clone()
    }

    fn context() -> Context<'static> {
        Context {
            now: NOW,
            offered: true,
            ha_origin: ORIGIN,
            floor: None,
            running: None,
            slot_bytes: SLOT_BYTES,
        }
    }

    fn install(id: &str) -> Value {
        cases(&fixture(), "install", "valid")[0]
            .as_object()
            .map(|base| {
                let mut message = base.clone();
                message.insert("install_id".into(), id.into());
                Value::Object(message)
            })
            .expect("object")
    }

    fn with(id: &str, key: &str, value: Value) -> Value {
        let mut message = install(id);
        message[key] = value;
        message
    }

    #[test]
    fn fixture_installs_parse_or_are_malformed() {
        let fixture = fixture();
        for message in cases(&fixture, "install", "valid") {
            assert!(parse(&message).is_ok(), "{message}");
        }
        for message in cases(&fixture, "install", "invalid") {
            let refusal = parse(&message).expect_err(&message.to_string());
            assert_eq!(refusal.reason, Reject::Malformed, "{message}");
        }
    }

    #[test]
    fn fixture_constants_match() {
        let fixture = fixture();
        assert_eq!(fixture["ha_origin"], ORIGIN);
        let limits = &fixture["limits"];
        assert_eq!(limits["slot_bytes"], SLOT_BYTES);
        assert_eq!(limits["install_id_bytes"], super::MAX_INSTALL_ID_BYTES);
        assert_eq!(limits["version_chars"], super::MAX_VERSION_CHARS);
        assert_eq!(limits["url_chars"], super::MAX_URL_CHARS);
        assert_eq!(
            limits["progress_min_interval_ms"],
            super::PROGRESS_MIN_INTERVAL_MS
        );
        let reasons: Vec<_> = [
            Reject::Expired,
            Reject::Duplicate,
            Reject::Busy,
            Reject::Incompatible,
            Reject::TooLarge,
            Reject::Malformed,
        ]
        .iter()
        .map(|reason| json!(reason.as_str()))
        .collect();
        assert_eq!(Value::Array(reasons), fixture["reject_reasons"]);
        let stages: Vec<_> = [Stage::Download, Stage::Verify, Stage::Write, Stage::Reboot]
            .iter()
            .map(|stage| json!(stage.as_str()))
            .collect();
        assert_eq!(Value::Array(stages), fixture["stages"]);
    }

    #[test]
    fn an_install_that_yields_no_id_is_dropped() {
        for message in [
            json!({"type": "install"}),
            json!({"type": "install", "install_id": ""}),
            json!({"type": "install", "install_id": 7}),
            with("a\u{7}b", "size", json!(1)),
        ] {
            let refusal = parse(&message).expect_err("refused");
            assert_eq!(refusal.install_id, None);
            assert_eq!(refusal.result(), None);
        }
        let refusal = parse(&with("i9", "channel", json!("nightly"))).expect_err("refused");
        assert_eq!(
            refusal.result(),
            Some(result_message("i9", Err(Reject::Malformed)))
        );
    }

    #[test]
    fn a_valid_install_is_accepted_and_becomes_active() {
        let mut gate = InstallGate::new();
        let accepted = gate.accept(&install("i1"), &context()).expect("accepted");
        assert_eq!(accepted.install_id, "i1");
        assert_eq!(accepted.channel, Channel::Stable);
        assert_eq!(gate.active(), Some("i1"));
    }

    #[test]
    fn a_repeated_id_is_a_duplicate_even_after_a_rejection() {
        let mut gate = InstallGate::new();
        let expired = with("i1", "expires_at", json!(NOW));
        assert_eq!(
            gate.accept(&expired, &context())
                .expect_err("expired")
                .reason,
            Reject::Expired
        );
        assert_eq!(
            gate.accept(&install("i1"), &context())
                .expect_err("dup")
                .reason,
            Reject::Duplicate
        );
        assert!(gate.accept(&install("i2"), &context()).is_ok());
    }

    #[test]
    fn only_one_install_runs_at_a_time() {
        let mut gate = InstallGate::new();
        gate.accept(&install("i1"), &context()).expect("first");
        assert_eq!(
            gate.accept(&install("i2"), &context())
                .expect_err("busy")
                .reason,
            Reject::Busy
        );
        gate.finish();
        assert_eq!(gate.active(), None);
        assert!(gate.accept(&install("i3"), &context()).is_ok());
    }

    #[test]
    fn only_the_last_sixteen_ids_are_remembered() {
        let mut gate = InstallGate::new();
        for index in 0..17 {
            let id = format!("i{index}");
            gate.accept(&install(&id), &context()).expect("accepted");
            gate.finish();
        }
        assert!(gate.accept(&install("i0"), &context()).is_ok(), "forgotten");
        gate.finish();
        assert_eq!(
            gate.accept(&install("i16"), &context())
                .expect_err("dup")
                .reason,
            Reject::Duplicate
        );
    }

    #[test]
    fn checks_run_in_the_documented_order() {
        let mut gate = InstallGate::new();
        gate.accept(&install("i1"), &context()).expect("first");
        // Expired wins over busy, busy over the origin, incompatible and size checks.
        let all_wrong = with("i2", "expires_at", json!(NOW - 1));
        assert_eq!(
            gate.accept(&all_wrong, &context()).expect_err("x").reason,
            Reject::Expired
        );
        let mut message = with("i3", "size", json!(SLOT_BYTES + 1));
        message["version"] = json!("x");
        assert_eq!(
            gate.accept(&message, &context()).expect_err("x").reason,
            Reject::Busy
        );
        gate.finish();
        let mut message = with("i4", "size", json!(SLOT_BYTES + 1));
        message["version"] = json!("x");
        assert_eq!(
            gate.accept(&message, &context()).expect_err("x").reason,
            Reject::Incompatible
        );
    }

    #[test]
    fn the_slot_size_is_inclusive() {
        let mut gate = InstallGate::new();
        assert!(
            gate.accept(&with("i1", "size", json!(SLOT_BYTES)), &context())
                .is_ok()
        );
        gate.finish();
        let refusal = gate
            .accept(&with("i2", "size", json!(SLOT_BYTES + 1)), &context())
            .expect_err("too large");
        assert_eq!(refusal.reason, Reject::TooLarge);
        assert_eq!(gate.active(), None, "a refusal starts nothing");
    }

    #[test]
    fn an_install_without_ota_or_below_the_floor_is_incompatible() {
        let mut gate = InstallGate::new();
        let without = Context {
            offered: false,
            ..context()
        };
        assert_eq!(
            gate.accept(&install("i1"), &without).expect_err("x").reason,
            Reject::Incompatible
        );
        let floor = Context {
            floor: Some((0, 3, 0)),
            ..context()
        };
        assert_eq!(
            gate.accept(&install("i2"), &floor).expect_err("x").reason,
            Reject::Incompatible
        );
        let beta = with("i3", "version", json!("0.3.0-beta.1"));
        assert!(gate.accept(&beta, &floor).is_ok());
    }

    #[test]
    fn the_download_must_come_from_the_ha_origin() {
        let mut gate = InstallGate::new();
        for (index, url) in [
            "https://evil.example/api/njorun/firmware/x",
            "https://ha.local:8123.evil.example/fw",
            "https://ha.local:81234/fw",
            "https://ha.local:8123",
            "http://ha.local:8123/fw",
        ]
        .iter()
        .enumerate()
        {
            let message = with(&format!("o{index}"), "url", json!(url));
            assert_eq!(
                gate.accept(&message, &context()).expect_err(url).reason,
                Reject::Malformed,
                "{url}"
            );
        }
    }

    #[test]
    fn a_lower_version_needs_the_downgrade_flag() {
        let running = Context {
            running: Some((0, 3, 0)),
            ..context()
        };
        let mut gate = InstallGate::new();
        assert_eq!(
            gate.accept(&install("i1"), &running)
                .expect_err("lower")
                .reason,
            Reject::Incompatible,
            "0.2.0 is below 0.3.0"
        );
        let flagged = with("i2", "downgrade", json!(true));
        assert!(gate.accept(&flagged, &running).is_ok(), "deliberate");
        gate.finish();
        let same = with("i3", "version", json!("0.3.0"));
        assert!(
            gate.accept(&same, &running).is_ok(),
            "a reinstall is no downgrade"
        );
        gate.finish();
        let beta_of_same = with("i4", "version", json!("0.3.0-beta.2"));
        assert!(
            gate.accept(&beta_of_same, &running).is_ok(),
            "compared by core"
        );
        gate.finish();
        let plain = with("i5", "downgrade", json!(false));
        assert!(
            gate.accept(&plain, &context()).is_ok(),
            "unknown running version"
        );
    }

    #[test]
    fn the_downgrade_flag_does_not_lower_the_floor() {
        let floor = Context {
            floor: Some((0, 3, 0)),
            running: Some((0, 4, 0)),
            ..context()
        };
        let mut gate = InstallGate::new();
        let flagged = with("i1", "downgrade", json!(true));
        assert_eq!(
            gate.accept(&flagged, &floor).expect_err("floor").reason,
            Reject::Incompatible
        );
        let above = with("i2", "version", json!("0.3.5"));
        let flagged = {
            let mut message = above;
            message["downgrade"] = json!(true);
            message
        };
        assert!(gate.accept(&flagged, &floor).is_ok());
    }

    #[test]
    fn the_downgrade_field_must_be_a_boolean() {
        let refusal = parse(&with("i1", "downgrade", json!("yes"))).expect_err("refused");
        assert_eq!(refusal.reason, Reject::Malformed);
        assert!(!parse(&install("i2")).expect("plain").downgrade);
        assert!(
            parse(&with("i3", "downgrade", json!(true)))
                .expect("flag")
                .downgrade
        );
    }

    #[test]
    fn the_slot_size_is_the_one_read_from_the_partition_table() {
        let big = Context {
            slot_bytes: 14 * 1024 * 1024,
            ..context()
        };
        let mut gate = InstallGate::new();
        let ten = with("i1", "size", json!(10 * 1024 * 1024));
        assert!(gate.accept(&ten, &big).is_ok());
        gate.finish();
        assert_eq!(
            gate.accept(&with("i2", "size", json!(10 * 1024 * 1024)), &context())
                .expect_err("8 MiB slot")
                .reason,
            Reject::TooLarge
        );
    }

    #[test]
    fn versions_compare_by_their_core() {
        assert_eq!(core_version("0.3.0-beta.1+abc"), Some((0, 3, 0)));
        assert_eq!(core_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(core_version("1.2"), None);
        assert_eq!(core_version("1.2.3.4"), None);
        assert_eq!(core_version("x"), None);
    }

    #[test]
    fn results_match_the_fixture_shape() {
        let fixture = fixture();
        let valid = cases(&fixture, "install_result", "valid");
        assert_eq!(result_message("i1", Ok(())), valid[0]);
        let reasons = [
            Reject::Expired,
            Reject::Duplicate,
            Reject::Busy,
            Reject::Incompatible,
            Reject::TooLarge,
            Reject::Malformed,
        ];
        for (index, reason) in reasons.into_iter().enumerate() {
            assert_eq!(result_message("i1", Err(reason)), valid[index + 1]);
        }
    }

    #[test]
    fn progress_matches_the_fixture_and_its_bounds() {
        let fixture = fixture();
        for message in cases(&fixture, "install_progress", "valid") {
            let stage = match message["stage"].as_str().expect("stage") {
                "download" => Stage::Download,
                "verify" => Stage::Verify,
                "write" => Stage::Write,
                _ => Stage::Reboot,
            };
            let built = progress_message(
                "i1",
                stage,
                u32::try_from(message["done"].as_u64().expect("done")).expect("u32"),
                u32::try_from(message["total"].as_u64().expect("total")).expect("u32"),
            );
            assert_eq!(built.as_ref(), Some(&message));
        }
        assert_eq!(progress_message("i1", Stage::Write, 1, 0), None);
        assert_eq!(progress_message("i1", Stage::Write, 2, 1), None);
        assert_eq!(progress_message("", Stage::Write, 1, 1), None);
    }

    #[test]
    fn progress_is_paced_per_stage() {
        let mut pacer = ProgressPacer::new();
        assert!(pacer.due(0, Stage::Download, 0, 100));
        assert!(!pacer.due(500, Stage::Download, 10, 100));
        assert!(pacer.due(1_000, Stage::Download, 20, 100));
        assert!(!pacer.due(1_100, Stage::Download, 30, 100));
        assert!(
            pacer.due(1_200, Stage::Download, 100, 100),
            "end of a stage"
        );
        assert!(pacer.due(1_300, Stage::Verify, 0, 100), "new stage");
        assert!(!pacer.due(1_400, Stage::Verify, 50, 100));
    }

    #[test]
    fn outcomes_follow_the_running_version_and_the_boot_result() {
        assert_eq!(
            Outcome::reconcile("0.2.0", "0.2.0", true),
            Outcome::Installed
        );
        assert_eq!(
            Outcome::reconcile("0.2.0", "0.1.0", true),
            Outcome::RolledBack
        );
        assert_eq!(
            Outcome::reconcile("0.2.0", "0.2.0", false),
            Outcome::RolledBack,
            "an unconfirmed boot is no install"
        );
        let fixture = fixture();
        let valid = cases(&fixture, "install_outcome", "valid");
        assert_eq!(
            outcome_message("i1", Outcome::Installed, "0.2.0").as_ref(),
            Some(&valid[0])
        );
        assert_eq!(
            outcome_message("i1", Outcome::RolledBack, "0.1.0").as_ref(),
            Some(&valid[1])
        );
        assert_eq!(outcome_message("i1", Outcome::Installed, ""), None);
    }
}
