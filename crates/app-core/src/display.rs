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
    /// Brightness a live backlight command chose (decision D6); it replaces the persistent
    /// `brightness` setting until `reset_live` or a restart.
    live_brightness: Option<u8>,
}

impl DisplayController {
    /// Start with a full idle interval before the first blanking operation.
    #[must_use]
    pub const fn new(now_ms: u64) -> Self {
        Self {
            last_activity_ms: now_ms,
            manual_off: false,
            live_brightness: None,
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
                self.live_brightness.unwrap_or(settings.brightness)
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

    /// Record an ordinary interaction without changing a manual-off state.
    pub const fn activity(&mut self, now_ms: u64) {
        self.last_activity_ms = now_ms;
    }

    /// Whether qualified local motion may wake the display. This includes an explicit
    /// manual-off because proximity wake is a local alternative to touching the panel.
    #[must_use]
    pub fn motion_wake_armed(&self, now_ms: u64, settings: &Settings) -> bool {
        let state = self.state(now_ms, settings);
        state.is_off || state.show_clock
    }

    /// Explicit remote, touch or qualified motion wake may leave manual-off.
    pub const fn wake(&mut self, now_ms: u64) {
        self.manual_off = false;
        self.activity(now_ms);
    }

    /// Explicit off remains off through brightness changes until a wake source occurs.
    pub const fn turn_off(&mut self) {
        self.manual_off = true;
    }

    /// The level the display shows when awake: a live level, else the `brightness` setting.
    #[must_use]
    pub fn level(&self, settings: &Settings) -> u8 {
        self.live_brightness.unwrap_or(settings.brightness)
    }

    /// A live backlight command (decision D6). Off is the manual-off of the blueprint; on
    /// wakes the display and, with a level, shows that level instead of the `brightness`
    /// setting. Nothing here changes a setting.
    pub const fn backlight(&mut self, on: bool, brightness: Option<u8>, now_ms: u64) {
        if on {
            self.wake(now_ms);
            if brightness.is_some() {
                self.live_brightness = brightness;
            }
        } else {
            if brightness.is_some() {
                self.live_brightness = brightness;
            }
            self.turn_off();
        }
    }

    /// Return to what the persistent settings say: a new configuration (or a restart, which
    /// starts with a new controller) ends any live backlight state.
    pub const fn reset_live(&mut self, now_ms: u64) {
        self.live_brightness = None;
        self.wake(now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::DisplayController;
    use crate::settings::Settings;

    #[test]
    fn motion_arms_for_idle_and_manual_off() {
        let settings = Settings {
            idle_timeout: 1,
            screensaver: "display_off".to_owned(),
            ..Settings::default()
        };
        let mut display = DisplayController::new(0);

        assert!(!display.motion_wake_armed(999, &settings));
        assert!(display.motion_wake_armed(1_000, &settings));
        display.activity(1_000);
        assert!(!display.motion_wake_armed(1_000, &settings));

        display.turn_off();
        assert!(display.motion_wake_armed(1_000, &settings));
        display.wake(1_000);
        assert!(!display.motion_wake_armed(1_000, &settings));
    }

    #[test]
    fn clock_idle_can_be_ended_by_motion() {
        let settings = Settings {
            idle_timeout: 1,
            screensaver: "clock".to_owned(),
            ..Settings::default()
        };
        let display = DisplayController::new(0);

        assert!(display.motion_wake_armed(1_000, &settings));
    }
}
