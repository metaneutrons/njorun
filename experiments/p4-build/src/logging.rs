//! Panel logging: one `log` facade for the whole firmware.
//!
//! Every record goes to the console, to a crash ring in RTC memory (kept across a software
//! reset and replayed after a crash) and to a queue that a background thread ships to a
//! Grafana Loki push endpoint. ESP-IDF's own log lines take the same path through a C hook.
//!
//! The sink is configured in Home Assistant with the `input_text.njorun_log_sink` helper: a
//! Loki URL such as `https://loki.example.org`, or empty/`off`. Secrets are masked before a
//! record leaves the device (see `njorun_app_core::logging::redact`).

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use log::{LevelFilter, Log, Metadata, Record};
use njorun_app_core::logging::{
    format_line, loki_push, parse_line, wall_nanos, Entry, Level, Origin, SinkSetting,
};

use crate::board;

/// Home Assistant helper that holds the sink URL.
pub(crate) const SINK_ENTITY: &str = "input_text.njorun_log_sink";

const SEND_EVERY: Duration = Duration::from_secs(5);
const BATCH_MAX: usize = 100;
const PENDING_MAX: usize = 400;
const POST_TIMEOUT_MS: u32 = 5_000;
const MIN_VALID_UNIX_SECONDS: u64 = 1_704_067_200;

#[derive(Clone, PartialEq, Eq)]
enum Sink {
    /// Home Assistant has not answered yet: keep everything.
    Unknown,
    Off,
    On(String),
}

static SINK: Mutex<Sink> = Mutex::new(Sink::Unknown);
static WORKER: Mutex<Option<SinkWorker>> = Mutex::new(None);

/// Sink state, driven by `tick` from a thread that already runs (the Home Assistant
/// worker), so logging costs no thread stack of its own in scarce internal RAM.
struct SinkWorker {
    origin: Origin,
    previous: Option<(u32, String)>,
    pending: Vec<Entry>,
    buffer: Vec<u8>,
    backoff: Duration,
    last_send: Instant,
}
static LOGGER: PanelLogger = PanelLogger;

struct PanelLogger;

/// Writes one line to the console, ignoring failures. `println!` panics when the USB
/// console write fails (EIO when no host reads it, such as on a charger), which turned every
/// start into a panic and a restart loop.
fn console(line: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
}

fn level_of(level: log::Level) -> Level {
    match level {
        log::Level::Error => Level::Error,
        log::Level::Warn => Level::Warn,
        log::Level::Info => Level::Info,
        log::Level::Debug => Level::Debug,
        log::Level::Trace => Level::Trace,
    }
}

impl Log for PanelLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let target = record
            .target()
            .rsplit("::")
            .next()
            .unwrap_or(record.target());
        let line = format_line(
            level_of(record.level()),
            board::uptime_ms(),
            target,
            &record.args().to_string(),
        );
        console(&line);
        board::log_append(&line);
    }

    fn flush(&self) {}
}

/// Installs the logger and the ESP-IDF hook, and starts the sink thread. Call first in `main`.
pub(crate) fn init() {
    if let Err(code) = board::log_init() {
        console(&format!(
            "E (0) log: crash log and sink unavailable: error {code}"
        ));
    }
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
    let previous = board::log_previous();
    let origin = Origin {
        host: format!("njorun-{}", board::station_mac()),
        firmware: concat!(env!("CARGO_PKG_VERSION"), "-d1001").to_owned(),
        boot: board::log_boot(),
    };
    log::info!(target: "log", "boot {} on {} ({})", origin.boot, origin.host, origin.firmware);
    *WORKER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(SinkWorker {
        origin,
        previous,
        pending: Vec::new(),
        buffer: vec![0_u8; 16 * 1024],
        backoff: SEND_EVERY,
        last_send: Instant::now(),
    });
}

/// Applies the Home Assistant helper's value: `None` when the helper does not exist.
pub(crate) fn set_sink(value: Option<&str>) {
    let next = match value.map(njorun_app_core::logging::parse_sink) {
        None | Some(Some(SinkSetting::Off)) => Sink::Off,
        Some(Some(SinkSetting::Loki(url))) => Sink::On(url),
        Some(None) => {
            log::warn!(target: "log", "{SINK_ENTITY} is not a valid http(s) URL; remote logging off");
            Sink::Off
        }
    };
    let mut sink = SINK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *sink != next {
        match &next {
            Sink::On(url) => log::info!(target: "log", "remote logging to {url}"),
            Sink::Off => log::info!(target: "log", "remote logging off"),
            Sink::Unknown => {}
        }
        *sink = next;
    }
}

fn unix_now() -> Option<Duration> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .filter(|now| now.as_secs() >= MIN_VALID_UNIX_SECONDS)
}

/// The previous boot's crash log as records, oldest first.
fn crash_replay(previous: Option<(u32, String)>, now: Duration) -> Vec<Entry> {
    let Some((boot, text)) = previous else {
        return Vec::new();
    };
    let base = now.as_nanos();
    text.lines()
        .filter_map(|line| parse_line(line, Level::Info))
        .enumerate()
        .map(|(index, record)| Entry {
            unix_nanos: base + index as u128,
            record,
            crash: true,
            boot,
        })
        .collect()
}

/// Ships queued records when due. Cheap when there is nothing to do; call it often.
pub(crate) fn tick() {
    let Ok(mut guard) = WORKER.try_lock() else {
        return;
    };
    let Some(worker) = guard.as_mut() else {
        return;
    };
    worker.tick();
}

impl SinkWorker {
    fn tick(&mut self) {
        let sink = SINK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let url = match sink {
            Sink::Unknown => return,
            Sink::Off => {
                // Keep the queue fresh; the console and the crash ring still have everything.
                self.pending.clear();
                self.previous = None;
                let _ = board::log_drain(&mut self.buffer);
                return;
            }
            Sink::On(url) => url,
        };
        if self.last_send.elapsed() < self.backoff {
            return;
        }
        let Some(now) = unix_now() else {
            return;
        };
        let now_uptime = board::uptime_ms();
        if let Some(replay) = self.previous.take() {
            let entries = crash_replay(Some(replay), now);
            if !entries.is_empty() {
                log::warn!(target: "log", "sending the log of the previous boot ({} lines)", entries.len());
            }
            self.pending.extend(entries);
        }
        let (text, dropped) = board::log_drain(&mut self.buffer);
        if dropped > 0 {
            self.pending.push(Entry {
                unix_nanos: now.as_nanos(),
                record: njorun_app_core::logging::Record {
                    level: Level::Warn,
                    target: "log".to_owned(),
                    message: format!("{dropped} log lines dropped: queue full"),
                    uptime_ms: Some(now_uptime),
                },
                crash: false,
                boot: self.origin.boot,
            });
        }
        for line in text.lines() {
            if let Some(record) = parse_line(line, Level::Info) {
                self.pending.push(Entry {
                    unix_nanos: wall_nanos(now, now_uptime, record.uptime_ms),
                    record,
                    crash: false,
                    boot: self.origin.boot,
                });
            }
        }
        if self.pending.len() > PENDING_MAX {
            let excess = self.pending.len() - PENDING_MAX;
            self.pending.drain(..excess);
        }
        self.last_send = Instant::now();
        if self.pending.is_empty() {
            return;
        }
        let count = self.pending.len().min(BATCH_MAX);
        let body = loki_push(&self.origin, &self.pending[..count]).to_string();
        match board::http_post(&url, "application/json", body.as_bytes(), POST_TIMEOUT_MS) {
            Ok(()) => {
                self.pending.drain(..count);
                self.backoff = SEND_EVERY;
            }
            Err(code) => {
                self.backoff = (self.backoff * 2).min(Duration::from_secs(120));
                console(&format!(
                    "W ({}) log: log sink {url} failed: error {code:#x}, next try in {} s",
                    board::uptime_ms(),
                    self.backoff.as_secs()
                ));
            }
        }
    }
}
