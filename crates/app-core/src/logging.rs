//! Log records for the panel: one format for the console, the crash log and a remote sink,
//! with secrets removed before anything leaves the device.
//!
//! The firmware feeds raw lines (its own records and ESP-IDF's console output) through
//! [`parse_line`] and ships batches built by [`loki_push`] to a Grafana Loki push endpoint.

use serde_json::{Value, json};

/// Severity, ordered from most to least severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    /// The lowercase name Loki and Grafana use for the `level` label.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    /// The single letter used on the console and in the crash log.
    #[must_use]
    pub const fn letter(self) -> char {
        match self {
            Self::Error => 'E',
            Self::Warn => 'W',
            Self::Info => 'I',
            Self::Debug => 'D',
            Self::Trace => 'V',
        }
    }

    #[must_use]
    pub const fn from_letter(letter: char) -> Option<Self> {
        match letter {
            'E' => Some(Self::Error),
            'W' => Some(Self::Warn),
            'I' => Some(Self::Info),
            'D' => Some(Self::Debug),
            'V' => Some(Self::Trace),
            _ => None,
        }
    }
}

/// One parsed log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub level: Level,
    /// Subsystem: a Rust module area (`ha`, `wifi`, …) or an ESP-IDF tag.
    pub target: String,
    pub message: String,
    /// Milliseconds since boot, when the line carried it.
    pub uptime_ms: Option<u64>,
}

/// The console and crash-log form of a panel record: `I (12345) ha: message`.
#[must_use]
pub fn format_line(level: Level, uptime_ms: u64, target: &str, message: &str) -> String {
    format!("{} ({uptime_ms}) {target}: {message}", level.letter())
}

/// Removes ANSI colour codes, as ESP-IDF writes them on the console.
#[must_use]
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Parses `L (uptime) target: message`, the format of both the panel and ESP-IDF. Lines
/// without that shape (banners, panic dumps) become `fallback` level records with target
/// `console`.
#[must_use]
pub fn parse_line(raw: &str, fallback: Level) -> Option<Record> {
    let text = strip_ansi(raw);
    let text = text.trim_end_matches(['\r', '\n']).trim_end();
    if text.trim().is_empty() {
        return None;
    }
    let mut chars = text.chars();
    let structured = chars.next().and_then(Level::from_letter).and_then(|level| {
        let rest = text.get(1..)?.strip_prefix(" (")?;
        let close = rest.find(')')?;
        let uptime_ms = rest[..close].parse::<u64>().ok()?;
        let rest = rest[close + 1..].strip_prefix(' ')?;
        let colon = rest.find(": ")?;
        let target = &rest[..colon];
        if target.is_empty() || target.len() > 48 || target.contains(' ') {
            return None;
        }
        Some(Record {
            level,
            target: target.to_owned(),
            message: rest[colon + 2..].to_owned(),
            uptime_ms: Some(uptime_ms),
        })
    });
    Some(structured.unwrap_or_else(|| Record {
        level: fallback,
        target: "console".to_owned(),
        message: text.to_owned(),
        uptime_ms: None,
    }))
}

const SECRET_KEYS: [&str; 7] = [
    "access_token",
    "refresh_token",
    "password",
    "code_verifier",
    "client_secret",
    "code",
    "token",
];

fn mask_after(text: &mut String, start: usize) {
    let end = text[start..]
        .find(['"', '&', ',', ' ', '}', '\'', ';', '\n'])
        .map_or(text.len(), |offset| start + offset);
    if end > start {
        text.replace_range(start..end, "***");
    }
}

/// Masks secrets so no token, password or authorization code leaves the device: bearer
/// tokens, `key=value` and `"key":"value"` pairs for known secret keys, and long base64url
/// runs that look like tokens.
#[must_use]
pub fn redact(text: &str) -> String {
    let mut out = text.to_owned();
    // Bearer tokens.
    let mut search = 0;
    while let Some(offset) = out[search..].find("Bearer ") {
        let start = search + offset + "Bearer ".len();
        mask_after(&mut out, start);
        search = start;
    }
    // key=value and "key":"value" / "key": "value".
    for key in SECRET_KEYS {
        for pattern in [
            format!("{key}="),
            format!("\"{key}\":\""),
            format!("\"{key}\": \""),
        ] {
            let mut search = 0;
            while let Some(offset) = out[search..].find(&pattern) {
                let at = search + offset;
                // `code=` must not match inside `error_code=` or `postcode=`.
                let boundary = at == 0
                    || !out[..at]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
                let start = at + pattern.len();
                if boundary {
                    mask_after(&mut out, start);
                }
                search = start.min(out.len());
            }
        }
    }
    // Long base64url runs: JWTs and refresh tokens.
    let mut result = String::with_capacity(out.len());
    let mut run = String::new();
    let flush = |run: &mut String, result: &mut String| {
        if run.len() >= 40 && run.chars().any(|c| c.is_ascii_digit()) {
            result.push_str("***");
        } else {
            result.push_str(run);
        }
        run.clear();
    };
    for c in out.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
            run.push(c);
        } else {
            flush(&mut run, &mut result);
            result.push(c);
        }
    }
    flush(&mut run, &mut result);
    result
}

/// Where the panel should send its logs, from the user's setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkSetting {
    Off,
    /// A full Loki push URL.
    Loki(String),
}

/// Reads the sink setting: empty or `off` disables it; `http(s)://host[:port]` gets the
/// standard push path; a URL with a path is used as given. Rejects anything else.
#[must_use]
pub fn parse_sink(value: &str) -> Option<SinkSetting> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("off") {
        return Some(SinkSetting::Off);
    }
    let rest = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host_ok = !authority.is_empty()
        && !authority.contains('@')
        && authority.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'[' | b']')
        });
    if !host_ok || value.len() > 256 || value.contains(['?', '#', ' ']) {
        return None;
    }
    let scheme = if value.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    let path = path.trim_end_matches('/');
    Some(SinkSetting::Loki(if path.is_empty() {
        format!("{scheme}://{authority}/loki/api/v1/push")
    } else {
        format!("{scheme}://{authority}/{path}")
    }))
}

/// Whether a reset reason (as the board names it) means the previous run crashed, so its log
/// is worth replaying. A normal restart, an update or a power cycle is not a crash.
#[must_use]
pub fn is_crash_reset(reason: &str) -> bool {
    matches!(
        reason,
        "panic" | "interrupt watchdog" | "task watchdog" | "other watchdog" | "CPU lockup"
    )
}

/// Identity attached to every record the panel ships.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// Low-cardinality host label, such as `njorun-e8f60ae0464c`.
    pub host: String,
    pub firmware: String,
    /// Counts boots, so records of one run can be told apart.
    pub boot: u32,
}

/// A record ready to ship, with its wall-clock time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub unix_nanos: u128,
    pub record: Record,
    /// Replayed from the previous boot's crash log.
    pub crash: bool,
    /// The boot the record belongs to (differs from `Origin::boot` for crash replays).
    pub boot: u32,
}

/// The JSON body of a Loki push: one stream per label combination, each line a JSON object
/// with the record's details. Messages are redacted here, whatever the caller did before.
#[must_use]
pub fn loki_push(origin: &Origin, entries: &[Entry]) -> Value {
    let mut streams: Vec<(String, &'static str, bool, Vec<Value>)> = Vec::new();
    for entry in entries {
        let level = entry.record.level.name();
        let line = json!({
            "target": entry.record.target,
            "msg": redact(&entry.record.message),
            "boot": entry.boot,
            "uptime_ms": entry.record.uptime_ms,
            "fw": origin.firmware,
        })
        .to_string();
        let value = json!([entry.unix_nanos.to_string(), line]);
        if let Some(stream) = streams
            .iter_mut()
            .find(|(_, l, crash, _)| *l == level && *crash == entry.crash)
        {
            stream.3.push(value);
        } else {
            streams.push((origin.host.clone(), level, entry.crash, vec![value]));
        }
    }
    json!({
        "streams": streams
            .into_iter()
            .map(|(host, level, crash, values)| {
                let mut labels = json!({
                    "source": "panel",
                    "app": "njorun",
                    "host": host,
                    "level": level,
                });
                if crash {
                    labels["kind"] = json!("crash");
                }
                json!({"stream": labels, "values": values})
            })
            .collect::<Vec<_>>()
    })
}

/// Wall-clock nanoseconds of a record logged at `uptime_ms`, given the clock `now` and the
/// current uptime. Records without an uptime get `now`.
#[must_use]
pub fn wall_nanos(now: std::time::Duration, now_uptime_ms: u64, uptime_ms: Option<u64>) -> u128 {
    let age = now_uptime_ms.saturating_sub(uptime_ms.unwrap_or(now_uptime_ms));
    now.as_nanos().saturating_sub(u128::from(age) * 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::{
        Entry, Level, Origin, Record, SinkSetting, format_line, is_crash_reset, loki_push,
        parse_line, parse_sink, redact, strip_ansi, wall_nanos,
    };

    #[test]
    fn records_get_their_own_wall_time() {
        let now = std::time::Duration::from_hours(500_000);
        assert_eq!(wall_nanos(now, 10_000, Some(10_000)), now.as_nanos());
        assert_eq!(
            wall_nanos(now, 10_000, Some(7_500)),
            now.as_nanos() - 2_500_000_000
        );
        assert_eq!(wall_nanos(now, 10_000, None), now.as_nanos());
        assert_eq!(wall_nanos(now, 10_000, Some(20_000)), now.as_nanos());
    }

    #[test]
    fn panel_and_esp_idf_lines_parse_alike() {
        let own = format_line(Level::Warn, 1234, "ha", "session lost");
        assert_eq!(own, "W (1234) ha: session lost");
        assert_eq!(
            parse_line(&own, Level::Info),
            Some(Record {
                level: Level::Warn,
                target: "ha".into(),
                message: "session lost".into(),
                uptime_ms: Some(1234),
            })
        );
        let esp = "\u{1b}[0;31mE (47288) esp-tls: [sock=54] select() timeout\u{1b}[0m\n";
        let record = parse_line(esp, Level::Info).unwrap();
        assert_eq!(record.level, Level::Error);
        assert_eq!(record.target, "esp-tls");
        assert_eq!(record.message, "[sock=54] select() timeout");
        assert_eq!(record.uptime_ms, Some(47288));
    }

    #[test]
    fn unstructured_lines_keep_their_text() {
        let record = parse_line("Guru Meditation Error: Core 0 panic'ed", Level::Error).unwrap();
        assert_eq!(record.level, Level::Error);
        assert_eq!(record.target, "console");
        assert!(record.message.starts_with("Guru"));
        assert_eq!(parse_line("   \n", Level::Info), None);
        assert_eq!(strip_ansi("\u{1b}[0;32mI\u{1b}[0m"), "I");
    }

    #[test]
    fn secrets_are_masked() {
        // A dummy JWT, assembled so secret scanners don't flag the source.
        let token = [
            "eyJhbGciOiJIUzI1NiJ9",
            "eyJpc3MiOiIxMjM0NTY3ODkwIn0",
            "abc123def456ghi789",
        ]
        .join(".");
        assert_eq!(
            redact(&format!("Authorization: Bearer {token}")),
            "Authorization: Bearer ***"
        );
        assert_eq!(
            redact("grant_type=refresh_token&refresh_token=abc&client_id=x"),
            "grant_type=refresh_token&refresh_token=***&client_id=x"
        );
        assert_eq!(
            redact(r#"{"type":"auth","access_token":"secret-1"}"#),
            r#"{"type":"auth","access_token":"***"}"#
        );
        assert_eq!(redact("code=XYZ&state=1"), "code=***&state=1");
        assert_eq!(redact("error_code=5"), "error_code=5", "only whole keys");
        assert_eq!(
            redact(&format!("token was {}", "a1b2c3d4".repeat(6))),
            "token was ***"
        );
        assert_eq!(redact("plain message 42"), "plain message 42");
    }

    #[test]
    fn only_crashes_count_as_crash_resets() {
        for reason in [
            "panic",
            "interrupt watchdog",
            "task watchdog",
            "other watchdog",
            "CPU lockup",
        ] {
            assert!(is_crash_reset(reason), "{reason}");
        }
        for reason in [
            "power-on", "software", "USB", "external", "JTAG", "brownout", "unknown",
        ] {
            assert!(!is_crash_reset(reason), "{reason}");
        }
    }

    #[test]
    fn sink_setting_is_parsed_strictly() {
        assert_eq!(parse_sink(""), Some(SinkSetting::Off));
        assert_eq!(parse_sink(" off "), Some(SinkSetting::Off));
        assert_eq!(
            parse_sink("http://192.168.2.5:3100"),
            Some(SinkSetting::Loki(
                "http://192.168.2.5:3100/loki/api/v1/push".into()
            ))
        );
        assert_eq!(
            parse_sink("https://logs.example/loki/api/v1/push/"),
            Some(SinkSetting::Loki(
                "https://logs.example/loki/api/v1/push".into()
            ))
        );
        assert_eq!(parse_sink("ftp://x"), None);
        assert_eq!(parse_sink("http://user:pw@host"), None);
        assert_eq!(parse_sink("http://host/?q=1"), None);
    }

    #[test]
    fn loki_push_groups_by_level_and_marks_crash_replays() {
        let origin = Origin {
            host: "njorun-test".into(),
            firmware: "0.1.0".into(),
            boot: 7,
        };
        let entry = |level, message: &str, crash, boot| Entry {
            unix_nanos: 1_700_000_000_000_000_000,
            record: Record {
                level,
                target: "ha".into(),
                message: message.into(),
                uptime_ms: Some(5),
            },
            crash,
            boot,
        };
        let body = loki_push(
            &origin,
            &[
                entry(Level::Info, "one", false, 7),
                entry(Level::Info, "two", false, 7),
                entry(Level::Error, "refresh_token=abc", false, 7),
                entry(Level::Error, "panic", true, 6),
            ],
        );
        let streams = body["streams"].as_array().unwrap();
        assert_eq!(streams.len(), 3);
        assert_eq!(streams[0]["stream"]["level"], "info");
        assert_eq!(streams[0]["values"].as_array().unwrap().len(), 2);
        assert_eq!(streams[0]["stream"]["source"], "panel");
        assert_eq!(streams[0]["stream"]["host"], "njorun-test");
        assert!(streams[0]["stream"].get("kind").is_none());
        let error_line: serde_json::Value =
            serde_json::from_str(streams[1]["values"][0][1].as_str().unwrap()).unwrap();
        assert_eq!(error_line["msg"], "refresh_token=***");
        assert_eq!(error_line["boot"], 7);
        assert_eq!(streams[2]["stream"]["kind"], "crash");
        assert_eq!(streams[0]["values"][0][0], "1700000000000000000");
    }
}
