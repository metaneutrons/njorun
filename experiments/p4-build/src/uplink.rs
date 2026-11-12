// SPDX-FileCopyrightText: 2026 Fabian Schmieder
// SPDX-License-Identifier: GPL-3.0-or-later

//! Which interface carries the session (plan 006, decision N8).
//!
//! The decision is `njorun_app_core::link::Selector`. This module feeds it what the board sees
//! and makes the default route follow. On a board without a wired port (the D1001) Wi-Fi is the
//! only candidate and nothing changes. The wired part has not run on a board yet.

use std::sync::Mutex;

use njorun_app_core::link::{Observation, Selector};
use njorun_app_core::network::Interface;

use crate::{board, store};

/// How long a wired start waits for the link when the firmware starts. The tablet does not wait
/// at all: the link and the address are looked at again on every pass of the session loop.
const START_WAIT_MS: u32 = 0;

struct Uplink {
    selector: Selector,
    /// What the route points at now, so that it is set only when it changes.
    routed: Option<Interface>,
}

static UPLINK: Mutex<Uplink> = Mutex::new(Uplink {
    selector: Selector::new(),
    routed: None,
});

fn uplink() -> std::sync::MutexGuard<'static, Uplink> {
    // Every change replaces a field as a whole, so a panic leaves nothing half written.
    UPLINK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Starts the wired interface with its stored setup, when the board has one. Returns at once.
pub(crate) fn start() {
    if !board::has_ethernet() {
        return;
    }
    match board::connect_ethernet(&store::ethernet_ip(), START_WAIT_MS) {
        Ok(()) => log::info!(target: "net", "wired interface started"),
        Err(code) => log::warn!(target: "net", "wired interface did not start: error {code:#x}"),
    }
}

/// Looks at both interfaces and points the default route at the one that should carry the
/// session. Called on every pass of the session loop, `session_open` says whether a session is
/// open over the interface chosen last.
pub(crate) fn update(session_open: bool) {
    let observation = Observation {
        wired_ready: board::has_ethernet() && board::ethernet_address().is_some(),
        wifi_ready: board::wifi_address().is_some(),
        session_open,
    };
    let mut state = uplink();
    let chosen = state.selector.update(board::uptime_ms(), observation);
    if chosen == state.routed {
        return;
    }
    if let Some(interface) = chosen {
        // Only a board with both interfaces has a choice to make; with one, the stack already
        // routes over it.
        if board::has_ethernet() {
            match board::prefer_interface(interface) {
                Ok(()) => log::info!(target: "net", "the session now runs over {interface:?}"),
                Err(code) => {
                    // Not tried again on every pass; the next change of the choice tries again.
                    log::warn!(target: "net", "route to {interface:?} not set: error {code:#x}");
                }
            }
        }
    }
    state.routed = chosen;
}

/// Whether the session runs over `interface`. Without a wired port the session can only run
/// over Wi-Fi, so that holds before the first pass of [`update`] too.
pub(crate) fn carries(interface: Interface) -> bool {
    match uplink().routed {
        Some(routed) => routed == interface,
        None => !board::has_ethernet() && interface == Interface::Wifi,
    }
}
