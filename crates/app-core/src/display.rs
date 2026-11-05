//! Hardware-independent display timeout and wake policy.
//!
//! A frontend supplies monotonic milliseconds and translates the returned level
//! into its display adapter. This does not assert physical backlight control.

use crate::settings::Settings;

/// Requested display output, independent of a GPIO or window implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayState {
    pub is_off: bool,
    pub brightness: u8,
    pub show_clock: bool,
}

/// Whether a touchscreen interaction may reach the dashboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchDisposition {
    /// The interaction wakes the screen and must not activate a control.
    WakeOnly,
    /// The screen was awake; ordinary gesture processing may continue.
    Interactive,
}

/// Tracks local activity without storing wall-clock timestamps or credentials.
#[derive(Clone, Debug)]
pub struct DisplayController {
    last_activity_ms: u64,
    manual_off: bool,
}

impl DisplayController {
    /// Start with a full idle interval before the first blanking operation.
    #[must_use]
    pub const fn new(now_ms: u64) -> Self {
        Self {
            last_activity_ms: now_ms,
            manual_off: false,
        }
    }

    /// Evaluate validated settings against a monotonic clock. A clock regression
    /// fails awake instead of treating unsigned underflow as a long idle period.
    #[must_use]
    pub fn state(&self, now_ms: u64, settings: &Settings) -> DisplayState {
        let elapsed = now_ms.saturating_sub(self.last_activity_ms);
        let timed_out = elapsed >= u64::from(settings.idle_timeout) * 1000;
        let is_off = self.manual_off || (settings.screensaver == "display_off" && timed_out);
        let show_clock = !self.manual_off && settings.screensaver == "clock" && timed_out;
        DisplayState {
            is_off,
            show_clock,
            brightness: if is_off {
                0
            } else if show_clock {
                settings.idle_brightness
            } else {
                settings.brightness
            },
        }
    }

    /// Consume the first touch after timeout; a second touch may activate a tile.
    #[must_use]
    pub fn touch(&mut self, now_ms: u64, settings: &Settings) -> TouchDisposition {
        let previous = self.state(now_ms, settings);
        self.wake(now_ms);
        if previous.is_off || previous.show_clock {
            TouchDisposition::WakeOnly
        } else {
            TouchDisposition::Interactive
        }
    }

    /// Record an ordinary interaction or qualified motion. Motion must not
    /// override manual-off; callers decide whether the wake source is enabled.
    pub const fn activity(&mut self, now_ms: u64) {
        self.last_activity_ms = now_ms;
    }

    /// Explicit remote or touch wake may leave manual-off.
    pub const fn wake(&mut self, now_ms: u64) {
        self.manual_off = false;
        self.activity(now_ms);
    }

    /// Explicit off remains off through motion and brightness changes.
    pub const fn turn_off(&mut self) {
        self.manual_off = true;
    }
}
