//! Text and icon choices for entity tiles, shared by the simulator and the device firmware.

use serde_json::Value;

use crate::{ActionFeedback, ActionKind, EntityDomain, EntityTile, Room, TargetState};

/// Icon family for an entity tile; each front end maps it to its own artwork.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Lightbulb,
    Power,
    Play,
    Thermometer,
    Sensor,
    BinarySensor,
}

#[must_use]
pub fn room_summary(room: &Room, german: bool) -> String {
    if room.entities.is_empty() {
        if german {
            "Noch keine Steuerelemente".to_owned()
        } else {
            "No controls yet".to_owned()
        }
    } else if german {
        format!(
            "{} {}",
            room.entities.len(),
            if room.entities.len() == 1 {
                "Steuerelement"
            } else {
                "Steuerelemente"
            }
        )
    } else {
        format!(
            "{} {}",
            room.entities.len(),
            if room.entities.len() == 1 {
                "control"
            } else {
                "controls"
            }
        )
    }
}

#[must_use]
pub fn state_value(tile: &EntityTile, german: bool) -> String {
    let Some(state) = &tile.state else {
        return unavailable_label(german).to_owned();
    };
    if state.state == "unavailable" || state.state == "unknown" {
        return unavailable_label(german).to_owned();
    }

    let base_value = match tile.domain {
        EntityDomain::Light | EntityDomain::Switch | EntityDomain::BinarySensor => {
            let class = (tile.domain == EntityDomain::BinarySensor)
                .then(|| state.attributes.get("device_class").and_then(Value::as_str))
                .flatten();
            let (on, off) = match (class, german) {
                (Some("door" | "window" | "opening"), true) => ("Offen", "Geschlossen"),
                (Some("door" | "window" | "opening"), false) => ("Open", "Closed"),
                (Some("motion"), true) => ("Bewegung erkannt", "Keine Bewegung"),
                (Some("motion"), false) => ("Motion detected", "No motion"),
                (Some("moisture"), true) => ("Feucht", "Trocken"),
                (Some("moisture"), false) => ("Wet", "Dry"),
                (_, true) => ("Ein", "Aus"),
                (_, false) => ("On", "Off"),
            };
            match state.state.as_str() {
                "on" => on,
                "off" => off,
                other => other,
            }
            .to_owned()
        }
        EntityDomain::Scene if tile.is_stale => {
            return if german {
                "Szene offline"
            } else {
                "Scene offline"
            }
            .to_owned();
        }
        EntityDomain::Scene => {
            if german {
                "Szene ausführen".to_owned()
            } else {
                "Run scene".to_owned()
            }
        }
        EntityDomain::Sensor => {
            let unit = state
                .attributes
                .get("unit_of_measurement")
                .and_then(Value::as_str)
                .unwrap_or("");
            if unit.is_empty() {
                state.state.clone()
            } else {
                format!("{} {}", state.state, unit)
            }
        }
    };

    let value = if tile.domain == EntityDomain::Light
        && state.state == "on"
        && let Some(brightness) = state.attributes.get("brightness").and_then(Value::as_i64)
    {
        format!(
            "{} · {}%",
            if german { "Ein" } else { "On" },
            brightness_percent(brightness)
        )
    } else {
        base_value
    };

    if tile.is_stale {
        format!(
            "{} · {}",
            if german {
                "Letzter Stand"
            } else {
                "Last known"
            },
            value
        )
    } else {
        value
    }
}

#[must_use]
pub fn feedback_value(
    feedback: Option<&ActionFeedback>,
    tile: &EntityTile,
    german: bool,
) -> Option<String> {
    match feedback? {
        ActionFeedback::AwaitingAcknowledgement { action } => {
            let target = match action {
                ActionKind::Toggle(target) => *target,
                ActionKind::RunScene => {
                    return Some(if german {
                        "Szene wird ausgeführt …".to_owned()
                    } else {
                        "Running scene…".to_owned()
                    });
                }
            };
            Some(turning_label(target, german).to_owned())
        }
        ActionFeedback::AcceptedAwaitingState { target } => {
            Some(turning_label(*target, german).to_owned())
        }
        ActionFeedback::SceneRequested => Some(if german {
            "Angefordert".to_owned()
        } else {
            "Requested".to_owned()
        }),
        ActionFeedback::Confirmed { .. } => Some(state_value(tile, german)),
        ActionFeedback::Rejected { .. } => Some(if german {
            "Aktion fehlgeschlagen".to_owned()
        } else {
            "Action failed".to_owned()
        }),
        ActionFeedback::Unconfirmed { .. } => Some(if german {
            "Nicht bestätigt".to_owned()
        } else {
            "Unconfirmed".to_owned()
        }),
    }
}

#[must_use]
pub fn detail_attributes(tile: &EntityTile, german: bool) -> String {
    let Some(state) = &tile.state else {
        return String::new();
    };
    let mut values = Vec::new();
    if let Some(brightness) = state.attributes.get("brightness").and_then(Value::as_i64) {
        let percent = brightness_percent(brightness);
        values.push(format!(
            "{}: {percent}%",
            if german { "Helligkeit" } else { "Brightness" }
        ));
    }
    if let Some(unit) = state
        .attributes
        .get("unit_of_measurement")
        .and_then(Value::as_str)
    {
        values.push(format!(
            "{}: {unit}",
            if german { "Einheit" } else { "Unit" }
        ));
    }
    values.join(" · ")
}

fn brightness_percent(brightness: i64) -> u8 {
    let percent = (brightness.clamp(0, 255) * 100 + 127) / 255;
    u8::try_from(percent).unwrap_or_default()
}

#[must_use]
pub const fn unavailable_label(german: bool) -> &'static str {
    if german {
        "Nicht verfügbar"
    } else {
        "Unavailable"
    }
}

#[must_use]
pub const fn turning_label(target: TargetState, german: bool) -> &'static str {
    match (target, german) {
        (TargetState::On, false) => "Turning on…",
        (TargetState::Off, false) => "Turning off…",
        (TargetState::On, true) => "Wird eingeschaltet…",
        (TargetState::Off, true) => "Wird ausgeschaltet…",
    }
}

#[must_use]
pub fn entity_icon(tile: &EntityTile) -> IconKind {
    match tile.icon.as_deref() {
        Some("mdi:lightbulb") => IconKind::Lightbulb,
        Some("mdi:power") => IconKind::Power,
        Some("mdi:play") => IconKind::Play,
        Some("mdi:thermometer") => IconKind::Thermometer,
        Some("mdi:checkbox-blank-circle-outline") => IconKind::BinarySensor,
        _ => match tile.domain {
            EntityDomain::Light => IconKind::Lightbulb,
            EntityDomain::Switch => IconKind::Power,
            EntityDomain::Scene => IconKind::Play,
            EntityDomain::Sensor => {
                let attributes = tile.state.as_ref().map(|state| &state.attributes);
                if attributes.is_some_and(|attrs| {
                    attrs.get("device_class").and_then(Value::as_str) == Some("temperature")
                        || matches!(
                            attrs.get("unit_of_measurement").and_then(Value::as_str),
                            Some("°C" | "°F" | "K")
                        )
                }) {
                    IconKind::Thermometer
                } else {
                    IconKind::Sensor
                }
            }
            EntityDomain::BinarySensor => IconKind::BinarySensor,
        },
    }
}

/// Earliest Unix time treated as a synchronized clock (2024-01-01 UTC).
pub const MIN_VALID_UNIX_SECONDS: i64 = 1_704_067_200;

/// Extracts the UTC offset in seconds from an ISO 8601 timestamp such as
/// `2026-11-07T02:15:33.123456+01:00` or `...Z`, as rendered by Home Assistant's `now()`.
#[must_use]
pub fn parse_utc_offset(timestamp: &str) -> Option<i32> {
    let bytes = timestamp.as_bytes();
    if bytes.len() < 20 || !timestamp.is_ascii() {
        return None;
    }
    if timestamp.ends_with('Z') {
        return Some(0);
    }
    let tail = &timestamp[timestamp.len() - 6..];
    let sign = match tail.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    if tail.as_bytes()[3] != b':' {
        return None;
    }
    let hours: i32 = tail[1..3].parse().ok()?;
    let minutes: i32 = tail[4..6].parse().ok()?;
    if hours > 14 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

/// Wall-clock label for the header: 24-hour in German, 12-hour with AM/PM otherwise.
/// Returns `--:--` until the device clock has been synchronized.
#[must_use]
pub fn local_clock_label(unix_seconds: i64, utc_offset_seconds: i32, german: bool) -> String {
    if unix_seconds < MIN_VALID_UNIX_SECONDS {
        return "--:--".to_owned();
    }
    let local = unix_seconds + i64::from(utc_offset_seconds);
    let minute_of_day = local.rem_euclid(86_400) / 60;
    let (hour, minute) = (minute_of_day / 60, minute_of_day % 60);
    if german {
        format!("{hour:02}:{minute:02}")
    } else {
        let twelve = match hour % 12 {
            0 => 12,
            other => other,
        };
        format!(
            "{twelve}:{minute:02} {}",
            if hour < 12 { "AM" } else { "PM" }
        )
    }
}

#[cfg(test)]
mod clock_tests {
    use super::{local_clock_label, parse_utc_offset};

    #[test]
    fn parses_offsets() {
        assert_eq!(
            parse_utc_offset("2026-11-07T02:15:33.123456+01:00"),
            Some(3600)
        );
        assert_eq!(parse_utc_offset("2026-07-07T02:15:33+02:00"), Some(7200));
        assert_eq!(parse_utc_offset("2026-07-07T02:15:33-05:30"), Some(-19_800));
        assert_eq!(parse_utc_offset("2026-07-07T02:15:33.5Z"), Some(0));
        assert_eq!(parse_utc_offset("2026-07-07T02:15:33"), None);
        assert_eq!(parse_utc_offset("2026-07-07T02:15:33+99:00"), None);
        assert_eq!(parse_utc_offset("garbage"), None);
    }

    #[test]
    fn formats_local_time() {
        // 2026-11-07 01:30:00 UTC is 02:30 in Berlin (winter, +01:00).
        let unix = 1_794_015_000;
        assert_eq!(local_clock_label(unix, 3600, true), "02:30");
        assert_eq!(local_clock_label(unix, 3600, false), "2:30 AM");
        assert_eq!(local_clock_label(unix + 12 * 3600, 3600, false), "2:30 PM");
        assert_eq!(local_clock_label(unix - 150 * 60, 3600, false), "12:00 AM");
        assert_eq!(local_clock_label(unix, -19_800, true), "20:00");
    }

    #[test]
    fn unsynchronized_clock_shows_placeholder() {
        assert_eq!(local_clock_label(0, 3600, true), "--:--");
        assert_eq!(local_clock_label(1_000_000, 3600, false), "--:--");
    }
}
