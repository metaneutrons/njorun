//! Which interface carries the Home Assistant session (plan 006, decision N8).
//!
//! The wired link has priority as soon as it has an address. Wi-Fi stays joined as the fallback.
//! A wired link that is up but has no path to Home Assistant hands over to Wi-Fi and is tried
//! again every few minutes. The selector is pure: the caller passes the time and what it sees.

use crate::network::Interface;

/// How long the carrying link may stay without a session before the other one is used.
pub const NO_SESSION_MS: u64 = 60_000;
/// How long a wired link that failed to carry a session is left alone before it is tried again.
pub const RETRY_WIRED_MS: u64 = 180_000;

/// What the tablet sees at one moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    /// The wired interface has link and an address.
    pub wired_ready: bool,
    /// The Wi-Fi interface is joined and has an address.
    pub wifi_ready: bool,
    /// A session with Home Assistant is open over the interface the selector chose last.
    pub session_open: bool,
}

/// The state of the selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selector {
    chosen: Option<Interface>,
    /// Since when the chosen interface has had no session.
    without_session_since: Option<u64>,
    /// The wired link is not used before this time.
    wired_blocked_until: Option<u64>,
}

impl Selector {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            chosen: None,
            without_session_since: None,
            wired_blocked_until: None,
        }
    }

    /// The interface that should carry the session now, or `None` when there is no usable link.
    pub fn update(&mut self, now_ms: u64, seen: Observation) -> Option<Interface> {
        // The session belongs to the interface chosen last; an open one restarts its minute.
        if seen.session_open && self.chosen.is_some() {
            self.without_session_since = None;
        }
        if !seen.wired_ready {
            // A wired link that went away starts fresh when it comes back.
            self.wired_blocked_until = None;
        }
        if self
            .wired_blocked_until
            .is_some_and(|until| now_ms >= until)
        {
            self.wired_blocked_until = None;
        }
        // Give up on a wired link that carries no session while the other one could.
        if self.chosen == Some(Interface::Ethernet)
            && seen.wired_ready
            && seen.wifi_ready
            && self
                .without_session_since
                .is_some_and(|since| now_ms.saturating_sub(since) >= NO_SESSION_MS)
        {
            self.wired_blocked_until = Some(now_ms.saturating_add(RETRY_WIRED_MS));
        }
        let wired_usable = seen.wired_ready && self.wired_blocked_until.is_none();
        let next = if wired_usable {
            Some(Interface::Ethernet)
        } else if seen.wifi_ready {
            Some(Interface::Wifi)
        } else if seen.wired_ready {
            // Nothing else to fall back to: stay on the cable.
            Some(Interface::Ethernet)
        } else {
            None
        };
        if next != self.chosen {
            // The session the caller sees belongs to the old interface, so it says nothing about
            // the new one, which starts its minute now.
            self.chosen = next;
            self.without_session_since = next.map(|_| now_ms);
        } else if self.chosen.is_some()
            && self.without_session_since.is_none()
            && !seen.session_open
        {
            self.without_session_since = Some(now_ms);
        }
        self.chosen
    }

    /// The interface chosen by the last [`update`](Self::update).
    #[must_use]
    pub const fn chosen(&self) -> Option<Interface> {
        self.chosen
    }
}
