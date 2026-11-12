//! Network settings from Home Assistant on the tablet (plan 006, decisions N2, N5 and N7).
//!
//! A `network` message is tried, not just applied. The tablet keeps the stored values until the
//! new ones are proven: it joins with the candidate, and the candidate counts as working when a
//! management session with Home Assistant opens over it (N5). A candidate for an interface that
//! does not carry the session can only be proven as far as a TCP connection to Home Assistant
//! over it (N7); that needs a second interface, so on the D1001 (Wi-Fi only) the session proof
//! is the one that applies. Then the values are stored and the result is `applied`. When the join fails, or no session opens within the timeout, the tablet joins with
//! the stored values again and the result is `reverted` with the reason.
//!
//! A Wi-Fi trial breaks the session by design, so a result is never sent at the moment it is
//! decided. It is written to storage and reported by the next session. The record is written
//! before the candidate is tried as `reverted`, so a reset or a power loss during the trial
//! also ends with a report once Home Assistant is reachable again.
//!
//! The decisions that need no hardware are in `njorun_app_core::{network_message, trial}`.

use std::sync::Mutex;

use njorun_app_core::link::endpoint;
use njorun_app_core::network::{EthernetConfig, Interface, Mode, NetworkConfig, WifiConfig};
use njorun_app_core::network_message::{
    plan, InterfaceState, Outcome, Pending, Plan, Request, State,
};
use njorun_app_core::trial::{Proof, Reason, Step, Stored, Trial};

use crate::{board, store, uplink};

/// The interfaces of this board: Wi-Fi, and the wired one when the firmware is built for it. The
/// D1001 has Wi-Fi only.
pub(crate) fn interfaces() -> Vec<Interface> {
    let mut all = vec![Interface::Wifi];
    if board::has_ethernet() {
        all.push(Interface::Ethernet);
    }
    all
}

/// What the driver reported for a failed join, as `njorun_d1001_wifi_last_failure` and
/// `njorun_d1001_ethernet_last_failure` number it: 1 association (no link), 2 address.
const FAILURE_ADDRESS: i32 = 2;

/// How long the wired interface gets to show a link and an address in a trial.
const ETHERNET_WAIT_MS: u32 = 15_000;
/// How long the TCP connection to Home Assistant of a link proof may take, per interface.
const PROBE_MS: u32 = 8_000;

/// One attempt with new values, from the join until it is proven or given up.
struct Attempt {
    operation_id: String,
    revision: u64,
    trial: Trial,
    stored: Stored,
    /// The interfaces whose values differ from the stored ones: the ones that are joined anew,
    /// stored on a commit and rejoined on a revert.
    changed: Vec<Interface>,
}

static ATTEMPT: Mutex<Option<Attempt>> = Mutex::new(None);

/// The result that waits to be reported, kept in memory as well so that asking for it on every
/// pass of the session loop does not read flash. `None` outside means not read yet.
static PENDING: Mutex<Option<Option<Pending>>> = Mutex::new(None);

fn pending_cache() -> std::sync::MutexGuard<'static, Option<Option<Pending>>> {
    PENDING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Writes the record to storage and to the cache; `false` when storage refused it.
fn record(pending: Pending) -> bool {
    let saved = store::save_pending_network_result(&pending);
    if saved {
        *pending_cache() = Some(Some(pending));
    }
    saved
}

fn attempt() -> std::sync::MutexGuard<'static, Option<Attempt>> {
    // A panic cannot leave the attempt half written: every change replaces it as a whole.
    ATTEMPT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The values in use: the stored Wi-Fi setup and, on a board with a wired port, its IP setup.
fn current() -> NetworkConfig {
    NetworkConfig {
        wifi: store::load().wifi.map(|wifi| WifiConfig {
            ssid: wifi.ssid,
            password: (!wifi.password.is_empty()).then_some(wifi.password),
            ip: wifi.ip,
        }),
        ethernet: board::has_ethernet().then(|| EthernetConfig {
            ip: store::ethernet_ip(),
        }),
    }
}

/// The interfaces whose values `wanted` changes. An interface it does not mention stays as it is.
fn changed(wanted: &NetworkConfig, current: &NetworkConfig) -> Vec<Interface> {
    let mut result = Vec::new();
    if let Some(wanted) = &wanted.wifi {
        let same = current.wifi.as_ref().is_some_and(|current| {
            wanted.ssid == current.ssid
                && wanted.password.as_deref().unwrap_or_default()
                    == current.password.as_deref().unwrap_or_default()
                && wanted.ip == current.ip
        });
        if !same {
            result.push(Interface::Wifi);
        }
    }
    if let Some(wanted) = &wanted.ethernet {
        if current.ethernet.as_ref().is_none_or(|current| wanted.ip != current.ip) {
            result.push(Interface::Ethernet);
        }
    }
    result
}

/// What the failed join of one interface says, from the driver's number.
fn reason_of(failure: i32) -> Reason {
    if failure == FAILURE_ADDRESS {
        Reason::NoAddress
    } else {
        Reason::NoAssociation
    }
}

/// Joins `which` of the interfaces of `config`. The reason is what a result would say if it
/// failed.
fn join(config: &NetworkConfig, which: &[Interface]) -> Result<(), Reason> {
    for interface in which {
        match interface {
            Interface::Wifi => {
                let Some(wifi) = &config.wifi else {
                    return Err(Reason::NoAssociation);
                };
                board::connect_wifi(&wifi.ssid, wifi.password.as_deref().unwrap_or(""), &wifi.ip)
                    .map_err(|code| {
                        let failure = board::wifi_failure();
                        log::warn!(target: "net", "joining the network failed: error {code:#x}, kind {failure}");
                        reason_of(failure)
                    })?;
            }
            Interface::Ethernet => {
                let Some(ethernet) = &config.ethernet else {
                    return Err(Reason::NoAssociation);
                };
                board::connect_ethernet(&ethernet.ip, ETHERNET_WAIT_MS).map_err(|code| {
                    let failure = board::ethernet_failure();
                    log::warn!(target: "net", "the wired interface failed: error {code:#x}, kind {failure}");
                    reason_of(failure)
                })?;
            }
        }
    }
    Ok(())
}

/// Whether a TCP connection to Home Assistant over `interface` alone succeeds.
fn reaches_home_assistant(interface: Interface) -> bool {
    let saved = store::load();
    let Some(session) = saved.home_assistant else {
        return false;
    };
    let Some((host, port)) = endpoint(&session.url) else {
        return false;
    };
    board::probe_tcp(interface, host, port, PROBE_MS)
}

/// What the network is doing now, for a result. Nothing in it is secret.
pub(crate) fn state(session_open: bool) -> State {
    let current = current();
    let carries = |interface: Interface| session_open && uplink::carries(interface);
    State {
        wifi: Some(InterfaceState {
            mode: current
                .wifi
                .as_ref()
                .map_or(Mode::Dhcp, |wifi| wifi.ip.mode),
            address: board::wifi_address(),
            ssid: current.wifi.map(|wifi| wifi.ssid),
            carries_session: carries(Interface::Wifi),
        }),
        ethernet: current.ethernet.map(|ethernet| InterfaceState {
            mode: ethernet.ip.mode,
            address: board::ethernet_address(),
            ssid: None,
            carries_session: carries(Interface::Ethernet),
        }),
    }
}

/// What the caller does with a message that passed the gate.
pub(crate) enum Begun {
    /// Nothing was tried; answer now with this outcome over the session that is open.
    Answer(Outcome),
    /// The values are being tried. The result is reported by a session that opens later.
    Deferred,
}

/// Starts an attempt with `request`, or says what to answer at once.
pub(crate) fn begin(request: &Request) -> Begun {
    let mut slot = attempt();
    if slot.is_some() {
        // The tablet is in the middle of a trial. Home Assistant sends one message at a time.
        log::warn!(target: "net", "network revision {} refused: a trial is running", request.revision);
        return Begun::Answer(Outcome::Reverted(Reason::RejectedValues));
    }
    let current = current();
    match plan(request, store::network_revision(), &current) {
        Plan::AlreadyApplied => {
            log::info!(target: "net", "network revision {} is what is in use", request.revision);
            store::save_network_revision(request.revision);
            return Begun::Answer(Outcome::Applied(Proof::Session));
        }
        Plan::Stale => {
            log::warn!(target: "net", "network revision {} repeats a finished one with other values", request.revision);
            return Begun::Answer(Outcome::Reverted(Reason::RejectedValues));
        }
        Plan::Try => {}
    }
    // Written first as `reverted`: if the tablet resets while it tries, it comes back on the
    // stored values and reports exactly that.
    let pending = Pending {
        operation_id: request.operation_id.clone(),
        revision: request.revision,
        outcome: Outcome::Reverted(Reason::NoHomeAssistant),
    };
    if !record(pending) {
        return Begun::Answer(Outcome::Reverted(Reason::RejectedValues));
    }
    log::info!(target: "net", "trying network revision {}", request.revision);
    let changed = changed(&request.config, &current);
    // An interface that carries the session can only be proven by the session, which the join
    // itself interrupts. One that does not can be proven by a connection over it alone (N7).
    let proof = if [Interface::Wifi, Interface::Ethernet]
        .iter()
        .any(|interface| uplink::carries(*interface) && !changed.contains(interface))
    {
        Proof::Link
    } else {
        Proof::Session
    };
    let mut stored = Stored::new(current);
    stored.begin(&request.config);
    let mut trial = Trial::start(board::uptime_ms(), proof);
    let joined = join(stored.in_use(), &changed);
    if let Err(reason) = joined {
        trial.failed(reason);
    } else {
        // `join` returns with an address, so the next thing to prove is the session or, for a
        // link that does not carry it, the connection.
        trial.joined();
        trial.address();
    }
    let running = Attempt {
        operation_id: request.operation_id.clone(),
        revision: request.revision,
        trial,
        stored,
        changed,
    };
    match joined {
        Err(reason) => give_up(running, reason),
        Ok(()) if proof == Proof::Link => finish_link(running),
        Ok(()) => *slot = Some(running),
    }
    Begun::Deferred
}

/// Proves a link that does not carry the session by a connection to Home Assistant over it.
fn finish_link(mut running: Attempt) {
    let reached = running
        .changed
        .iter()
        .all(|interface| reaches_home_assistant(*interface));
    if reached && matches!(running.trial.reached(), Step::Commit(Proof::Link)) {
        commit(running, Proof::Link);
    } else {
        give_up(running, Reason::NoHomeAssistant);
    }
}

/// The attempt worked: the values become the stored ones and the result is kept to report.
fn commit(mut done: Attempt, proof: Proof) {
    let config = done.stored.commit().clone();
    if done.changed.contains(&Interface::Wifi) {
        if let Some(wifi) = &config.wifi {
            store::save_wifi(&wifi.ssid, wifi.password.as_deref().unwrap_or(""), &wifi.ip);
        }
    }
    if done.changed.contains(&Interface::Ethernet) {
        if let Some(ethernet) = &config.ethernet {
            store::save_ethernet_ip(&ethernet.ip);
        }
    }
    store::save_network_revision(done.revision);
    record(Pending {
        operation_id: done.operation_id,
        revision: done.revision,
        outcome: Outcome::Applied(proof),
    });
    log::info!(target: "net", "network revision {} applied", done.revision);
}

/// Time passes. A session that runs over one of the interfaces being tried is the proof of the
/// running attempt, and its values become the stored ones. At the deadline the tablet returns to
/// the stored values. Cheap when nothing runs.
pub(crate) fn poll(session_open: bool) {
    let mut slot = attempt();
    let Some(running) = slot.as_mut() else {
        return;
    };
    let step = if session_open
        && running
            .changed
            .iter()
            .any(|interface| uplink::carries(*interface))
    {
        match running.trial.session_opened() {
            Step::Continue => running.trial.tick(board::uptime_ms()),
            decided => decided,
        }
    } else {
        running.trial.tick(board::uptime_ms())
    };
    let Some(done) = (match step {
        Step::Continue => None,
        Step::Commit(_) | Step::Revert(_) => slot.take(),
    }) else {
        return;
    };
    drop(slot);
    match step {
        Step::Commit(proof) => commit(done, proof),
        Step::Revert(reason) => give_up(done, reason),
        Step::Continue => {}
    }
}

/// The attempt failed: join the changed interfaces with the stored values again and keep the
/// result to report.
fn give_up(mut done: Attempt, reason: Reason) {
    done.stored.abandon();
    log::warn!(target: "net", "network revision {} reverted: {}", done.revision, reason.wire());
    if join(done.stored.persisted(), &done.changed).is_err() {
        log::warn!(target: "net", "the stored network did not join either");
    }
    store::save_network_revision(done.revision);
    record(Pending {
        operation_id: done.operation_id,
        revision: done.revision,
        outcome: Outcome::Reverted(reason),
    });
}

/// The result to report now, if there is one. Nothing while an attempt runs, because the
/// record then only says what would happen if the tablet reset.
pub(crate) fn pending() -> Option<Pending> {
    if attempt().is_some() {
        return None;
    }
    pending_cache()
        .get_or_insert_with(store::pending_network_result)
        .clone()
}

/// Home Assistant has the result, or will never take it.
pub(crate) fn reported() {
    store::clear_pending_network_result();
    *pending_cache() = Some(None);
}
