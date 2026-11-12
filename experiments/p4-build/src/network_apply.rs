//! Network settings from Home Assistant on the tablet (plan 006, decisions N2, N5 and N7).
//!
//! A `network` message is tried, not just applied. The tablet keeps the stored values until the
//! new ones are proven: it joins with the candidate, and the candidate counts as working when a
//! management session with Home Assistant opens over it (the D1001 has Wi-Fi only, so the
//! session proof of N5 is the one that applies). Then the values are stored and the result is
//! `applied`. When the join fails, or no session opens within the timeout, the tablet joins with
//! the stored values again and the result is `reverted` with the reason.
//!
//! A Wi-Fi trial breaks the session by design, so a result is never sent at the moment it is
//! decided. It is written to storage and reported by the next session. The record is written
//! before the candidate is tried as `reverted`, so a reset or a power loss during the trial
//! also ends with a report once Home Assistant is reachable again.
//!
//! The decisions that need no hardware are in `njorun_app_core::{network_message, trial}`.

use std::sync::Mutex;

use njorun_app_core::network::{Interface, Mode, NetworkConfig, WifiConfig};
use njorun_app_core::network_message::{
    plan, InterfaceState, Outcome, Pending, Plan, Request, State,
};
use njorun_app_core::trial::{Proof, Reason, Step, Stored, Trial};

use crate::{board, store};

/// The interfaces of this board. The D1001 has Wi-Fi only.
pub(crate) const INTERFACES: [Interface; 1] = [Interface::Wifi];

/// What the driver reported for a failed join, as `njorun_d1001_wifi_last_failure` numbers it.
const FAILURE_ADDRESS: i32 = 2;

/// One attempt with new values, from the join until it is proven or given up.
struct Attempt {
    operation_id: String,
    revision: u64,
    trial: Trial,
    stored: Stored,
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

/// The values in use: the stored Wi-Fi setup.
fn current() -> NetworkConfig {
    NetworkConfig {
        wifi: store::load().wifi.map(|wifi| WifiConfig {
            ssid: wifi.ssid,
            password: (!wifi.password.is_empty()).then_some(wifi.password),
            ip: wifi.ip,
        }),
        ethernet: None,
    }
}

/// Joins with the Wi-Fi of `config`. The reason is what a result would say if it failed.
fn join(config: &NetworkConfig) -> Result<(), Reason> {
    let Some(wifi) = &config.wifi else {
        return Err(Reason::NoAssociation);
    };
    board::connect_wifi(&wifi.ssid, wifi.password.as_deref().unwrap_or(""), &wifi.ip).map_err(
        |code| {
            let failure = board::wifi_failure();
            log::warn!(target: "net", "joining the network failed: error {code:#x}, kind {failure}");
            if failure == FAILURE_ADDRESS {
                Reason::NoAddress
            } else {
                Reason::NoAssociation
            }
        },
    )
}

/// What the network is doing now, for a result. Nothing in it is secret.
pub(crate) fn state(carries_session: bool) -> State {
    let wifi = current().wifi;
    State {
        wifi: Some(InterfaceState {
            mode: wifi.as_ref().map_or(Mode::Dhcp, |wifi| wifi.ip.mode),
            address: board::wifi_address(),
            ssid: wifi.map(|wifi| wifi.ssid),
            carries_session,
        }),
        ethernet: None,
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
    let mut stored = Stored::new(current);
    stored.begin(&request.config);
    let mut trial = Trial::start(board::uptime_ms(), Proof::Session);
    match join(stored.in_use()) {
        Ok(()) => {
            // `join` returns with an address, so the next thing to prove is the session.
            trial.joined();
            trial.address();
            *slot = Some(Attempt {
                operation_id: request.operation_id.clone(),
                revision: request.revision,
                trial,
                stored,
            });
        }
        Err(reason) => {
            trial.failed(reason);
            give_up(
                Attempt {
                    operation_id: request.operation_id.clone(),
                    revision: request.revision,
                    trial,
                    stored,
                },
                reason,
            );
        }
    }
    Begun::Deferred
}

/// A management session opened. When it is the proof of the running attempt, the values become
/// the stored ones.
pub(crate) fn session_opened() {
    let mut slot = attempt();
    let Some(running) = slot.as_mut() else {
        return;
    };
    let Step::Commit(proof) = running.trial.session_opened() else {
        return;
    };
    let Some(mut done) = slot.take() else {
        return;
    };
    let config = done.stored.commit().clone();
    if let Some(wifi) = &config.wifi {
        store::save_wifi(&wifi.ssid, wifi.password.as_deref().unwrap_or(""), &wifi.ip);
    }
    store::save_network_revision(done.revision);
    record(Pending {
        operation_id: done.operation_id,
        revision: done.revision,
        outcome: Outcome::Applied(proof),
    });
    log::info!(target: "net", "network revision {} applied", done.revision);
}

/// Time passes. At the deadline of the running attempt the tablet returns to the stored values.
/// Cheap when nothing runs.
pub(crate) fn poll() {
    let mut slot = attempt();
    let Some(running) = slot.as_mut() else {
        return;
    };
    let Step::Revert(reason) = running.trial.tick(board::uptime_ms()) else {
        return;
    };
    if let Some(done) = slot.take() {
        drop(slot);
        give_up(done, reason);
    }
}

/// The attempt failed: join with the stored values again and keep the result to report.
fn give_up(mut done: Attempt, reason: Reason) {
    done.stored.abandon();
    log::warn!(target: "net", "network revision {} reverted: {}", done.revision, reason.wire());
    if join(done.stored.persisted()).is_err() {
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
