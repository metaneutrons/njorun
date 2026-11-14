//! Text, icon and control choices for entity tiles, shared by the simulator and the device
//! firmware.

use serde_json::Value;

use crate::{
    ActionFeedback, ActionKind, AlarmMode, EntityDomain, EntityTile, HaEntityState, LockTarget,
    PlaybackTarget, Room, TargetState,
};

/// Icon family for an entity tile; each front end maps it to its own artwork.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Lightbulb,
    Power,
    Play,
    Thermometer,
    Sensor,
    BinarySensor,
    Fan,
    Lock,
    LockOpen,
    MediaPlayer,
    Camera,
    Shield,
    Script,
    Automation,
    ToggleSwitch,
    Button,
    Number,
    Select,
    Cover,
    Climate,
    Water,
    Robot,
    Calendar,
    Weather,
    Presence,
    Timer,
    Update,
    Voice,
    Remote,
    Text,
}

/// Whether the screens use the dark theme.
///
/// `dark` always, `automatic` when the sun is down, anything else never. `daylight` is whether
/// the sun is up, as last reported by Home Assistant; before the first report `automatic` stays
/// light.
#[must_use]
pub fn dark_mode(theme: &str, daylight: Option<bool>) -> bool {
    match theme {
        "dark" => true,
        "automatic" => daylight == Some(false),
        _ => false,
    }
}

/// The six-digit pairing code in two groups of three, as it is read aloud: `360971` becomes
/// `360 971`. Anything else is returned unchanged.
#[must_use]
pub fn spaced_code(code: &str) -> String {
    if code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit()) {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_owned()
    }
}

#[must_use]
pub fn room_summary(room: &Room, german: bool) -> String {
    let lights: Vec<_> = room
        .entities
        .iter()
        .filter(|tile| tile.domain == EntityDomain::Light)
        .collect();
    let known_on = lights
        .iter()
        .filter(|tile| !tile.is_stale && tile.is_available())
        .filter(|tile| tile.state.as_ref().is_some_and(|state| state.state == "on"))
        .count();
    let all_lights_off = !lights.is_empty()
        && lights.iter().all(|tile| {
            !tile.is_stale
                && tile.is_available()
                && tile
                    .state
                    .as_ref()
                    .is_some_and(|state| state.state == "off")
        });

    let mut parts = Vec::new();
    if known_on > 0 {
        parts.push(if german {
            format!(
                "{known_on} {} an",
                if known_on == 1 { "Licht" } else { "Lichter" }
            )
        } else {
            format!(
                "{known_on} {} on",
                if known_on == 1 { "light" } else { "lights" }
            )
        });
    } else if all_lights_off {
        parts.push(if german { "Lichter aus" } else { "Lights off" }.to_owned());
    }

    if let Some(value) = room
        .entities
        .iter()
        .find_map(|tile| room_temperature(tile, german))
    {
        parts.push(value);
    }
    if !parts.is_empty() {
        return parts.join(" · ");
    }

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

fn room_temperature(tile: &EntityTile, german: bool) -> Option<String> {
    if tile.domain != EntityDomain::Sensor || tile.is_stale || !tile.is_available() {
        return None;
    }
    let state = tile.state.as_ref()?;
    let numeric = state.state.parse::<f64>().ok()?;
    if !numeric.is_finite() {
        return None;
    }
    let unit = attribute_str(tile, "unit_of_measurement");
    let temperature = attribute_str(tile, "device_class") == Some("temperature")
        || matches!(unit, Some("°C" | "°F" | "K"));
    temperature.then(|| {
        let numeric = if german {
            state.state.replace('.', ",")
        } else {
            state.state.clone()
        };
        unit.map_or_else(|| numeric.clone(), |unit| format!("{numeric} {unit}"))
    })
}

fn attribute_str<'a>(tile: &'a EntityTile, key: &str) -> Option<&'a str> {
    tile.state
        .as_ref()?
        .attributes
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn attribute_f64(tile: &EntityTile, key: &str) -> Option<f64> {
    tile.state
        .as_ref()?
        .attributes
        .get(key)
        .and_then(Value::as_f64)
}

#[allow(clippy::match_same_arms)] // Keep device-class meanings together in the translation table.
fn on_off(on: bool, class: Option<&str>, german: bool) -> &'static str {
    match (class, german, on) {
        (Some("door" | "window" | "opening"), true, true) => "Offen",
        (Some("door" | "window" | "opening"), true, false) => "Geschlossen",
        (Some("door" | "window" | "opening"), false, true) => "Open",
        (Some("door" | "window" | "opening"), false, false) => "Closed",
        (Some("motion"), true, true) => "Bewegung erkannt",
        (Some("motion"), true, false) => "Keine Bewegung",
        (Some("motion"), false, true) => "Motion detected",
        (Some("motion"), false, false) => "No motion",
        (Some("moisture"), true, true) => "Feucht",
        (Some("moisture"), true, false) => "Trocken",
        (Some("moisture"), false, true) => "Wet",
        (Some("moisture"), false, false) => "Dry",
        (
            Some("battery" | "problem" | "safety" | "tamper" | "smoke" | "gas" | "carbon_monoxide"),
            true,
            true,
        ) => "Warnung",
        (
            Some("battery" | "problem" | "safety" | "tamper" | "smoke" | "gas" | "carbon_monoxide"),
            true,
            false,
        ) => "Normal",
        (
            Some("battery" | "problem" | "safety" | "tamper" | "smoke" | "gas" | "carbon_monoxide"),
            false,
            true,
        ) => "Alert",
        (
            Some("battery" | "problem" | "safety" | "tamper" | "smoke" | "gas" | "carbon_monoxide"),
            false,
            false,
        ) => "Normal",
        (Some("connectivity" | "plug"), true, true) => "Verbunden",
        (Some("connectivity" | "plug"), true, false) => "Getrennt",
        (Some("connectivity" | "plug"), false, true) => "Connected",
        (Some("connectivity" | "plug"), false, false) => "Disconnected",
        (Some("occupancy" | "presence"), true, true) => "Anwesend",
        (Some("occupancy" | "presence"), true, false) => "Abwesend",
        (Some("occupancy" | "presence"), false, true) => "Present",
        (Some("occupancy" | "presence"), false, false) => "Away",
        (Some("lock"), true, true) => "Entriegelt",
        (Some("lock"), true, false) => "Verriegelt",
        (Some("lock"), false, true) => "Unlocked",
        (Some("lock"), false, false) => "Locked",
        (Some("battery_charging"), true, true) => "Lädt",
        (Some("battery_charging"), true, false) => "Lädt nicht",
        (Some("battery_charging"), false, true) => "Charging",
        (Some("battery_charging"), false, false) => "Not charging",
        (Some("cold"), false, true) => "Cold",
        (Some("cold"), false, false) => "Normal",
        (Some("cold"), true, true) => "Kalt",
        (Some("cold"), true, false) => "Normal",
        (Some("heat"), false, true) => "Hot",
        (Some("heat"), false, false) => "Normal",
        (Some("heat"), true, true) => "Heiß",
        (Some("heat"), true, false) => "Normal",
        (Some("light"), false, true) => "Light detected",
        (Some("light"), false, false) => "Dark",
        (Some("light"), true, true) => "Hell",
        (Some("light"), true, false) => "Dunkel",
        (Some("moving"), false, true) => "Moving",
        (Some("moving"), false, false) => "Stopped",
        (Some("moving"), true, true) => "In Bewegung",
        (Some("moving"), true, false) => "Stillstand",
        (Some("power"), false, true) => "Power detected",
        (Some("power"), false, false) => "No power",
        (Some("power"), true, true) => "Strom vorhanden",
        (Some("power"), true, false) => "Kein Strom",
        (Some("running"), false, true) => "Running",
        (Some("running"), false, false) => "Stopped",
        (Some("running"), true, true) => "Läuft",
        (Some("running"), true, false) => "Gestoppt",
        (Some("sound"), false, true) => "Sound detected",
        (Some("sound"), false, false) => "Quiet",
        (Some("sound"), true, true) => "Geräusch erkannt",
        (Some("sound"), true, false) => "Ruhig",
        (Some("update"), false, true) => "Update available",
        (Some("update"), false, false) => "Up to date",
        (Some("update"), true, true) => "Update verfügbar",
        (Some("update"), true, false) => "Aktuell",
        (Some("vibration"), false, true) => "Vibration detected",
        (Some("vibration"), false, false) => "No vibration",
        (Some("vibration"), true, true) => "Vibration erkannt",
        (Some("vibration"), true, false) => "Keine Vibration",
        (_, true, true) => "Ein",
        (_, true, false) => "Aus",
        (_, false, true) => "On",
        (_, false, false) => "Off",
    }
}

fn lock_label(state: &str, german: bool) -> Option<&'static str> {
    Some(match (state, german) {
        ("locked", false) => "Locked",
        ("locked", true) => "Verriegelt",
        ("unlocked", false) => "Unlocked",
        ("unlocked", true) => "Entriegelt",
        ("locking", false) => "Locking…",
        ("locking", true) => "Wird verriegelt …",
        ("unlocking", false) => "Unlocking…",
        ("unlocking", true) => "Wird entriegelt …",
        ("jammed", false) => "Jammed",
        ("jammed", true) => "Blockiert",
        ("open", false) => "Open",
        ("open", true) => "Offen",
        ("opening", false) => "Opening…",
        ("opening", true) => "Wird geöffnet …",
        _ => return None,
    })
}

fn alarm_label(state: &str, german: bool) -> Option<&'static str> {
    Some(match (state, german) {
        ("disarmed", false) => "Disarmed",
        ("disarmed", true) => "Deaktiviert",
        ("armed_home", false) => "Armed home",
        ("armed_home", true) => "Zuhause scharf",
        ("armed_away", false) => "Armed away",
        ("armed_away", true) => "Abwesend scharf",
        ("armed_night", false) => "Armed night",
        ("armed_night", true) => "Nacht scharf",
        ("armed_vacation", false) => "Armed vacation",
        ("armed_vacation", true) => "Urlaub scharf",
        ("armed_custom_bypass", false) => "Armed custom",
        ("armed_custom_bypass", true) => "Benutzerdefiniert scharf",
        ("arming", false) => "Arming…",
        ("arming", true) => "Wird scharf geschaltet …",
        ("disarming", false) => "Disarming…",
        ("disarming", true) => "Wird deaktiviert …",
        ("pending", false) => "Pending",
        ("pending", true) => "Wartet",
        ("triggered", false) => "Triggered!",
        ("triggered", true) => "Ausgelöst!",
        _ => return None,
    })
}

fn media_text(tile: &EntityTile, playback: &str, german: bool) -> String {
    let heading = attribute_str(tile, "media_title");
    let text =
        |english: &str, german_text: &str| if german { german_text } else { english }.to_owned();
    match playback {
        "playing" => match (heading, attribute_str(tile, "media_artist")) {
            (Some(heading), Some(artist)) => format!("{artist} – {heading}"),
            (Some(heading), None) => heading.to_owned(),
            _ => text("Playing", "Wiedergabe"),
        },
        "paused" => heading.map_or_else(
            || text("Paused", "Pausiert"),
            |heading| format!("{} · {heading}", text("Paused", "Pausiert")),
        ),
        "idle" | "on" => text("Ready", "Bereit"),
        "standby" => text("Standby", "Bereitschaft"),
        "buffering" => text("Buffering…", "Puffert …"),
        "off" => text("Off", "Aus"),
        other => other.to_owned(),
    }
}

fn common_state(state: &str, german: bool) -> String {
    let (en, de) = match state {
        "on" => ("On", "Ein"),
        "off" => ("Off", "Aus"),
        "open" => ("Open", "Offen"),
        "closed" => ("Closed", "Geschlossen"),
        "opening" => ("Opening…", "Wird geöffnet …"),
        "closing" => ("Closing…", "Wird geschlossen …"),
        "heat" => ("Heating", "Heizen"),
        "cool" => ("Cooling", "Kühlen"),
        "heat_cool" => ("Heat / cool", "Heizen / Kühlen"),
        "auto" => ("Automatic", "Automatisch"),
        "dry" => ("Drying", "Entfeuchten"),
        "fan_only" => ("Ventilation", "Lüften"),
        "idle" => ("Ready", "Bereit"),
        "active" => ("Active", "Aktiv"),
        "paused" => ("Paused", "Pausiert"),
        "cleaning" => ("Cleaning", "Reinigung"),
        "docked" => ("Docked", "In der Station"),
        "returning" => ("Returning", "Rückkehr"),
        "mowing" => ("Mowing", "Mäht"),
        "error" => ("Error", "Fehler"),
        "sunny" => ("Sunny", "Sonnig"),
        "cloudy" => ("Cloudy", "Bewölkt"),
        "partlycloudy" => ("Partly cloudy", "Leicht bewölkt"),
        "rainy" => ("Rain", "Regen"),
        "pouring" => ("Heavy rain", "Starkregen"),
        "snowy" => ("Snow", "Schnee"),
        "clear-night" => ("Clear night", "Klare Nacht"),
        "fog" => ("Fog", "Nebel"),
        "above_horizon" => ("Daylight", "Tag"),
        "below_horizon" => ("Night", "Nacht"),
        _ => return state.replace('_', " "),
    };
    if german { de } else { en }.into()
}

/// The base text for a tile; `None` for a stale scene, which reads "Scene offline".
fn domain_text(tile: &EntityTile, state: &HaEntityState, german: bool) -> Option<String> {
    let on = state.state == "on";
    Some(match tile.domain {
        EntityDomain::Light
        | EntityDomain::Switch
        | EntityDomain::BinarySensor
        | EntityDomain::Fan
        | EntityDomain::InputBoolean => {
            let class = (tile.domain == EntityDomain::BinarySensor)
                .then(|| attribute_str(tile, "device_class"))
                .flatten();
            match state.state.as_str() {
                "on" | "off" => on_off(on, class, german).to_owned(),
                other => other.to_owned(),
            }
        }
        EntityDomain::Scene if tile.is_stale => return None,
        EntityDomain::Scene => if german {
            "Szene ausführen"
        } else {
            "Run scene"
        }
        .to_owned(),
        EntityDomain::Script if state.state == "on" => {
            if german { "Läuft" } else { "Running" }.to_owned()
        }
        EntityDomain::Script => if german {
            "Skript ausführen"
        } else {
            "Run script"
        }
        .to_owned(),
        EntityDomain::Button | EntityDomain::InputButton => {
            if german { "Drücken" } else { "Press" }.to_owned()
        }
        EntityDomain::Automation => match (state.state.as_str(), german) {
            ("on", false) => "Enabled".to_owned(),
            ("on", true) => "Aktiv".to_owned(),
            ("off", false) => "Disabled".to_owned(),
            ("off", true) => "Inaktiv".to_owned(),
            (other, _) => other.to_owned(),
        },
        EntityDomain::Lock => {
            lock_label(&state.state, german).map_or_else(|| state.state.clone(), str::to_owned)
        }
        EntityDomain::AlarmControlPanel => {
            alarm_label(&state.state, german).map_or_else(|| state.state.clone(), str::to_owned)
        }
        EntityDomain::MediaPlayer => media_text(tile, &state.state, german),
        EntityDomain::Camera => if german { "Kamera" } else { "Camera" }.to_owned(),
        EntityDomain::Sensor | EntityDomain::Number | EntityDomain::InputNumber => {
            let unit = attribute_str(tile, "unit_of_measurement").unwrap_or("");
            let mut value = state.state.clone();
            if let (Ok(number), Some(precision)) = (
                value.parse::<f64>(),
                state
                    .attributes
                    .get("display_precision")
                    .or_else(|| state.attributes.get("suggested_display_precision"))
                    .and_then(Value::as_u64),
            ) && number.is_finite()
                && precision <= 12
            {
                value = format!("{:.*}", usize::try_from(precision).unwrap_or(12), number);
            }
            if german && value.parse::<f64>().is_ok() {
                value = value.replace('.', ",");
            }
            if attribute_str(tile, "device_class") == Some("enum") {
                value = value.replace('_', " ");
            }
            if unit.is_empty() {
                value
            } else {
                format!("{value} {unit}")
            }
        }
        EntityDomain::Select | EntityDomain::InputSelect => state.state.clone(),
        EntityDomain::Person | EntityDomain::DeviceTracker => match state.state.as_str() {
            "home" => if german { "Zuhause" } else { "Home" }.into(),
            "not_home" => if german { "Abwesend" } else { "Away" }.into(),
            other => other.replace('_', " "),
        },
        EntityDomain::Update => if state.state == "on" {
            if german {
                "Update verfügbar"
            } else {
                "Update available"
            }
        } else {
            if german { "Aktuell" } else { "Up to date" }
        }
        .into(),
        EntityDomain::Event => attribute_str(tile, "event_type").map_or_else(
            || if german { "Ereignis" } else { "Event" }.into(),
            |event| event.replace('_', " "),
        ),
        _ => common_state(&state.state, german),
    })
}

#[must_use]
pub fn state_value(tile: &EntityTile, german: bool) -> String {
    let Some(state) = &tile.state else {
        return unavailable_label(german).to_owned();
    };
    if !tile.is_available() {
        return if state.state == "unknown" {
            if german { "Unbekannt" } else { "Unknown" }
        } else {
            unavailable_label(german)
        }
        .to_owned();
    }
    let on = state.state == "on";

    let Some(base_value) = domain_text(tile, state, german) else {
        return if german {
            "Szene offline"
        } else {
            "Scene offline"
        }
        .to_owned();
    };

    let value = if tile.domain == EntityDomain::Light
        && on
        && let Some(brightness) = state.attributes.get("brightness").and_then(Value::as_i64)
    {
        format!(
            "{} · {}%",
            if german { "Ein" } else { "On" },
            brightness_percent(brightness)
        )
    } else if tile.domain == EntityDomain::Fan
        && on
        && let Some(percentage) = attribute_f64(tile, "percentage")
    {
        format!(
            "{} · {}%",
            if german { "Ein" } else { "On" },
            percentage.round()
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

/// State context for a details sheet. A slider renders its numeric value itself, so repeating
/// the same value in the state line would invert the hierarchy and waste space.
#[must_use]
pub fn detail_value(tile: &EntityTile, german: bool) -> String {
    let value = state_value(tile, german);
    match detail_controls(tile) {
        Control::Slider(_) if matches!(tile.domain, EntityDomain::Light | EntityDomain::Fan) => {
            value
                .rsplit_once(" · ")
                .filter(|(_, suffix)| {
                    suffix
                        .strip_suffix('%')
                        .is_some_and(|number| number.parse::<f64>().is_ok())
                })
                .map_or_else(|| value.clone(), |(status, _)| status.to_owned())
        }
        Control::Slider(_) => String::new(),
        _ => value,
    }
}

/// Text for an in-flight action, such as "Turning on…".
#[must_use]
pub fn pending_label(action: &ActionKind, tile: &EntityTile, german: bool) -> String {
    let text = match (action, german) {
        (ActionKind::Toggle(target), _) => return turning_label(*target, german).to_owned(),
        (ActionKind::RunScene, false) => match tile.domain {
            EntityDomain::Scene => "Running scene…",
            EntityDomain::Script => "Running script…",
            _ => "Working…",
        },
        (ActionKind::RunScene, true) => match tile.domain {
            EntityDomain::Scene => "Szene wird ausgeführt …",
            EntityDomain::Script => "Skript läuft …",
            _ => "Wird ausgeführt …",
        },
        (ActionKind::Lock(LockTarget::Locked), false) => "Locking…",
        (ActionKind::Lock(LockTarget::Locked), true) => "Wird verriegelt …",
        (ActionKind::Lock(LockTarget::Unlocked), false) => "Unlocking…",
        (ActionKind::Lock(LockTarget::Unlocked), true) => "Wird entriegelt …",
        (ActionKind::Alarm(state), false) if state == "disarmed" => "Disarming…",
        (ActionKind::Alarm(state), true) if state == "disarmed" => "Wird deaktiviert …",
        (ActionKind::Alarm(_), false) => "Arming…",
        (ActionKind::Alarm(_), true) => "Wird scharf geschaltet …",
        (ActionKind::Playback(PlaybackTarget::Playing), false) => "Starting…",
        (ActionKind::Playback(PlaybackTarget::Playing), true) => "Wird gestartet …",
        (ActionKind::Playback(PlaybackTarget::Paused), false) => "Pausing…",
        (ActionKind::Playback(PlaybackTarget::Paused), true) => "Wird pausiert …",
        (_, false) => "Setting…",
        (_, true) => "Wird gesetzt …",
    };
    text.to_owned()
}

#[must_use]
pub fn feedback_value(
    feedback: Option<&ActionFeedback>,
    tile: &EntityTile,
    german: bool,
) -> Option<String> {
    match feedback? {
        ActionFeedback::AwaitingAcknowledgement { action }
        | ActionFeedback::AcceptedAwaitingConfirmation { action } => {
            Some(pending_label(action, tile, german))
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

/// Whether the tile should look switched on or active.
#[must_use]
#[allow(clippy::match_same_arms)] // Group state semantics by domain for auditing.
pub fn tile_active(tile: &EntityTile) -> bool {
    if tile.is_stale {
        return false;
    }
    let state = tile.state_value();
    match tile.domain {
        EntityDomain::Light
        | EntityDomain::Switch
        | EntityDomain::BinarySensor
        | EntityDomain::Fan
        | EntityDomain::InputBoolean
        | EntityDomain::Automation
        | EntityDomain::Script => state == Some("on"),
        EntityDomain::MediaPlayer => state == Some("playing"),
        EntityDomain::Climate | EntityDomain::WaterHeater => {
            state.is_some_and(|state| !matches!(state, "off" | "unknown" | "unavailable"))
        }
        EntityDomain::Humidifier
        | EntityDomain::Siren
        | EntityDomain::Remote
        | EntityDomain::Group => state == Some("on"),
        EntityDomain::Cover | EntityDomain::Valve => matches!(state, Some("open" | "opening")),
        EntityDomain::Vacuum => state == Some("cleaning"),
        EntityDomain::LawnMower => state == Some("mowing"),
        EntityDomain::Person | EntityDomain::DeviceTracker => state == Some("home"),
        EntityDomain::Timer => state == Some("active"),
        EntityDomain::Lock => state == Some("unlocked") || state == Some("open"),
        EntityDomain::AlarmControlPanel => {
            state.is_some_and(|state| state.starts_with("armed") || state == "triggered")
        }
        _ => false,
    }
}

#[must_use]
pub fn detail_attributes(tile: &EntityTile, german: bool) -> String {
    let Some(state) = &tile.state else {
        return String::new();
    };
    let mut values = Vec::new();
    // Brightness, fan percentage and units already appear in the primary value or their
    // controls. This block is only for genuinely supplementary information.
    if let Some(battery) = state
        .attributes
        .get("battery_level")
        .and_then(Value::as_f64)
    {
        let percent = battery.clamp(0.0, 100.0).round();
        values.push(format!(
            "{}: {percent}%",
            if german { "Batterie" } else { "Battery" }
        ));
    }
    for (key, english, deutsch) in [
        (
            "current_temperature",
            "Current temperature",
            "Aktuelle Temperatur",
        ),
        ("current_humidity", "Humidity", "Luftfeuchtigkeit"),
        ("temperature", "Temperature", "Temperatur"),
        ("humidity", "Humidity", "Luftfeuchtigkeit"),
        ("pressure", "Pressure", "Luftdruck"),
        ("wind_speed", "Wind speed", "Windgeschwindigkeit"),
        ("wind_bearing", "Wind direction", "Windrichtung"),
        ("hvac_action", "Activity", "Aktivität"),
        ("current_position", "Position", "Position"),
        ("current_tilt_position", "Tilt", "Neigung"),
        ("cleaning_area", "Cleaned area", "Gereinigte Fläche"),
        ("message", "Event", "Termin"),
        ("start_time", "Starts", "Beginn"),
        ("end_time", "Ends", "Ende"),
        ("location", "Location", "Ort"),
        ("event_type", "Last event", "Letztes Ereignis"),
        ("source_type", "Location source", "Standortquelle"),
        ("latitude", "Latitude", "Breitengrad"),
        ("longitude", "Longitude", "Längengrad"),
        ("gps_accuracy", "Accuracy (m)", "Genauigkeit (m)"),
        ("current", "Running instances", "Laufende Instanzen"),
        ("last_triggered", "Last run", "Letzter Lauf"),
        ("next_event", "Next event", "Nächstes Ereignis"),
        ("duration", "Duration", "Dauer"),
        ("finishes_at", "Finishes", "Endet"),
        ("installed_version", "Installed", "Installiert"),
        ("latest_version", "Available version", "Verfügbare Version"),
        ("in_progress", "Update progress", "Updatefortschritt"),
        ("elevation", "Elevation", "Sonnenhöhe"),
        ("next_rising", "Sunrise", "Sonnenaufgang"),
        ("next_setting", "Sunset", "Sonnenuntergang"),
        (
            "air_quality_index",
            "Air quality index",
            "Luftqualitätsindex",
        ),
        ("particulate_matter_2_5", "PM2.5", "PM2,5"),
        ("carbon_dioxide", "CO₂", "CO₂"),
    ] {
        if let Some(value) = state
            .attributes
            .get(key)
            .filter(|value| !value.is_null() && !value.is_object() && !value.is_array())
        {
            let value = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned);
            values.push(format!(
                "{}: {}",
                if german { deutsch } else { english },
                value
            ));
        }
    }
    let explanation = crate::entity_extensions::explanation(tile.domain, german);
    if !explanation.is_empty() {
        values.push(explanation);
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
#[allow(clippy::match_same_arms)] // Each domain has an explicit, reviewable icon mapping.
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
            EntityDomain::Fan => IconKind::Fan,
            EntityDomain::Lock => {
                if matches!(tile.state_value(), Some("unlocked" | "open")) {
                    IconKind::LockOpen
                } else {
                    IconKind::Lock
                }
            }
            EntityDomain::MediaPlayer => IconKind::MediaPlayer,
            EntityDomain::Camera => IconKind::Camera,
            EntityDomain::AlarmControlPanel => IconKind::Shield,
            EntityDomain::Script => IconKind::Script,
            EntityDomain::Automation => IconKind::Automation,
            EntityDomain::InputBoolean => IconKind::ToggleSwitch,
            EntityDomain::Button | EntityDomain::InputButton => IconKind::Button,
            EntityDomain::Number | EntityDomain::InputNumber => IconKind::Number,
            EntityDomain::Select | EntityDomain::InputSelect => IconKind::Select,
            EntityDomain::Climate => IconKind::Climate,
            EntityDomain::WaterHeater => IconKind::Water,
            EntityDomain::Weather => IconKind::Weather,
            EntityDomain::Image | EntityDomain::ImageProcessing => IconKind::Camera,
            EntityDomain::Siren => IconKind::Shield,
            EntityDomain::Humidifier | EntityDomain::Valve => IconKind::Water,
            EntityDomain::Cover => IconKind::Cover,
            EntityDomain::Vacuum | EntityDomain::LawnMower => IconKind::Robot,
            EntityDomain::Calendar
            | EntityDomain::Date
            | EntityDomain::Datetime
            | EntityDomain::InputDatetime => IconKind::Calendar,
            EntityDomain::DeviceTracker
            | EntityDomain::Person
            | EntityDomain::Zone
            | EntityDomain::GeoLocation => IconKind::Presence,
            EntityDomain::Timer | EntityDomain::Time | EntityDomain::Schedule => IconKind::Timer,
            EntityDomain::Update => IconKind::Update,
            EntityDomain::AssistSatellite
            | EntityDomain::Conversation
            | EntityDomain::Stt
            | EntityDomain::Tts
            | EntityDomain::WakeWord => IconKind::Voice,
            EntityDomain::Remote | EntityDomain::Infrared | EntityDomain::RadioFrequency => {
                IconKind::Remote
            }
            EntityDomain::Text | EntityDomain::InputText | EntityDomain::Notify => IconKind::Text,
            EntityDomain::Counter => IconKind::Number,
            _ => IconKind::Sensor,
        },
    }
}

/// A numeric control in the details sheet.
#[derive(Debug, Clone, PartialEq)]
pub struct Slider {
    /// Action key the front end reports back: `brightness`, `fan`, `volume` or `number`.
    pub action: &'static str,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub value: f64,
    pub unit: String,
}

/// Controls the details sheet offers for an entity.
#[derive(Debug, Clone, PartialEq)]
pub enum Control {
    None,
    Slider(Slider),
    Options {
        options: Vec<String>,
        selected: Option<String>,
    },
    Lock {
        locked: bool,
    },
    Media {
        playing: bool,
        volume: Option<Slider>,
    },
    Alarm {
        modes: Vec<AlarmMode>,
        disarmed: bool,
        /// A numeric code is needed to arm or disarm.
        needs_code: bool,
    },
    Camera,
    Automation,
}

fn light_is_dimmable(tile: &EntityTile) -> bool {
    let Some(state) = &tile.state else {
        return false;
    };
    state
        .attributes
        .get("supported_color_modes")
        .and_then(Value::as_array)
        .map_or_else(
            || state.attributes.contains_key("brightness"),
            |modes| {
                modes
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|mode| mode != "onoff" && mode != "unknown")
            },
        )
}

/// Whether a light offers a brightness control.
#[must_use]
pub fn light_supports_brightness(tile: &EntityTile) -> bool {
    tile.domain == EntityDomain::Light && light_is_dimmable(tile)
}

const fn percent(value: f64) -> f64 {
    value.clamp(0.0, 100.0).round()
}

fn percent_slider(action: &'static str, value: f64) -> Slider {
    Slider {
        action,
        min: if action == "brightness" { 1.0 } else { 0.0 },
        max: 100.0,
        step: 1.0,
        value: percent(value),
        unit: "%".to_owned(),
    }
}

fn number_control(tile: &EntityTile, state: &HaEntityState) -> Control {
    let min = attribute_f64(tile, "min").unwrap_or(0.0);
    let max = attribute_f64(tile, "max").unwrap_or(100.0);
    if max <= min {
        return Control::None;
    }
    Control::Slider(Slider {
        action: "number",
        min,
        max,
        step: attribute_f64(tile, "step")
            .filter(|step| *step > 0.0)
            .unwrap_or(1.0),
        value: state.state.parse().unwrap_or(min).clamp(min, max),
        unit: attribute_str(tile, "unit_of_measurement")
            .unwrap_or("")
            .to_owned(),
    })
}

fn alarm_control(tile: &EntityTile, state: &HaEntityState) -> Control {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let features = attribute_f64(tile, "supported_features").map(|bits| bits as u64);
    let modes = AlarmMode::ALL
        .into_iter()
        .filter(|mode| {
            features.map_or(
                matches!(mode, AlarmMode::Home | AlarmMode::Away | AlarmMode::Night),
                |bits| bits & mode.feature_bit() != 0,
            )
        })
        .collect();
    let needs_code = state
        .attributes
        .get("code_format")
        .is_some_and(|format| !format.is_null());
    Control::Alarm {
        modes,
        disarmed: state.state == "disarmed",
        needs_code,
    }
}

fn light_control(state: &HaEntityState) -> Control {
    let value = if state.state == "on" {
        state
            .attributes
            .get("brightness")
            .and_then(Value::as_i64)
            .map_or(100.0, |brightness| {
                f64::from(brightness_percent(brightness))
            })
    } else {
        0.0
    };
    Control::Slider(percent_slider("brightness", value))
}

/// The controls to show for this tile; `Control::None` when a tap already does everything.
#[must_use]
pub fn detail_controls(tile: &EntityTile) -> Control {
    let Some(state) = &tile.state else {
        return Control::None;
    };
    if !tile.is_available() {
        return Control::None;
    }
    match tile.domain {
        EntityDomain::Light if light_is_dimmable(tile) => light_control(state),
        EntityDomain::Fan => attribute_f64(tile, "percentage").map_or(Control::None, |value| {
            let mut slider = percent_slider("fan", value);
            slider.step = attribute_f64(tile, "percentage_step")
                .unwrap_or(1.0)
                .max(1.0);
            Control::Slider(slider)
        }),
        EntityDomain::Number | EntityDomain::InputNumber => number_control(tile, state),
        EntityDomain::Select | EntityDomain::InputSelect => Control::Options {
            options: state
                .attributes
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            selected: Some(state.state.clone()),
        },
        EntityDomain::Lock => Control::Lock {
            locked: state.state == "locked",
        },
        EntityDomain::MediaPlayer => Control::Media {
            playing: state.state == "playing",
            volume: attribute_f64(tile, "volume_level")
                .map(|level| percent_slider("volume", level * 100.0)),
        },
        EntityDomain::AlarmControlPanel => alarm_control(tile, state),
        EntityDomain::Camera => Control::Camera,
        EntityDomain::Automation => Control::Automation,
        _ => Control::None,
    }
}

/// Everything a front end needs to draw the details-sheet controls.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // A flat view model that maps one-to-one onto Slint.
pub struct DetailView {
    /// `none`, `slider`, `options`, `lock`, `media`, `alarm`, `camera` or `automation`.
    pub control: &'static str,
    pub slider: Option<Slider>,
    pub options: Vec<String>,
    pub selected_option: String,
    pub locked: bool,
    pub playing: bool,
    pub has_volume: bool,
    pub needs_code: bool,
    /// Alarm buttons as `(action key, label)`.
    pub actions: Vec<(String, String)>,
}

/// Label for an alarm action key.
#[must_use]
pub fn action_label(key: &str, german: bool) -> &'static str {
    match (key, german) {
        ("arm_home", false) => "Arm home",
        ("arm_home", true) => "Zuhause scharf",
        ("arm_away", false) => "Arm away",
        ("arm_away", true) => "Abwesend scharf",
        ("arm_night", false) => "Arm night",
        ("arm_night", true) => "Nacht scharf",
        ("arm_vacation", false) => "Arm vacation",
        ("arm_vacation", true) => "Urlaub scharf",
        ("arm_custom_bypass", false) => "Arm custom",
        ("arm_custom_bypass", true) => "Benutzerdefiniert scharf",
        ("disarm", false) => "Disarm",
        ("disarm", true) => "Deaktivieren",
        _ => "",
    }
}

/// The details-sheet controls for a tile in a form both front ends can map one-to-one.
#[must_use]
pub fn detail_view(tile: &EntityTile, german: bool) -> DetailView {
    let mut view = DetailView {
        control: "none",
        slider: None,
        options: Vec::new(),
        selected_option: String::new(),
        locked: false,
        playing: false,
        has_volume: false,
        needs_code: false,
        actions: Vec::new(),
    };
    match detail_controls(tile) {
        Control::None => {}
        Control::Slider(slider) => {
            view.control = "slider";
            view.slider = Some(slider);
        }
        Control::Options { options, selected } => {
            view.control = "options";
            view.options = options;
            view.selected_option = selected.unwrap_or_default();
        }
        Control::Lock { locked } => {
            view.control = "lock";
            view.locked = locked;
        }
        Control::Media { playing, volume } => {
            view.control = "media";
            view.playing = playing;
            view.has_volume = volume.is_some();
            view.slider = volume;
        }
        Control::Alarm {
            modes,
            disarmed,
            needs_code,
        } => {
            view.control = "alarm";
            view.needs_code = needs_code;
            if disarmed {
                view.actions = modes
                    .into_iter()
                    .map(|mode| {
                        (
                            mode.key().to_owned(),
                            action_label(mode.key(), german).to_owned(),
                        )
                    })
                    .collect();
            } else {
                view.actions = vec![(
                    "disarm".to_owned(),
                    action_label("disarm", german).to_owned(),
                )];
            }
        }
        Control::Camera => view.control = "camera",
        Control::Automation => view.control = "automation",
    }
    view
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

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of [`days_from_civil`].
const fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

/// UTC RFC 3339 timestamp for a Unix second count.
#[must_use]
pub fn unix_timestamp(unix: i64) -> String {
    let (year, month, day) = civil_from_days(unix.div_euclid(86_400));
    let second = unix.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second / 3600,
        second % 3600 / 60,
        second % 60
    )
}

/// Unix seconds of an ISO 8601 timestamp, honouring its own UTC offset.
fn parse_unix_seconds(timestamp: &str) -> Option<i64> {
    if !timestamp.is_ascii() || timestamp.len() < 19 {
        return None;
    }
    let number = |range: std::ops::Range<usize>| timestamp.get(range)?.parse::<i64>().ok();
    let bytes = timestamp.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' || !matches!(bytes[10], b'T' | b' ') {
        return None;
    }
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    let offset = if timestamp.len() > 19 {
        parse_utc_offset(timestamp).unwrap_or(0)
    } else {
        0
    };
    Some(
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
            - i64::from(offset),
    )
}

/// A time stamp for the details sheet in the panel's local time, such as `07.11.2026 · 02:30`
/// in German or `Nov 7, 2026 · 2:30 AM` otherwise. Falls back to the original text.
#[must_use]
pub fn format_local_timestamp(timestamp: &str, utc_offset_seconds: i32, german: bool) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let Some(unix) = parse_unix_seconds(timestamp) else {
        return timestamp.to_owned();
    };
    let local = unix + i64::from(utc_offset_seconds);
    let (year, month, day) = civil_from_days(local.div_euclid(86_400));
    let clock = local_clock_label(
        local - i64::from(utc_offset_seconds),
        utc_offset_seconds,
        german,
    );
    if german {
        format!("{day:02}.{month:02}.{year} · {clock}")
    } else {
        let name = MONTHS[usize::try_from(month - 1).unwrap_or(0).min(11)];
        format!("{name} {day}, {year} · {clock}")
    }
}

#[cfg(test)]
mod timestamp_tests {
    use super::format_local_timestamp;

    #[test]
    fn converts_utc_to_local_time() {
        let utc = "2026-11-07T01:30:00.123456+00:00";
        assert_eq!(
            format_local_timestamp(utc, 3600, true),
            "07.11.2026 · 02:30"
        );
        assert_eq!(
            format_local_timestamp(utc, 3600, false),
            "Nov 7, 2026 · 2:30 AM"
        );
        assert_eq!(
            format_local_timestamp(utc, -19_800, true),
            "06.11.2026 · 20:00"
        );
    }

    #[test]
    fn rolls_over_midnight_month_and_year() {
        assert_eq!(
            format_local_timestamp("2026-12-31T23:30:00+00:00", 7200, true),
            "01.01.2027 · 01:30"
        );
        assert_eq!(
            format_local_timestamp("2028-02-29T23:59:59Z", 60, true),
            "01.03.2028 · 00:00"
        );
    }

    #[test]
    fn honours_the_timestamps_own_offset_and_bad_input() {
        assert_eq!(
            format_local_timestamp("2026-07-07T10:00:00+02:00", 7200, true),
            "07.07.2026 · 10:00"
        );
        assert_eq!(format_local_timestamp("not a time", 0, true), "not a time");
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
