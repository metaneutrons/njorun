//! Capability-aware controls and bounded Home Assistant action planning for entities.
//!
//! This module deliberately exposes a small, typed surface to the frontends. A key is
//! useful only when it appears in `controls` for the current entity snapshot; arguments
//! are checked again here before a service call is built.

use serde_json::{Map, Value, json};

use crate::controls::{
    ControlKind, ControlSpec, InputKind, InputSpec, attribute, feature, options, text,
};
use crate::{ActionError, ActionKind, EntityDomain as D, EntityTile, ServiceCall};

const LIGHT_EFFECT: u64 = 4;
const LIGHT_FLASH: u64 = 8;
const LIGHT_TRANSITION: u64 = 32;
const FAN_SET_SPEED: u64 = 1;
const FAN_OSCILLATE: u64 = 2;
const FAN_DIRECTION: u64 = 4;
const FAN_PRESET: u64 = 8;
const FAN_TURN_OFF: u64 = 16;
const FAN_TURN_ON: u64 = 32;
const CLIMATE_TARGET_TEMP: u64 = 1;
const CLIMATE_TARGET_RANGE: u64 = 2;
const CLIMATE_TARGET_HUMIDITY: u64 = 4;
const CLIMATE_FAN_MODE: u64 = 8;
const CLIMATE_PRESET: u64 = 16;
const CLIMATE_SWING: u64 = 32;
const CLIMATE_TURN_ON: u64 = 256;
const CLIMATE_TURN_OFF: u64 = 128;
const CLIMATE_SWING_HORIZONTAL: u64 = 512;
const COVER_OPEN: u64 = 1;
const COVER_CLOSE: u64 = 2;
const COVER_SET_POSITION: u64 = 4;
const COVER_STOP: u64 = 8;
const COVER_OPEN_TILT: u64 = 16;
const COVER_CLOSE_TILT: u64 = 32;
const COVER_STOP_TILT: u64 = 64;
const COVER_SET_TILT: u64 = 128;
const COVER_SPEED: u64 = 256;
const HUMIDIFIER_MODES: u64 = 1;
const WATER_HEATER_TARGET_TEMP: u64 = 1;
const WATER_HEATER_OPERATION_MODE: u64 = 2;
const WATER_HEATER_AWAY_MODE: u64 = 4;
const WATER_HEATER_ON_OFF: u64 = 8;
const VALVE_OPEN: u64 = 1;
const VALVE_CLOSE: u64 = 2;
const VALVE_SET_POSITION: u64 = 4;
const VALVE_STOP: u64 = 8;
const SIREN_TURN_ON: u64 = 1;
const SIREN_TURN_OFF: u64 = 2;
const SIREN_TONES: u64 = 4;
const SIREN_VOLUME: u64 = 8;
const SIREN_DURATION: u64 = 16;
const LAWN_START: u64 = 1;
const LAWN_PAUSE: u64 = 2;
const LAWN_DOCK: u64 = 4;
const LAWN_STOP: u64 = 8;
const VACUUM_PAUSE: u64 = 4;
const VACUUM_STOP: u64 = 8;
const VACUUM_RETURN_HOME: u64 = 16;
const VACUUM_FAN_SPEED: u64 = 32;
const VACUUM_LOCATE: u64 = 512;
const VACUUM_CLEAN_SPOT: u64 = 1024;
const VACUUM_START: u64 = 8192;
const VACUUM_CLEAN_AREA: u64 = 16_384;
const UPDATE_INSTALL: u64 = 1;
const UPDATE_SPECIFIC_VERSION: u64 = 2;
const UPDATE_BACKUP: u64 = 8;
const UPDATE_RELEASE_NOTES: u64 = 16;
const WEATHER_DAILY: u64 = 1;
const WEATHER_HOURLY: u64 = 2;
const WEATHER_TWICE_DAILY: u64 = 4;
const TODO_CREATE: u64 = 1;
const TODO_DELETE: u64 = 2;
const TODO_UPDATE: u64 = 4;
const TODO_MOVE: u64 = 8;
const TODO_DUE_DATE: u64 = 16;
const TODO_DUE_DATETIME: u64 = 32;
const TODO_DESCRIPTION: u64 = 64;
const NOTIFY_TITLE: u64 = 1;
const REMOTE_LEARN: u64 = 1;
const REMOTE_DELETE: u64 = 2;
const REMOTE_ACTIVITY: u64 = 4;
const CAMERA_ON_OFF: u64 = 1;
const CAMERA_STREAM: u64 = 2;
const ALARM_HOME: u64 = 1;
const ALARM_AWAY: u64 = 2;
const ALARM_NIGHT: u64 = 4;
const ALARM_TRIGGER: u64 = 8;
const ALARM_CUSTOM_BYPASS: u64 = 16;
const ALARM_VACATION: u64 = 32;
const CALENDAR_CREATE: u64 = 1;
const CALENDAR_DELETE: u64 = 2;
const CALENDAR_UPDATE: u64 = 4;

fn available(tile: &EntityTile) -> bool {
    tile.command_enabled && !tile.is_stale && tile.is_available()
}

fn label(german: bool, en: &str, de: &str) -> String {
    text(german, en, de)
}

fn localized_button(key: &'static str, german: bool, en: &str, de: &str) -> ControlSpec {
    let mut control = ControlSpec::button(key, &label(german, en, de));
    if matches!(key, "turn_on" | "turn_off" | "toggle") {
        control.icon = "power";
    }
    control
}

fn push_power_controls(
    tile: &EntityTile,
    german: bool,
    out: &mut Vec<ControlSpec>,
    allow_on: bool,
    allow_off: bool,
) {
    let (show_on, show_off) = match tile.state_value() {
        Some("on") => (false, allow_off),
        Some("off") => (allow_on, false),
        _ => (allow_on, allow_off),
    };
    if show_on {
        out.push(localized_button(
            "turn_on",
            german,
            "Turn on",
            "Einschalten",
        ));
    }
    if show_off {
        out.push(localized_button(
            "turn_off",
            german,
            "Turn off",
            "Ausschalten",
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn slider(
    key: &'static str,
    german: bool,
    en: &str,
    de: &str,
    value: f64,
    minimum: f64,
    maximum: f64,
    step: f64,
    unit: &str,
) -> ControlSpec {
    ControlSpec {
        key,
        label: label(german, en, de),
        kind: ControlKind::Slider,
        value: json!(value),
        options: Vec::new(),
        minimum,
        maximum,
        step,
        unit: unit.to_owned(),
        inputs: Vec::new(),
        enabled: true,
        dangerous: false,
        icon: "",
        group: "",
    }
}

fn select(
    key: &'static str,
    german: bool,
    en: &str,
    de: &str,
    values: Vec<String>,
    current: Option<&Value>,
) -> ControlSpec {
    let mut control = localized_button(key, german, en, de);
    control.kind = ControlKind::Select;
    control.options = values;
    control.value = current.cloned().unwrap_or(Value::Null);
    control
}

fn toggle(key: &'static str, german: bool, en: &str, de: &str, value: bool) -> ControlSpec {
    let mut control = localized_button(key, german, en, de);
    control.kind = ControlKind::Toggle;
    control.value = json!(value);
    control
}

fn form(
    key: &'static str,
    german: bool,
    en: &str,
    de: &str,
    inputs: Vec<InputSpec>,
) -> ControlSpec {
    let mut control = localized_button(key, german, en, de);
    control.kind = ControlKind::Form;
    control.inputs = inputs;
    control
}

#[allow(clippy::too_many_arguments)]
fn number_input(
    key: &'static str,
    german: bool,
    en: &str,
    de: &str,
    value: f64,
    minimum: f64,
    maximum: f64,
    step: f64,
    unit: &str,
    required: bool,
) -> InputSpec {
    let input_label = if unit.is_empty() {
        label(german, en, de)
    } else {
        format!("{} ({unit})", label(german, en, de))
    };
    InputSpec {
        key,
        label: input_label,
        kind: InputKind::Number,
        value: json!(value),
        options: Vec::new(),
        minimum,
        maximum,
        step,
        required,
    }
}

fn select_input(
    key: &'static str,
    german: bool,
    en: &str,
    de: &str,
    values: Vec<String>,
) -> InputSpec {
    InputSpec {
        key,
        label: label(german, en, de),
        kind: InputKind::Select,
        value: Value::Null,
        options: values,
        minimum: 0.0,
        maximum: 100.0,
        step: 1.0,
        required: true,
    }
}

fn numeric_attr(tile: &EntityTile, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| attribute(tile, key).and_then(Value::as_f64))
        .filter(|value| value.is_finite())
}

fn bool_attr(tile: &EntityTile, key: &str) -> bool {
    attribute(tile, key)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn str_attr<'a>(tile: &'a EntityTile, key: &str) -> Option<&'a str> {
    attribute(tile, key).and_then(Value::as_str)
}

fn string_options(tile: &EntityTile, key: &str) -> Vec<String> {
    options(tile, key)
}

fn feature_list(tile: &EntityTile, key: &str) -> Vec<String> {
    string_options(tile, key)
}

fn has_feature_mask(tile: &EntityTile) -> bool {
    attribute(tile, "supported_features")
        .and_then(Value::as_u64)
        .is_some()
}

fn range(
    tile: &EntityTile,
    min_key: &str,
    max_key: &str,
    default_min: f64,
    default_max: f64,
) -> (f64, f64) {
    let min = numeric_attr(tile, &[min_key]).unwrap_or(default_min);
    let max = numeric_attr(tile, &[max_key]).unwrap_or(default_max);
    if min.is_finite() && max.is_finite() && min <= max {
        (min, max)
    } else {
        (default_min, default_max)
    }
}

// The light catalog is one data-driven feature table spanning all HA color modes.
#[allow(clippy::too_many_lines)]
fn controls_light(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    push_power_controls(tile, german, out, true, true);
    let modes = string_options(tile, "supported_color_modes");
    let has_brightness = modes.iter().any(|mode| mode != "onoff")
        || (modes.is_empty() && attribute(tile, "brightness").is_some());
    if has_brightness {
        let value = numeric_attr(tile, &["brightness"]).unwrap_or(255.0) * 100.0 / 255.0;
        out.push(slider(
            "brightness",
            german,
            "Brightness",
            "Helligkeit",
            value,
            1.0,
            100.0,
            1.0,
            "%",
        ));
        out.push(form(
            "brightness_step_pct",
            german,
            "Change brightness",
            "Helligkeit ändern",
            vec![number_input(
                "step",
                german,
                "Relative change",
                "Relative Änderung",
                10.0,
                -100.0,
                100.0,
                1.0,
                "%",
                true,
            )],
        ));
        out.push(form(
            "brightness_step",
            german,
            "Change raw brightness",
            "Rohhelligkeit ändern",
            vec![number_input(
                "step",
                german,
                "Relative change",
                "Relative Änderung",
                25.0,
                -225.0,
                255.0,
                1.0,
                "",
                true,
            )],
        ));
    }
    if modes.iter().any(|mode| mode == "color_temp")
        || numeric_attr(tile, &["min_color_temp_kelvin"]).is_some()
    {
        let (minimum, maximum) = range(
            tile,
            "min_color_temp_kelvin",
            "max_color_temp_kelvin",
            2000.0,
            6500.0,
        );
        let current = numeric_attr(tile, &["color_temp_kelvin"]).unwrap_or(minimum);
        out.push(slider(
            "color_temp",
            german,
            "Color temperature",
            "Farbtemperatur",
            current,
            minimum,
            maximum,
            1.0,
            "K",
        ));
    }
    if modes
        .iter()
        .any(|mode| matches!(mode.as_str(), "rgb" | "rgbw" | "rgbww"))
    {
        out.push(form(
            "rgb_color",
            german,
            "Set RGB color",
            "RGB-Farbe setzen",
            vec![
                number_input("red", german, "Red", "Rot", 0.0, 0.0, 255.0, 1.0, "", true),
                number_input(
                    "green", german, "Green", "Grün", 0.0, 0.0, 255.0, 1.0, "", true,
                ),
                number_input(
                    "blue", german, "Blue", "Blau", 0.0, 0.0, 255.0, 1.0, "", true,
                ),
            ],
        ));
    }
    if modes
        .iter()
        .any(|mode| matches!(mode.as_str(), "hs" | "xy" | "rgb" | "rgbw" | "rgbww"))
    {
        out.push(form(
            "color_name",
            german,
            "Set named color",
            "Benannte Farbe setzen",
            vec![InputSpec::text(
                "color_name",
                &label(german, "CSS color name", "CSS-Farbname"),
                true,
            )],
        ));
    }
    if modes.iter().any(|mode| mode == "hs") {
        out.push(form(
            "hs_color",
            german,
            "Set hue and saturation",
            "Farbton und Sättigung setzen",
            vec![
                number_input(
                    "hue", german, "Hue", "Farbton", 0.0, 0.0, 360.0, 1.0, "°", true,
                ),
                number_input(
                    "saturation",
                    german,
                    "Saturation",
                    "Sättigung",
                    100.0,
                    0.0,
                    100.0,
                    1.0,
                    "%",
                    true,
                ),
            ],
        ));
    }
    if modes.iter().any(|mode| mode == "xy") {
        out.push(form(
            "xy_color",
            german,
            "Set XY color",
            "XY-Farbe setzen",
            vec![
                number_input("x", german, "X", "X", 0.0, 0.0, 1.0, 0.001, "", true),
                number_input("y", german, "Y", "Y", 0.0, 0.0, 1.0, 0.001, "", true),
            ],
        ));
    }
    if modes.iter().any(|mode| mode == "rgbw") {
        out.push(form(
            "rgbw_color",
            german,
            "Set RGBW color",
            "RGBW-Farbe setzen",
            vec![
                number_input("red", german, "Red", "Rot", 0.0, 0.0, 255.0, 1.0, "", true),
                number_input(
                    "green", german, "Green", "Grün", 0.0, 0.0, 255.0, 1.0, "", true,
                ),
                number_input(
                    "blue", german, "Blue", "Blau", 0.0, 0.0, 255.0, 1.0, "", true,
                ),
                number_input(
                    "white", german, "White", "Weiß", 0.0, 0.0, 255.0, 1.0, "", true,
                ),
            ],
        ));
    }
    if modes.iter().any(|mode| mode == "rgbww") {
        out.push(form(
            "rgbww_color",
            german,
            "Set RGBWW color",
            "RGBWW-Farbe setzen",
            vec![
                number_input("red", german, "Red", "Rot", 0.0, 0.0, 255.0, 1.0, "", true),
                number_input(
                    "green", german, "Green", "Grün", 0.0, 0.0, 255.0, 1.0, "", true,
                ),
                number_input(
                    "blue", german, "Blue", "Blau", 0.0, 0.0, 255.0, 1.0, "", true,
                ),
                number_input(
                    "cold_white",
                    german,
                    "Cold white",
                    "Kaltweiß",
                    0.0,
                    0.0,
                    255.0,
                    1.0,
                    "",
                    true,
                ),
                number_input(
                    "warm_white",
                    german,
                    "Warm white",
                    "Warmweiß",
                    0.0,
                    0.0,
                    255.0,
                    1.0,
                    "",
                    true,
                ),
            ],
        ));
    }
    if modes.iter().any(|mode| mode == "white") {
        out.push(slider(
            "white",
            german,
            "White brightness",
            "Weißhelligkeit",
            255.0,
            0.0,
            255.0,
            1.0,
            "",
        ));
    }
    let effects = string_options(tile, "effect_list");
    if !effects.is_empty() && feature(tile, LIGHT_EFFECT) {
        out.push(select(
            "effect",
            german,
            "Effect",
            "Effekt",
            effects,
            attribute(tile, "effect"),
        ));
    }
    if feature(tile, LIGHT_TRANSITION) {
        out.push(slider(
            "transition",
            german,
            "Transition",
            "Übergangszeit",
            0.0,
            0.0,
            300.0,
            1.0,
            "s",
        ));
    }
    if feature(tile, LIGHT_FLASH) {
        out.push(select(
            "flash",
            german,
            "Flash",
            "Blinken",
            vec!["short".into(), "long".into()],
            None,
        ));
    }
}

fn controls_fan(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    let legacy_power = !has_feature_mask(tile);
    push_power_controls(
        tile,
        german,
        out,
        legacy_power || feature(tile, FAN_TURN_ON),
        legacy_power || feature(tile, FAN_TURN_OFF),
    );
    if feature(tile, FAN_SET_SPEED)
        || (!has_feature_mask(tile) && numeric_attr(tile, &["percentage"]).is_some())
    {
        out.push(slider(
            "percentage",
            german,
            "Speed",
            "Geschwindigkeit",
            numeric_attr(tile, &["percentage"]).unwrap_or(0.0),
            0.0,
            100.0,
            1.0,
            "%",
        ));
        for (key, en, de) in [
            (
                "increase_speed",
                "Increase speed",
                "Geschwindigkeit erhöhen",
            ),
            (
                "decrease_speed",
                "Decrease speed",
                "Geschwindigkeit verringern",
            ),
        ] {
            out.push(form(
                key,
                german,
                en,
                de,
                vec![number_input(
                    "percentage_step",
                    german,
                    "Step (0 uses the entity default)",
                    "Schritt (0 nutzt den Entity-Standard)",
                    0.0,
                    0.0,
                    100.0,
                    1.0,
                    "%",
                    false,
                )],
            ));
        }
    }
    let presets = string_options(tile, "preset_modes");
    if !presets.is_empty()
        && (feature(tile, FAN_PRESET)
            || (!has_feature_mask(tile) && attribute(tile, "preset_modes").is_some()))
    {
        out.push(select(
            "preset_mode",
            german,
            "Preset",
            "Voreinstellung",
            presets,
            attribute(tile, "preset_mode"),
        ));
    }
    if feature(tile, FAN_DIRECTION) {
        out.push(select(
            "direction",
            german,
            "Direction",
            "Richtung",
            vec!["forward".into(), "reverse".into()],
            attribute(tile, "direction"),
        ));
    }
    if feature(tile, FAN_OSCILLATE) {
        out.push(toggle(
            "oscillate",
            german,
            "Oscillate",
            "Oszillation",
            bool_attr(tile, "oscillating"),
        ));
    }
}

fn controls_lock(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    let transient = matches!(tile.state_value(), Some("locking" | "unlocking"));
    let with_code = attribute(tile, "code_format").is_some_and(|value| !value.is_null());
    let code_input = || InputSpec {
        key: "code",
        label: label(german, "Code", "Code"),
        kind: InputKind::Password,
        value: Value::Null,
        options: vec![],
        minimum: 0.0,
        maximum: 32.0,
        step: 1.0,
        required: true,
    };
    let mut lock = if with_code {
        form("lock", german, "Lock", "Verriegeln", vec![code_input()])
    } else {
        localized_button("lock", german, "Lock", "Verriegeln")
    };
    let mut unlock = if with_code {
        form("unlock", german, "Unlock", "Entriegeln", vec![code_input()])
    } else {
        localized_button("unlock", german, "Unlock", "Entriegeln")
    };
    lock.enabled = !transient && tile.state_value() != Some("locked");
    unlock.enabled = !transient && tile.state_value() != Some("unlocked");
    out.extend([lock, unlock]);
    if feature(tile, 1) {
        out.push(if with_code {
            form(
                "open",
                german,
                "Open latch",
                "Falle öffnen",
                vec![code_input()],
            )
        } else {
            localized_button("open", german, "Open latch", "Falle öffnen")
        });
    }
}

fn alarm_code_input(german: bool, required: bool) -> InputSpec {
    InputSpec {
        key: "code",
        label: label(german, "Code", "Code"),
        kind: InputKind::Password,
        value: Value::Null,
        options: vec![],
        minimum: 0.0,
        maximum: 32.0,
        step: 1.0,
        required,
    }
}

fn controls_alarm(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    let code_format = attribute(tile, "code_format").is_some_and(|value| !value.is_null());
    let arm_required = bool_attr(tile, "code_arm_required");
    for (key, service, en, de, bit) in [
        (
            "arm_home",
            "alarm_arm_home",
            "Arm home",
            "Intern scharf",
            ALARM_HOME,
        ),
        (
            "arm_away",
            "alarm_arm_away",
            "Arm away",
            "Extern scharf",
            ALARM_AWAY,
        ),
        (
            "arm_night",
            "alarm_arm_night",
            "Arm night",
            "Nachtmodus",
            ALARM_NIGHT,
        ),
        (
            "arm_vacation",
            "alarm_arm_vacation",
            "Arm vacation",
            "Urlaubsmodus",
            ALARM_VACATION,
        ),
        (
            "arm_custom_bypass",
            "alarm_arm_custom_bypass",
            "Arm custom bypass",
            "Benutzerdefiniert scharf",
            ALARM_CUSTOM_BYPASS,
        ),
    ] {
        if feature(tile, bit) {
            let mut control = if arm_required {
                form(key, german, en, de, vec![alarm_code_input(german, true)])
            } else if code_format {
                form(key, german, en, de, vec![alarm_code_input(german, false)])
            } else {
                localized_button(key, german, en, de)
            };
            control.dangerous = true;
            control.group = "alarm";
            let _ = service;
            out.push(control);
        }
    }
    let mut disarm = if code_format {
        form(
            "disarm",
            german,
            "Disarm",
            "Unscharf",
            vec![alarm_code_input(german, true)],
        )
    } else {
        localized_button("disarm", german, "Disarm", "Unscharf")
    };
    disarm.dangerous = true;
    disarm.group = "alarm";
    out.push(disarm);
    if feature(tile, ALARM_TRIGGER) {
        let mut trigger = localized_button("trigger", german, "Trigger alarm", "Alarm auslösen");
        trigger.dangerous = true;
        trigger.group = "alarm";
        out.push(trigger);
    }
}

fn controls_number(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    let min = numeric_attr(tile, &["min"]).unwrap_or(0.0);
    let max = numeric_attr(tile, &["max"]).unwrap_or(100.0);
    let step = numeric_attr(tile, &["step"])
        .unwrap_or(1.0)
        .abs()
        .max(f64::EPSILON);
    let current = tile
        .state_value()
        .and_then(|state| state.parse::<f64>().ok())
        .unwrap_or(min);
    out.push(slider(
        "set_value",
        german,
        "Set value",
        "Wert setzen",
        current,
        min,
        max,
        step,
        str_attr(tile, "unit_of_measurement").unwrap_or(""),
    ));
    out.push(form(
        "set_value_exact",
        german,
        "Enter exact value",
        "Exakten Wert eingeben",
        vec![number_input(
            "value",
            german,
            "Value",
            "Wert",
            current,
            min,
            max,
            step,
            str_attr(tile, "unit_of_measurement").unwrap_or(""),
            true,
        )],
    ));
}

fn push_input_number_step_controls(german: bool, out: &mut Vec<ControlSpec>) {
    out.push(localized_button(
        "increment",
        german,
        "Increment",
        "Erhöhen",
    ));
    out.push(localized_button(
        "decrement",
        german,
        "Decrement",
        "Verringern",
    ));
}

fn weather_forecast_options(tile: &EntityTile) -> Vec<String> {
    let mut values = Vec::new();
    if feature(tile, WEATHER_DAILY) {
        values.push("daily".into());
    }
    if feature(tile, WEATHER_HOURLY) {
        values.push("hourly".into());
    }
    if feature(tile, WEATHER_TWICE_DAILY) {
        values.push("twice_daily".into());
    }
    values
}

fn todo_inputs(
    tile: &EntityTile,
    german: bool,
    item_required: bool,
    include_status: bool,
) -> Vec<InputSpec> {
    let mut inputs = vec![InputSpec::text(
        "item",
        &label(german, "Item name or UID", "Eintrag oder UID"),
        item_required,
    )];
    if include_status {
        let mut status = select_input(
            "status",
            german,
            "Status",
            "Status",
            vec!["needs_action".into(), "completed".into()],
        );
        status.required = false;
        inputs.push(status);
        inputs.push(InputSpec::text(
            "rename",
            &label(german, "New name", "Neuer Name"),
            false,
        ));
    }
    if feature(tile, TODO_DUE_DATE) {
        inputs.push(InputSpec::text(
            "due_date",
            &label(german, "Due date (YYYY-MM-DD)", "Fällig am (JJJJ-MM-TT)"),
            false,
        ));
    }
    if feature(tile, TODO_DUE_DATETIME) {
        inputs.push(InputSpec::text(
            "due_datetime",
            &label(german, "Due date and time", "Fällig am"),
            false,
        ));
    }
    if feature(tile, TODO_DESCRIPTION) {
        inputs.push(InputSpec::text(
            "description",
            &label(german, "Description", "Beschreibung"),
            false,
        ));
    }
    inputs
}

fn controls_camera(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    if feature(tile, CAMERA_ON_OFF) {
        push_power_controls(tile, german, out, true, true);
    }
    if attribute(tile, "motion_detection").is_some() {
        if bool_attr(tile, "motion_detection") {
            out.push(localized_button(
                "disable_motion_detection",
                german,
                "Disable motion detection",
                "Bewegungserkennung deaktivieren",
            ));
        } else {
            out.push(localized_button(
                "enable_motion_detection",
                german,
                "Enable motion detection",
                "Bewegungserkennung aktivieren",
            ));
        }
    }
    out.push(form(
        "snapshot",
        german,
        "Take snapshot",
        "Schnappschuss aufnehmen",
        vec![InputSpec::text(
            "filename",
            &label(german, "File path (.jpg)", "Dateipfad (.jpg)"),
            true,
        )],
    ));
    if feature(tile, CAMERA_STREAM) {
        let mut media_player = InputSpec::text(
            "media_player",
            &label(german, "Media player", "Media-Player"),
            true,
        );
        media_player.kind = InputKind::Entity;
        media_player.options = vec!["media_player".into()];
        out.push(form(
            "play_stream",
            german,
            "Play stream",
            "Stream abspielen",
            vec![media_player],
        ));
        out.push(form(
            "record",
            german,
            "Record clip",
            "Clip aufnehmen",
            vec![
                InputSpec::text(
                    "filename",
                    &label(german, "File path (.mp4)", "Dateipfad (.mp4)"),
                    true,
                ),
                number_input(
                    "duration", german, "Duration", "Dauer", 30.0, 1.0, 3600.0, 1.0, "s", false,
                ),
                number_input(
                    "lookback",
                    german,
                    "Lookback",
                    "Rückblick",
                    0.0,
                    0.0,
                    300.0,
                    1.0,
                    "s",
                    false,
                ),
            ],
        ));
    }
}

// Remote command forms share one catalog because the available buttons depend on the entity's feature mask.
#[allow(clippy::too_many_lines)]
fn controls_remote(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    push_power_controls(tile, german, out, true, true);
    out.push(localized_button("toggle", german, "Toggle", "Umschalten"));
    if feature(tile, REMOTE_ACTIVITY) {
        let activities = string_options(tile, "activity_list");
        if !activities.is_empty() {
            out.push(select(
                "activity",
                german,
                "Start activity",
                "Aktivität starten",
                activities,
                attribute(tile, "current_activity"),
            ));
        }
    }
    out.push(form(
        "send_command",
        german,
        "Send command",
        "Befehl senden",
        vec![
            InputSpec::text(
                "command",
                &label(
                    german,
                    "Commands (separated by semicolons)",
                    "Befehle (durch Semikola getrennt)",
                ),
                true,
            ),
            InputSpec::text(
                "device",
                &label(german, "Device (optional)", "Gerät (optional)"),
                false,
            ),
            number_input(
                "num_repeats",
                german,
                "Repeats",
                "Wiederholungen",
                1.0,
                0.0,
                255.0,
                1.0,
                "",
                false,
            ),
            number_input(
                "delay_secs",
                german,
                "Delay",
                "Verzögerung",
                0.4,
                0.0,
                60.0,
                0.1,
                "s",
                false,
            ),
            number_input(
                "hold_secs",
                german,
                "Hold",
                "Haltedauer",
                0.0,
                0.0,
                60.0,
                0.1,
                "s",
                false,
            ),
        ],
    ));
    if feature(tile, REMOTE_LEARN) {
        let mut inputs = vec![
            InputSpec::text(
                "device",
                &label(german, "Device (optional)", "Gerät (optional)"),
                false,
            ),
            InputSpec::text(
                "command",
                &label(
                    german,
                    "Commands (separated by semicolons)",
                    "Befehle (durch Semikola getrennt)",
                ),
                false,
            ),
            select_input(
                "command_type",
                german,
                "Command type",
                "Befehlstyp",
                vec!["ir".into(), "rf".into()],
            ),
            number_input(
                "timeout",
                german,
                "Timeout",
                "Zeitlimit",
                30.0,
                0.0,
                60.0,
                5.0,
                "s",
                false,
            ),
        ];
        inputs.push(InputSpec {
            key: "alternative",
            label: label(german, "Alternative code", "Alternativcode"),
            kind: InputKind::Toggle,
            value: json!(false),
            options: vec![],
            minimum: 0.0,
            maximum: 1.0,
            step: 1.0,
            required: false,
        });
        inputs
            .iter_mut()
            .find(|input| input.key == "command_type")
            .expect("inserted")
            .required = false;
        out.push(form(
            "learn_command",
            german,
            "Learn command",
            "Befehl lernen",
            inputs,
        ));
    }
    if feature(tile, REMOTE_DELETE) {
        out.push(form(
            "delete_command",
            german,
            "Delete command",
            "Befehl löschen",
            vec![
                InputSpec::text(
                    "device",
                    &label(german, "Device (optional)", "Gerät (optional)"),
                    false,
                ),
                InputSpec::text(
                    "command",
                    &label(
                        german,
                        "Commands (separated by semicolons)",
                        "Befehle (durch Semikola getrennt)",
                    ),
                    true,
                ),
            ],
        ));
    }
}

const SCRIPT_INPUT_KEYS: [&str; 16] = [
    "field_0", "field_1", "field_2", "field_3", "field_4", "field_5", "field_6", "field_7",
    "field_8", "field_9", "field_10", "field_11", "field_12", "field_13", "field_14", "field_15",
];

#[derive(Debug, Clone)]
struct ScriptField {
    name: String,
    input: InputSpec,
}

// Convert the bounded set of HA selector schemas into the UI's typed input descriptors.
#[allow(clippy::too_many_lines)]
fn script_fields(tile: &EntityTile, german: bool) -> (Vec<ScriptField>, bool, bool) {
    let Some(fields) = attribute(tile, "njorun_script_fields").and_then(Value::as_object) else {
        return (Vec::new(), false, false);
    };
    let mut fields: Vec<_> = fields.iter().collect();
    fields.sort_by(|left, right| left.0.cmp(right.0));
    let mut output = Vec::new();
    let mut unsupported_required = false;
    for (index, (name, definition)) in fields.into_iter().enumerate() {
        let required = definition
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if index >= SCRIPT_INPUT_KEYS.len() {
            unsupported_required |= required;
            continue;
        }
        let key = SCRIPT_INPUT_KEYS[index];
        let selector = definition.get("selector").and_then(Value::as_object);
        let display_name = definition
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(name);
        let mut input = match selector
            .and_then(|selector| selector.iter().next())
            .map(|(key, value)| (key.as_str(), value))
        {
            Some(("text", config)) => {
                let mut input = InputSpec::text(key, display_name, required);
                let maximum = config
                    .get("max")
                    .and_then(Value::as_u64)
                    .unwrap_or(255)
                    .clamp(1, 4096);
                input.maximum = f64::from(u32::try_from(maximum).unwrap_or(4096));
                input
            }
            Some(("number", config)) => {
                let minimum = config
                    .get("min")
                    .and_then(Value::as_f64)
                    .unwrap_or(-1_000_000.0);
                let maximum = config
                    .get("max")
                    .and_then(Value::as_f64)
                    .unwrap_or(1_000_000.0);
                let step = config
                    .get("step")
                    .and_then(Value::as_f64)
                    .unwrap_or(1.0)
                    .abs()
                    .max(f64::EPSILON);
                number_input(
                    key,
                    german,
                    display_name,
                    display_name,
                    config
                        .get("default")
                        .and_then(Value::as_f64)
                        .unwrap_or(minimum),
                    minimum,
                    maximum,
                    step,
                    "",
                    required,
                )
            }
            Some(("boolean", _)) => InputSpec {
                key,
                label: display_name.to_owned(),
                kind: InputKind::Toggle,
                value: Value::Bool(false),
                options: vec![],
                minimum: 0.0,
                maximum: 1.0,
                step: 1.0,
                required,
            },
            Some(("select", config)) => {
                let values = config
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| {
                                value.as_str().map(str::to_owned).or_else(|| {
                                    value
                                        .get("value")
                                        .and_then(Value::as_str)
                                        .map(str::to_owned)
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if values.is_empty() {
                    unsupported_required |= required;
                    continue;
                }
                let mut input =
                    select_input(key, german, display_name, display_name, values.clone());
                input.required = required;
                input.options = values;
                input
            }
            Some(("entity", config)) => {
                let domains = config
                    .get("domain")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .or_else(|| {
                        config
                            .get("domain")
                            .and_then(Value::as_str)
                            .map(|domain| vec![domain.to_owned()])
                    })
                    .unwrap_or_default();
                InputSpec {
                    key,
                    label: display_name.to_owned(),
                    kind: InputKind::Entity,
                    value: Value::Null,
                    options: domains,
                    minimum: 0.0,
                    maximum: 100.0,
                    step: 1.0,
                    required,
                }
            }
            _ => {
                unsupported_required |= required;
                continue;
            }
        };
        if let Some(default) = definition.get("default") {
            input.value = default.clone();
        }
        output.push(ScriptField {
            name: name.clone(),
            input,
        });
    }
    (output, unsupported_required, true)
}

fn controls_select(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    let choices = string_options(tile, "options");
    if !choices.is_empty() {
        let current = tile
            .state
            .as_ref()
            .map(|state| Value::String(state.state.clone()));
        let mut control = select(
            "select_option",
            german,
            "Select option",
            "Option auswählen",
            choices,
            current.as_ref(),
        );
        control.enabled = true;
        out.push(control);
        out.push(localized_button(
            "select_first",
            german,
            "First",
            "Erste Option",
        ));
        out.push(localized_button(
            "select_last",
            german,
            "Last",
            "Letzte Option",
        ));
        for (key, en, de) in [
            ("select_next", "Next", "Nächste Option"),
            ("select_previous", "Previous", "Vorherige Option"),
        ] {
            out.push(form(
                key,
                german,
                en,
                de,
                vec![InputSpec {
                    key: "cycle",
                    label: label(german, "Wrap around", "Am Ende fortsetzen"),
                    kind: InputKind::Toggle,
                    value: json!(true),
                    options: vec![],
                    minimum: 0.0,
                    maximum: 1.0,
                    step: 1.0,
                    required: false,
                }],
            ));
        }
    }
}

// The HVAC feature table selects several independent climate controls.
#[allow(clippy::too_many_lines)]
fn controls_climate(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    let modes = string_options(tile, "hvac_modes");
    if !modes.is_empty() {
        let current = tile
            .state
            .as_ref()
            .map(|state| Value::String(state.state.clone()));
        out.push(select(
            "hvac_mode",
            german,
            "HVAC mode",
            "Betriebsart",
            modes,
            current.as_ref(),
        ));
    }
    push_power_controls(
        tile,
        german,
        out,
        feature(tile, CLIMATE_TURN_ON),
        feature(tile, CLIMATE_TURN_OFF),
    );
    let min = numeric_attr(tile, &["min_temp"]).unwrap_or(7.0);
    let max = numeric_attr(tile, &["max_temp"]).unwrap_or(35.0);
    let step = numeric_attr(tile, &["target_temp_step", "precision"])
        .unwrap_or(0.5)
        .abs()
        .max(0.1);
    let unit = str_attr(tile, "temperature_unit").unwrap_or("");
    if feature(tile, CLIMATE_TARGET_TEMP) {
        let target = numeric_attr(tile, &["temperature"]).unwrap_or(min);
        out.push(form(
            "set_temperature",
            german,
            "Set temperature",
            "Temperatur setzen",
            vec![number_input(
                "temperature",
                german,
                "Temperature",
                "Temperatur",
                target,
                min,
                max,
                step,
                unit,
                true,
            )],
        ));
    }
    if feature(tile, CLIMATE_TARGET_RANGE) {
        let low = numeric_attr(tile, &["target_temp_low"]).unwrap_or(min);
        let high = numeric_attr(tile, &["target_temp_high"]).unwrap_or(max);
        out.push(form(
            "set_temperature_range",
            german,
            "Set temperature range",
            "Temperaturbereich setzen",
            vec![
                number_input(
                    "target_temp_low",
                    german,
                    "Lower temperature",
                    "Untere Temperatur",
                    low,
                    min,
                    max,
                    step,
                    unit,
                    true,
                ),
                number_input(
                    "target_temp_high",
                    german,
                    "Upper temperature",
                    "Obere Temperatur",
                    high,
                    min,
                    max,
                    step,
                    unit,
                    true,
                ),
            ],
        ));
    }
    if feature(tile, CLIMATE_TARGET_HUMIDITY) {
        let (min_h, max_h) = range(tile, "min_humidity", "max_humidity", 0.0, 100.0);
        out.push(slider(
            "set_humidity",
            german,
            "Target humidity",
            "Ziel-Luftfeuchtigkeit",
            numeric_attr(tile, &["humidity", "target_humidity"]).unwrap_or(min_h),
            min_h,
            max_h,
            numeric_attr(tile, &["target_humidity_step"])
                .unwrap_or(1.0)
                .max(1.0),
            "%",
        ));
    }
    for (bit, attr, key, en, de, service) in [
        (
            CLIMATE_PRESET,
            "preset_modes",
            "preset_mode",
            "Preset",
            "Voreinstellung",
            "preset_mode",
        ),
        (
            CLIMATE_FAN_MODE,
            "fan_modes",
            "fan_mode",
            "Fan mode",
            "Lüftermodus",
            "fan_mode",
        ),
        (
            CLIMATE_SWING,
            "swing_modes",
            "swing_mode",
            "Swing mode",
            "Schwenkmodus",
            "swing_mode",
        ),
        (
            CLIMATE_SWING_HORIZONTAL,
            "swing_horizontal_modes",
            "swing_horizontal_mode",
            "Horizontal swing",
            "Horizontales Schwenken",
            "swing_horizontal_mode",
        ),
    ] {
        let values = string_options(tile, attr);
        if feature(tile, bit) && !values.is_empty() {
            out.push(select(key, german, en, de, values, attribute(tile, key)));
            let _ = service;
        }
    }
}

fn controls_cover(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    for (bit, key, en, de) in [
        (COVER_OPEN, "open", "Open", "Öffnen"),
        (COVER_CLOSE, "close", "Close", "Schließen"),
        (COVER_STOP, "stop", "Stop", "Anhalten"),
        (COVER_OPEN_TILT, "open_tilt", "Open tilt", "Lamellen öffnen"),
        (
            COVER_CLOSE_TILT,
            "close_tilt",
            "Close tilt",
            "Lamellen schließen",
        ),
        (
            COVER_STOP_TILT,
            "stop_tilt",
            "Stop tilt",
            "Lamellen anhalten",
        ),
    ] {
        if feature(tile, bit) {
            out.push(localized_button(key, german, en, de));
        }
    }
    if feature(tile, COVER_SET_POSITION) {
        out.push(slider(
            "position",
            german,
            "Position",
            "Position",
            numeric_attr(tile, &["current_position"]).unwrap_or(0.0),
            0.0,
            100.0,
            1.0,
            "%",
        ));
    }
    if feature(tile, COVER_SET_TILT) {
        out.push(slider(
            "tilt_position",
            german,
            "Tilt",
            "Lamellenstellung",
            numeric_attr(tile, &["current_tilt_position"]).unwrap_or(0.0),
            0.0,
            100.0,
            1.0,
            "%",
        ));
    }
    let speeds = string_options(tile, "supported_speeds");
    if feature(tile, COVER_SPEED) && !speeds.is_empty() {
        if feature(tile, COVER_OPEN) {
            out.push(form(
                "open_speed",
                german,
                "Open at speed",
                "Mit Geschwindigkeit öffnen",
                vec![select_input(
                    "speed",
                    german,
                    "Speed",
                    "Geschwindigkeit",
                    speeds.clone(),
                )],
            ));
        }
        if feature(tile, COVER_CLOSE) {
            out.push(form(
                "close_speed",
                german,
                "Close at speed",
                "Mit Geschwindigkeit schließen",
                vec![select_input(
                    "speed",
                    german,
                    "Speed",
                    "Geschwindigkeit",
                    speeds.clone(),
                )],
            ));
        }
        if feature(tile, COVER_SET_POSITION) {
            out.push(form(
                "position_speed",
                german,
                "Position at speed",
                "Position mit Geschwindigkeit",
                vec![
                    number_input(
                        "position", german, "Position", "Position", 0.0, 0.0, 100.0, 1.0, "%", true,
                    ),
                    select_input("speed", german, "Speed", "Geschwindigkeit", speeds),
                ],
            ));
        }
    }
}

fn controls_humidifier(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    push_power_controls(tile, german, out, true, true);
    if numeric_attr(tile, &["humidity", "target_humidity"]).is_some() {
        let (min, max) = range(tile, "min_humidity", "max_humidity", 0.0, 100.0);
        out.push(slider(
            "humidity",
            german,
            "Humidity",
            "Luftfeuchtigkeit",
            numeric_attr(tile, &["humidity"]).unwrap_or(min),
            min,
            max,
            1.0,
            "%",
        ));
    }
    let modes = string_options(tile, "available_modes");
    if feature(tile, HUMIDIFIER_MODES) && !modes.is_empty() {
        out.push(select(
            "mode",
            german,
            "Mode",
            "Modus",
            modes,
            attribute(tile, "mode"),
        ));
    }
}

fn controls_water_heater(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    if feature(tile, WATER_HEATER_ON_OFF) {
        push_power_controls(tile, german, out, true, true);
    }
    if feature(tile, WATER_HEATER_TARGET_TEMP) {
        let (min, max) = range(tile, "min_temp", "max_temp", 0.0, 100.0);
        out.push(slider(
            "temperature",
            german,
            "Temperature",
            "Temperatur",
            numeric_attr(tile, &["temperature"]).unwrap_or(min),
            min,
            max,
            numeric_attr(tile, &["target_temp_step"]).unwrap_or(1.0),
            str_attr(tile, "unit_of_measurement").unwrap_or(""),
        ));
    }
    let modes = string_options(tile, "operation_list");
    if feature(tile, WATER_HEATER_OPERATION_MODE) && !modes.is_empty() {
        out.push(select(
            "operation_mode",
            german,
            "Operation mode",
            "Betriebsmodus",
            modes,
            attribute(tile, "operation_mode"),
        ));
    }
    if feature(tile, WATER_HEATER_AWAY_MODE) {
        out.push(toggle(
            "away_mode",
            german,
            "Away mode",
            "Abwesenheitsmodus",
            bool_attr(tile, "away_mode"),
        ));
    }
}

fn controls_valve(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    for (bit, key, en, de) in [
        (VALVE_OPEN, "open", "Open", "Öffnen"),
        (VALVE_CLOSE, "close", "Close", "Schließen"),
        (VALVE_STOP, "stop", "Stop", "Anhalten"),
    ] {
        if feature(tile, bit) {
            out.push(localized_button(key, german, en, de));
        }
    }
    if feature(tile, VALVE_SET_POSITION) {
        out.push(slider(
            "position",
            german,
            "Position",
            "Position",
            numeric_attr(tile, &["current_position"]).unwrap_or(0.0),
            0.0,
            100.0,
            1.0,
            "%",
        ));
    }
}

fn controls_siren(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    if feature(tile, SIREN_TURN_ON) {
        let mut inputs = Vec::new();
        let tones = feature_list(tile, "available_tones");
        if feature(tile, SIREN_TONES) && !tones.is_empty() {
            inputs.push(select_input("tone", german, "Tone", "Ton", tones));
        }
        if feature(tile, SIREN_VOLUME) {
            inputs.push(number_input(
                "volume_level",
                german,
                "Volume",
                "Lautstärke",
                1.0,
                0.0,
                1.0,
                0.01,
                "",
                false,
            ));
        }
        if feature(tile, SIREN_DURATION) {
            inputs.push(number_input(
                "duration", german, "Duration", "Dauer", 10.0, 1.0, 86_400.0, 1.0, "s", false,
            ));
        }
        if inputs.is_empty() {
            push_power_controls(tile, german, out, true, feature(tile, SIREN_TURN_OFF));
        } else {
            out.push(form("turn_on", german, "Turn on", "Einschalten", inputs));
        }
    }
    if feature(tile, SIREN_TURN_OFF)
        && !out.iter().any(|control| control.key == "turn_off")
        && tile.state_value() != Some("off")
    {
        out.push(localized_button(
            "turn_off",
            german,
            "Turn off",
            "Ausschalten",
        ));
    }
}

fn controls_lawn_mower(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    for (bit, key, en, de) in [
        (LAWN_START, "start", "Start mowing", "Mähen starten"),
        (LAWN_PAUSE, "pause", "Pause", "Pausieren"),
        (LAWN_STOP, "stop", "Stop", "Anhalten"),
        (LAWN_DOCK, "dock", "Return to dock", "Zur Ladestation"),
    ] {
        if feature(tile, bit) {
            out.push(localized_button(key, german, en, de));
        }
    }
}

fn controls_vacuum(tile: &EntityTile, german: bool, out: &mut Vec<ControlSpec>) {
    for (bit, key, en, de) in [
        (VACUUM_START, "start", "Start", "Starten"),
        (VACUUM_PAUSE, "pause", "Pause", "Pausieren"),
        (VACUUM_STOP, "stop", "Stop", "Anhalten"),
        (
            VACUUM_RETURN_HOME,
            "return_home",
            "Return to dock",
            "Zur Ladestation",
        ),
        (VACUUM_LOCATE, "locate", "Locate", "Suchen"),
        (
            VACUUM_CLEAN_SPOT,
            "clean_spot",
            "Clean spot",
            "Punktreinigung",
        ),
    ] {
        if feature(tile, bit) {
            out.push(localized_button(key, german, en, de));
        }
    }
    let speeds = string_options(tile, "fan_speed_list");
    if feature(tile, VACUUM_FAN_SPEED) && !speeds.is_empty() {
        out.push(select(
            "fan_speed",
            german,
            "Suction",
            "Saugstufe",
            speeds,
            attribute(tile, "fan_speed"),
        ));
    }
    if feature(tile, VACUUM_CLEAN_AREA) {
        out.push(form(
            "clean_area",
            german,
            "Clean mapped areas",
            "Zugeordnete Bereiche reinigen",
            vec![InputSpec::text(
                "area_ids",
                &label(
                    german,
                    "Home Assistant area IDs, separated by commas",
                    "Home-Assistant-Bereichs-IDs, durch Kommas getrennt",
                ),
                true,
            )],
        ));
    }
}

fn vacuum_area_ids(argument: &Value) -> Result<Vec<String>, ActionError> {
    const MAX_AREAS: usize = 32;
    const MAX_AREA_ID_BYTES: usize = 64;
    let map = object(argument, &["area_ids"], &["area_ids"])?;
    let value = required_text(map, "area_ids", MAX_AREAS * (MAX_AREA_ID_BYTES + 1))?;
    let mut areas = Vec::new();
    for area in value.split(',').map(str::trim) {
        if area.is_empty()
            || area.len() > MAX_AREA_ID_BYTES
            || !area
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            || areas.iter().any(|existing| existing == area)
        {
            return Err(ActionError::InvalidValue);
        }
        areas.push(area.to_owned());
    }
    if areas.is_empty() || areas.len() > MAX_AREAS {
        return Err(ActionError::InvalidValue);
    }
    Ok(areas)
}

fn group_supports_power(tile: &EntityTile) -> bool {
    let Some(members) = attribute(tile, "entity_id").and_then(Value::as_array) else {
        return false;
    };
    !members.is_empty()
        && members.iter().all(|member| {
            let Some(entity_id) = member.as_str() else {
                return false;
            };
            let Some((domain, object_id)) = entity_id.split_once('.') else {
                return false;
            };
            !object_id.is_empty()
                && matches!(
                    domain,
                    "automation"
                        | "fan"
                        | "humidifier"
                        | "input_boolean"
                        | "light"
                        | "media_player"
                        | "remote"
                        | "script"
                        | "switch"
                )
        })
}

// This is the domain dispatch table; each branch builds only currently offered operations.
#[allow(clippy::too_many_lines)]
fn build_controls(tile: &EntityTile, german: bool) -> Vec<ControlSpec> {
    let mut out = Vec::new();
    if !available(tile) {
        return out;
    }
    match tile.domain {
        D::Light => controls_light(tile, german, &mut out),
        D::Fan => controls_fan(tile, german, &mut out),
        D::Lock => controls_lock(tile, german, &mut out),
        D::AlarmControlPanel => controls_alarm(tile, german, &mut out),
        D::Number => controls_number(tile, german, &mut out),
        D::InputNumber => {
            controls_number(tile, german, &mut out);
            push_input_number_step_controls(german, &mut out);
        }
        D::Select | D::InputSelect => controls_select(tile, german, &mut out),
        D::Climate => controls_climate(tile, german, &mut out),
        D::Cover => controls_cover(tile, german, &mut out),
        D::Humidifier => controls_humidifier(tile, german, &mut out),
        D::WaterHeater => controls_water_heater(tile, german, &mut out),
        D::Valve => controls_valve(tile, german, &mut out),
        D::Siren => controls_siren(tile, german, &mut out),
        D::LawnMower => controls_lawn_mower(tile, german, &mut out),
        D::Vacuum => controls_vacuum(tile, german, &mut out),
        D::Switch | D::InputBoolean => push_power_controls(tile, german, &mut out, true, true),
        D::Scene => out.push(localized_button(
            "activate",
            german,
            "Activate",
            "Aktivieren",
        )),
        D::Button | D::InputButton => {
            out.push(localized_button("press", german, "Press", "Drücken"));
        }
        D::Script => {
            let (fields, unsupported_required, has_schema) = script_fields(tile, german);
            let inputs = fields
                .into_iter()
                .map(|field| field.input)
                .collect::<Vec<_>>();
            let mut start = if has_schema && !inputs.is_empty() {
                form("start", german, "Run", "Ausführen", inputs)
            } else {
                localized_button("start", german, "Run", "Ausführen")
            };
            let omitted_fields = numeric_attr(tile, &["njorun_script_fields_omitted"])
                .is_some_and(|count| count > 0.0);
            start.enabled = has_schema && !unsupported_required && !omitted_fields;
            if omitted_fields {
                start.label = label(
                    german,
                    "This script has more fields than the panel supports. Configure a wrapper script in Home Assistant.",
                    "Dieses Skript hat mehr Felder, als das Panel unterstützt. Richte ein Wrapper-Skript in Home Assistant ein.",
                );
            } else if unsupported_required {
                start.label = label(
                    german,
                    "A required script field uses an input the panel cannot render. Configure a wrapper script in Home Assistant.",
                    "Ein erforderliches Skriptfeld verwendet eine Eingabe, die das Panel nicht darstellen kann. Richte ein Wrapper-Skript in Home Assistant ein.",
                );
            } else if !has_schema {
                start.label = label(
                    german,
                    "Script parameters could not be read. Configure a wrapper script in Home Assistant.",
                    "Skriptparameter konnten nicht gelesen werden. Richte ein Wrapper-Skript in Home Assistant ein.",
                );
            }
            if !start.enabled {
                start.group = "explanation";
            }
            out.push(start);
            if tile.state_value() == Some("on") {
                out.push(localized_button("stop", german, "Stop", "Anhalten"));
            }
        }
        D::Automation => {
            match tile.state_value() {
                Some("on") => out.push(localized_button(
                    "disable",
                    german,
                    "Disable",
                    "Deaktivieren",
                )),
                Some("off") => out.push(localized_button("enable", german, "Enable", "Aktivieren")),
                _ => {
                    out.push(localized_button("enable", german, "Enable", "Aktivieren"));
                    out.push(localized_button(
                        "disable",
                        german,
                        "Disable",
                        "Deaktivieren",
                    ));
                }
            }
            let skip = InputSpec {
                key: "skip_condition",
                label: label(german, "Skip conditions", "Bedingungen überspringen"),
                kind: InputKind::Toggle,
                value: json!(false),
                options: vec![],
                minimum: 0.0,
                maximum: 1.0,
                step: 1.0,
                required: true,
            };
            out.push(form(
                "run",
                german,
                "Run automation",
                "Automation ausführen",
                vec![skip],
            ));
        }
        D::Text | D::InputText => {
            let min = numeric_attr(tile, &["min"])
                .unwrap_or(0.0)
                .clamp(0.0, 255.0);
            let max = numeric_attr(tile, &["max"])
                .unwrap_or(255.0)
                .clamp(1.0, 255.0);
            out.push(form(
                "set_value",
                german,
                "Set text",
                "Text setzen",
                vec![InputSpec::text(
                    "value",
                    &label(german, "Value", "Wert"),
                    min > 0.0,
                )],
            ));
            let input = &mut out.last_mut().expect("inserted").inputs[0];
            input.minimum = min;
            input.maximum = max;
        }
        D::Date => out.push(form(
            "set_value",
            german,
            "Set date",
            "Datum setzen",
            vec![InputSpec::text(
                "date",
                &label(german, "Date (YYYY-MM-DD)", "Datum (JJJJ-MM-TT)"),
                true,
            )],
        )),
        D::Time => out.push(form(
            "set_value",
            german,
            "Set time",
            "Uhrzeit setzen",
            vec![InputSpec::text(
                "time",
                &label(german, "Time (HH:MM:SS)", "Uhrzeit (HH:MM:SS)"),
                true,
            )],
        )),
        D::Datetime => out.push(form(
            "set_value",
            german,
            "Set date and time",
            "Datum und Uhrzeit setzen",
            vec![InputSpec::text(
                "datetime",
                &label(
                    german,
                    "Date and time (ISO 8601)",
                    "Datum und Uhrzeit (ISO 8601)",
                ),
                true,
            )],
        )),
        D::InputDatetime => {
            let mut inputs = Vec::new();
            if attribute(tile, "has_date")
                .and_then(Value::as_bool)
                .unwrap_or(true)
            {
                inputs.push(InputSpec::text(
                    "date",
                    &label(german, "Date (YYYY-MM-DD)", "Datum (JJJJ-MM-TT)"),
                    true,
                ));
            }
            if attribute(tile, "has_time")
                .and_then(Value::as_bool)
                .unwrap_or(true)
            {
                inputs.push(InputSpec::text(
                    "time",
                    &label(german, "Time (HH:MM:SS)", "Uhrzeit (HH:MM:SS)"),
                    true,
                ));
            }
            if !inputs.is_empty() {
                out.push(form(
                    "set_datetime",
                    german,
                    "Set date and time",
                    "Datum und Uhrzeit setzen",
                    inputs,
                ));
            }
        }
        D::Counter => {
            out.push(localized_button(
                "increment",
                german,
                "Increment",
                "Erhöhen",
            ));
            out.push(localized_button(
                "decrement",
                german,
                "Decrement",
                "Verringern",
            ));
            out.push(localized_button("reset", german, "Reset", "Zurücksetzen"));
            let (min, max) = range(tile, "minimum", "maximum", -1_000_000.0, 1_000_000.0);
            let step = numeric_attr(tile, &["step"]).unwrap_or(1.0).abs().max(1.0);
            let current = tile
                .state_value()
                .and_then(|v| v.parse().ok())
                .unwrap_or(min);
            out.push(slider(
                "set_value",
                german,
                "Set value",
                "Wert setzen",
                current,
                min,
                max,
                step,
                "",
            ));
            out.push(form(
                "set_value_exact",
                german,
                "Enter exact value",
                "Exakten Wert eingeben",
                vec![number_input(
                    "value", german, "Value", "Wert", current, min, max, step, "", true,
                )],
            ));
        }
        D::Timer => {
            out.push(form(
                "start",
                german,
                "Start timer",
                "Timer starten",
                vec![InputSpec::text(
                    "duration",
                    &label(german, "Duration (HH:MM:SS)", "Dauer (HH:MM:SS)"),
                    false,
                )],
            ));
            if tile.state_value() == Some("active") {
                out.push(localized_button("pause", german, "Pause", "Pausieren"));
                out.push(form(
                    "change",
                    german,
                    "Change timer",
                    "Timer ändern",
                    vec![InputSpec::text(
                        "duration",
                        &label(german, "Duration (HH:MM:SS)", "Dauer (HH:MM:SS)"),
                        true,
                    )],
                ));
            }
            if matches!(tile.state_value(), Some("active" | "paused")) {
                out.push(localized_button("cancel", german, "Cancel", "Abbrechen"));
                out.push(localized_button("finish", german, "Finish", "Beenden"));
            }
        }
        D::Update => {
            if tile.state_value() == Some("on") {
                if feature(tile, UPDATE_INSTALL) {
                    let mut install = localized_button(
                        "install",
                        german,
                        "Install update",
                        "Update installieren",
                    );
                    install.dangerous = true;
                    out.push(install);
                    if feature(tile, UPDATE_SPECIFIC_VERSION) {
                        out.push(form(
                            "install_version",
                            german,
                            "Install version",
                            "Version installieren",
                            vec![InputSpec::text(
                                "version",
                                &label(german, "Version", "Version"),
                                true,
                            )],
                        ));
                    }
                    if feature(tile, UPDATE_BACKUP) {
                        out.push(toggle(
                            "install_with_backup",
                            german,
                            "Install with backup",
                            "Mit Sicherung installieren",
                            true,
                        ));
                    }
                }
                out.push(localized_button(
                    "skip",
                    german,
                    "Skip update",
                    "Update überspringen",
                ));
            }
            if str_attr(tile, "skipped_version").is_some_and(|version| !version.is_empty()) {
                out.push(localized_button(
                    "clear_skipped",
                    german,
                    "Clear skipped update",
                    "Übersprungenes Update wieder anzeigen",
                ));
            }
            if feature(tile, UPDATE_RELEASE_NOTES) {
                out.push(localized_button(
                    "release_notes",
                    german,
                    "Release notes",
                    "Versionshinweise",
                ));
            }
        }
        D::Weather => {
            let forecast_types = weather_forecast_options(tile);
            if !forecast_types.is_empty() {
                out.push(select(
                    "get_forecasts",
                    german,
                    "Get forecast",
                    "Vorhersage abrufen",
                    forecast_types,
                    None,
                ));
            }
        }
        D::Calendar => {
            out.push(form(
                "get_events",
                german,
                "Get events",
                "Termine abrufen",
                vec![
                    InputSpec::text(
                        "start_date_time",
                        &label(german, "Start (ISO 8601)", "Beginn (ISO 8601)"),
                        false,
                    ),
                    InputSpec::text(
                        "end_date_time",
                        &label(german, "End (ISO 8601)", "Ende (ISO 8601)"),
                        false,
                    ),
                    InputSpec::text(
                        "duration",
                        &label(
                            german,
                            "Duration (seconds or HH:MM:SS)",
                            "Dauer (Sekunden oder HH:MM:SS)",
                        ),
                        false,
                    ),
                ],
            ));
            if feature(tile, CALENDAR_CREATE) {
                out.push(form(
                    "create_event",
                    german,
                    "Create event",
                    "Termin erstellen",
                    vec![
                        InputSpec::text("summary", &label(german, "Title", "Titel"), true),
                        InputSpec::text(
                            "start_date_time",
                            &label(
                                german,
                                "Start time (YYYY-MM-DD HH:MM:SS)",
                                "Beginn (JJJJ-MM-TT HH:MM:SS)",
                            ),
                            false,
                        ),
                        InputSpec::text(
                            "end_date_time",
                            &label(
                                german,
                                "End time (YYYY-MM-DD HH:MM:SS)",
                                "Ende (JJJJ-MM-TT HH:MM:SS)",
                            ),
                            false,
                        ),
                        InputSpec::text(
                            "start_date",
                            &label(
                                german,
                                "Start date for all-day event",
                                "Startdatum für ganztägigen Termin",
                            ),
                            false,
                        ),
                        InputSpec::text(
                            "end_date",
                            &label(
                                german,
                                "End date for all-day event",
                                "Enddatum für ganztägigen Termin",
                            ),
                            false,
                        ),
                        InputSpec::text(
                            "description",
                            &label(german, "Description", "Beschreibung"),
                            false,
                        ),
                        InputSpec::text("location", &label(german, "Location", "Ort"), false),
                    ],
                ));
                out.push(form(
                    "create_event_relative",
                    german,
                    "Create relative event",
                    "Relativen Termin erstellen",
                    vec![
                        InputSpec::text("summary", &label(german, "Title", "Titel"), true),
                        select_input(
                            "relative_unit",
                            german,
                            "Relative unit",
                            "Relative Einheit",
                            vec!["days".into(), "weeks".into()],
                        ),
                        number_input(
                            "relative_value",
                            german,
                            "Delay",
                            "Vorlauf",
                            1.0,
                            1.0,
                            3650.0,
                            1.0,
                            "",
                            true,
                        ),
                        InputSpec::text(
                            "description",
                            &label(german, "Description", "Beschreibung"),
                            false,
                        ),
                        InputSpec::text("location", &label(german, "Location", "Ort"), false),
                    ],
                ));
            }
            if feature(tile, CALENDAR_UPDATE) {
                out.push(form(
                    "update_event",
                    german,
                    "Update event",
                    "Termin aktualisieren",
                    vec![
                        InputSpec::text("uid", &label(german, "Event UID", "Termin-UID"), true),
                        InputSpec::text(
                            "recurrence_id",
                            &label(
                                german,
                                "Recurrence ID (optional)",
                                "Wiederholungs-ID (optional)",
                            ),
                            false,
                        ),
                        InputSpec::text(
                            "recurrence_range",
                            &label(
                                german,
                                "Recurrence range (optional)",
                                "Wiederholungsbereich (optional)",
                            ),
                            false,
                        ),
                        InputSpec::text("summary", &label(german, "Title", "Titel"), true),
                        InputSpec::text(
                            "start",
                            &label(
                                german,
                                "Start (ISO date or date-time)",
                                "Beginn (ISO-Datum oder -Zeit)",
                            ),
                            true,
                        ),
                        InputSpec::text(
                            "end",
                            &label(
                                german,
                                "End (ISO date or date-time)",
                                "Ende (ISO-Datum oder -Zeit)",
                            ),
                            true,
                        ),
                        InputSpec::text(
                            "description",
                            &label(german, "Description", "Beschreibung"),
                            false,
                        ),
                        InputSpec::text("location", &label(german, "Location", "Ort"), false),
                        InputSpec::text(
                            "rrule",
                            &label(
                                german,
                                "Recurrence rule (optional)",
                                "Wiederholungsregel (optional)",
                            ),
                            false,
                        ),
                    ],
                ));
            }
            if feature(tile, CALENDAR_DELETE) {
                let mut remove = form(
                    "delete_event",
                    german,
                    "Delete event",
                    "Termin löschen",
                    vec![
                        InputSpec::text("uid", &label(german, "Event UID", "Termin-UID"), true),
                        InputSpec::text(
                            "recurrence_id",
                            &label(
                                german,
                                "Recurrence ID (optional)",
                                "Wiederholungs-ID (optional)",
                            ),
                            false,
                        ),
                        InputSpec::text(
                            "recurrence_range",
                            &label(
                                german,
                                "Recurrence range (optional)",
                                "Wiederholungsbereich (optional)",
                            ),
                            false,
                        ),
                    ],
                );
                remove.dangerous = true;
                out.push(remove);
            }
        }
        D::Todo => {
            out.push(localized_button(
                "get_items",
                german,
                "Refresh items",
                "Einträge abrufen",
            ));
            if feature(tile, TODO_CREATE) {
                out.push(form(
                    "add_item",
                    german,
                    "Add item",
                    "Eintrag hinzufügen",
                    todo_inputs(tile, german, true, false),
                ));
            }
            if feature(tile, TODO_UPDATE) {
                out.push(form(
                    "update_item",
                    german,
                    "Update item",
                    "Eintrag aktualisieren",
                    todo_inputs(tile, german, true, true),
                ));
            }
            if feature(tile, TODO_MOVE) {
                out.push(form(
                    "move_item",
                    german,
                    "Move item",
                    "Eintrag verschieben",
                    vec![
                        InputSpec::text("uid", &label(german, "Item UID", "Eintrags-UID"), true),
                        InputSpec::text(
                            "previous_uid",
                            &label(
                                german,
                                "Previous item UID (blank to move first)",
                                "Vorherige Eintrags-UID (leer für Anfang)",
                            ),
                            false,
                        ),
                    ],
                ));
            }
            if feature(tile, TODO_DELETE) {
                out.push(form(
                    "remove_item",
                    german,
                    "Remove item",
                    "Eintrag entfernen",
                    vec![InputSpec::text(
                        "item",
                        &label(german, "Item or UID", "Eintrag oder UID"),
                        true,
                    )],
                ));
                out.push(localized_button(
                    "remove_completed_items",
                    german,
                    "Remove completed",
                    "Erledigte entfernen",
                ));
            }
        }
        D::Notify => {
            let mut inputs = vec![InputSpec::text(
                "message",
                &label(german, "Message", "Nachricht"),
                true,
            )];
            if feature(tile, NOTIFY_TITLE) {
                inputs.push(InputSpec::text(
                    "title",
                    &label(german, "Title", "Titel"),
                    false,
                ));
            }
            out.push(form(
                "send_message",
                german,
                "Send notification",
                "Benachrichtigung senden",
                inputs,
            ));
        }
        D::Tts => out.push(form(
            "speak",
            german,
            "Speak",
            "Sprechen",
            vec![
                InputSpec::text(
                    "media_player_entity_id",
                    &label(german, "Media player entity ID", "Media-Player-Entity-ID"),
                    true,
                ),
                InputSpec::text("message", &label(german, "Message", "Nachricht"), true),
                InputSpec::text(
                    "language",
                    &label(german, "Language (optional)", "Sprache (optional)"),
                    false,
                ),
                InputSpec {
                    key: "cache",
                    label: label(german, "Use cache", "Cache verwenden"),
                    kind: InputKind::Toggle,
                    value: json!(true),
                    options: vec![],
                    minimum: 0.0,
                    maximum: 1.0,
                    step: 1.0,
                    required: false,
                },
            ],
        )),
        D::AssistSatellite if feature(tile, 1) => out.push(form(
            "announce",
            german,
            "Announce",
            "Durchsage",
            vec![InputSpec::text(
                "message",
                &label(german, "Message", "Nachricht"),
                true,
            )],
        )),
        D::Remote => controls_remote(tile, german, &mut out),
        D::Camera => controls_camera(tile, german, &mut out),
        D::Group => {
            if group_supports_power(tile) {
                push_power_controls(tile, german, &mut out, true, true);
                out.push(localized_button("toggle", german, "Toggle", "Umschalten"));
            }
        }
        D::Image => out.push(form(
            "snapshot",
            german,
            "Save image",
            "Bild speichern",
            vec![InputSpec::text(
                "filename",
                &label(
                    german,
                    "Filename on the HA host",
                    "Dateiname auf dem HA-Host",
                ),
                true,
            )],
        )),
        D::ImageProcessing => out.push(localized_button(
            "scan",
            german,
            "Scan image",
            "Bild analysieren",
        )),
        D::Schedule => out.push(localized_button(
            "get_schedule",
            german,
            "Get schedule",
            "Zeitplan abrufen",
        )),
        // Read-only entities and provider-specific domains need a separate query/result
        // transport or integration-specific service schema before they can be controlled.
        _ => {}
    }
    out
}

fn valid_number(value: &Value, min: f64, max: f64, step: f64) -> Result<f64, ActionError> {
    let number = value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or(ActionError::InvalidValue)?;
    if number < min || number > max {
        return Err(ActionError::InvalidValue);
    }
    let step = step.abs().max(f64::EPSILON);
    let offset = (number - min) / step;
    if (offset - offset.round()).abs() > 1e-6 {
        return Err(ActionError::InvalidValue);
    }
    Ok(number)
}

fn valid_integer(value: &Value, min: u64, max: u64) -> Result<u64, ActionError> {
    let number = value
        .as_u64()
        .or_else(|| {
            let number = value.as_f64()?;
            if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
                return None;
            }
            format!("{number:.0}").parse::<u64>().ok()
        })
        .ok_or(ActionError::InvalidValue)?;
    if number < min || number > max {
        return Err(ActionError::InvalidValue);
    }
    Ok(number)
}

fn valid_signed_integer(value: &Value, min: i64, max: i64) -> Result<i64, ActionError> {
    let number = value
        .as_i64()
        .or_else(|| {
            let number = value.as_f64()?;
            if !number.is_finite() || number.fract() != 0.0 {
                return None;
            }
            format!("{number:.0}").parse::<i64>().ok()
        })
        .ok_or(ActionError::InvalidValue)?;
    if number < min || number > max {
        return Err(ActionError::InvalidValue);
    }
    Ok(number)
}

fn integer_bound(value: f64) -> Result<u64, ActionError> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return Err(ActionError::InvalidValue);
    }
    format!("{value:.0}")
        .parse::<u64>()
        .map_err(|_| ActionError::InvalidValue)
}

fn signed_integer_bound(value: f64) -> Result<i64, ActionError> {
    if !value.is_finite() || value.fract() != 0.0 {
        return Err(ActionError::InvalidValue);
    }
    format!("{value:.0}")
        .parse::<i64>()
        .map_err(|_| ActionError::InvalidValue)
}

fn bounded_text_length(value: f64) -> usize {
    if !value.is_finite() || value < 1.0 {
        return 4096;
    }
    if value >= 4096.0 {
        return 4096;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let maximum = value as usize;
    maximum
}

fn valid_string<'a>(value: &'a Value, choices: &[String]) -> Result<&'a str, ActionError> {
    let value = value.as_str().ok_or(ActionError::InvalidValue)?;
    if value.is_empty()
        || value.chars().count() > 255
        || !choices.iter().any(|choice| choice == value)
    {
        return Err(ActionError::InvalidValue);
    }
    Ok(value)
}

fn object<'a>(
    value: &'a Value,
    allowed: &[&str],
    required: &[&str],
) -> Result<&'a Map<String, Value>, ActionError> {
    let data = value.as_object().ok_or(ActionError::InvalidValue)?;
    if data.keys().any(|key| !allowed.contains(&key.as_str()))
        || required.iter().any(|key| !data.contains_key(*key))
    {
        return Err(ActionError::InvalidValue);
    }
    Ok(data)
}

fn validate_input(input: &InputSpec, value: &Value) -> Result<(), ActionError> {
    match input.kind {
        InputKind::Number => {
            valid_number(value, input.minimum, input.maximum, input.step)?;
        }
        InputKind::Select => {
            valid_string(value, &input.options)?;
        }
        InputKind::Toggle => {
            value.as_bool().ok_or(ActionError::InvalidValue)?;
        }
        InputKind::Entity => {
            let entity_id = value
                .as_str()
                .filter(|value| !value.is_empty() && value.chars().count() <= 255)
                .ok_or(ActionError::InvalidValue)?;
            let (domain, name) = entity_id.split_once('.').ok_or(ActionError::InvalidValue)?;
            if domain.is_empty()
                || name.is_empty()
                || (!input.options.is_empty()
                    && !input.options.iter().any(|option| option == domain))
            {
                return Err(ActionError::InvalidValue);
            }
        }
        InputKind::Text | InputKind::Password => {
            let value = value.as_str().ok_or(ActionError::InvalidValue)?;
            let maximum = bounded_text_length(input.maximum);
            if value.chars().count() > maximum || (input.required && value.is_empty()) {
                return Err(ActionError::InvalidValue);
            }
        }
    }
    Ok(())
}

fn validate_control_argument(
    tile: &EntityTile,
    key: &str,
    argument: &Value,
) -> Result<(), ActionError> {
    let control = build_controls(tile, false)
        .into_iter()
        .find(|control| control.key == key && control.enabled)
        .ok_or(ActionError::Unsupported)?;
    match control.kind {
        ControlKind::Button => {
            if !argument.is_null() && !argument.as_object().is_some_and(Map::is_empty) {
                return Err(ActionError::InvalidValue);
            }
        }
        ControlKind::Slider => {
            valid_number(argument, control.minimum, control.maximum, control.step)?;
        }
        ControlKind::Select => {
            valid_string(argument, &control.options)?;
        }
        ControlKind::Toggle => {
            argument.as_bool().ok_or(ActionError::InvalidValue)?;
        }
        ControlKind::Form => {
            let input_keys = control
                .inputs
                .iter()
                .map(|input| input.key)
                .collect::<Vec<_>>();
            let form = object(argument, &input_keys, &[])?;
            for input in &control.inputs {
                if let Some(value) = form.get(input.key) {
                    validate_input(input, value)?;
                } else if input.required {
                    return Err(ActionError::InvalidValue);
                }
            }
        }
    }
    Ok(())
}

fn required_text<'a>(
    map: &'a Map<String, Value>,
    key: &str,
    max: usize,
) -> Result<&'a str, ActionError> {
    let value = map
        .get(key)
        .and_then(Value::as_str)
        .ok_or(ActionError::InvalidValue)?;
    if value.is_empty() || value.chars().count() > max {
        return Err(ActionError::InvalidValue);
    }
    Ok(value)
}

fn optional_text(
    map: &Map<String, Value>,
    key: &str,
    max: usize,
) -> Result<Option<String>, ActionError> {
    map.get(key)
        .map(|value| {
            let text = value.as_str().ok_or(ActionError::InvalidValue)?;
            if text.chars().count() > max {
                return Err(ActionError::InvalidValue);
            }
            Ok(text.to_owned())
        })
        .transpose()
}

fn command_list(
    map: &Map<String, Value>,
    key: &str,
    required: bool,
) -> Result<Option<Vec<String>>, ActionError> {
    let Some(value) = map.get(key) else {
        return if required {
            Err(ActionError::InvalidValue)
        } else {
            Ok(None)
        };
    };
    let text = value.as_str().ok_or(ActionError::InvalidValue)?;
    if text.is_empty() && !required {
        return Ok(None);
    }
    let commands = text
        .split(';')
        .map(str::trim)
        .map(|command| {
            if command.is_empty() || command.chars().count() > 255 {
                Err(ActionError::InvalidValue)
            } else {
                Ok(command.to_owned())
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    if commands.is_empty() || commands.len() > 32 {
        return Err(ActionError::InvalidValue);
    }
    Ok(Some(commands))
}

fn add_todo_optional_fields(
    tile: &EntityTile,
    source: &Map<String, Value>,
    destination: &mut Map<String, Value>,
) -> Result<bool, ActionError> {
    let due_date = source
        .get("due_date")
        .map(|value| {
            let date = value
                .as_str()
                .filter(|date| valid_date(date))
                .ok_or(ActionError::InvalidValue)?;
            Ok::<_, ActionError>(date.to_owned())
        })
        .transpose()?;
    let due_datetime = source
        .get("due_datetime")
        .map(|value| {
            let datetime = value
                .as_str()
                .filter(|datetime| valid_datetime(datetime))
                .ok_or(ActionError::InvalidValue)?;
            Ok::<_, ActionError>(datetime.to_owned())
        })
        .transpose()?;
    if let Some(datetime) = due_datetime.as_deref() {
        let timezone = &datetime[19..];
        if timezone != "Z" && !timezone.starts_with('+') && !timezone.starts_with('-') {
            return Err(ActionError::InvalidValue);
        }
    }
    if due_date.is_some() && due_datetime.is_some() {
        return Err(ActionError::InvalidValue);
    }
    let description = optional_text(source, "description", 4096)?;
    if let Some(date) = &due_date {
        destination.insert("due_date".into(), json!(date));
    }
    if let Some(datetime) = &due_datetime {
        destination.insert("due_datetime".into(), json!(datetime));
    }
    if let Some(description) = &description {
        destination.insert("description".into(), json!(description));
    }
    if (!feature(tile, TODO_DUE_DATE) && source.contains_key("due_date"))
        || (!feature(tile, TODO_DUE_DATETIME) && source.contains_key("due_datetime"))
        || (!feature(tile, TODO_DESCRIPTION) && source.contains_key("description"))
    {
        return Err(ActionError::Unsupported);
    }
    Ok(due_date.is_some() || due_datetime.is_some() || description.is_some())
}

fn code_value(value: &Value, numeric_format: bool) -> Result<String, ActionError> {
    if let Some(code) = value.as_str() {
        if code.is_empty() || code.chars().count() > 32 {
            return Err(ActionError::InvalidValue);
        }
        if numeric_format && !code.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ActionError::InvalidValue);
        }
        return Ok(code.to_owned());
    }
    let number = value
        .as_f64()
        .filter(|number| number.is_finite() && *number >= 0.0 && number.fract() == 0.0)
        .ok_or(ActionError::InvalidValue)?;
    if number > 999_999_999.0 {
        return Err(ActionError::InvalidValue);
    }
    Ok(format!("{number:.0}"))
}

fn optional_code(
    map: &Map<String, Value>,
    required: bool,
    numeric_format: bool,
) -> Result<Option<String>, ActionError> {
    match map.get("code") {
        Some(value) => Ok(Some(code_value(value, numeric_format)?)),
        None if required => Err(ActionError::InvalidValue),
        None => Ok(None),
    }
}

fn service(tile: &EntityTile, name: &'static str, data: Value) -> (ActionKind, ServiceCall) {
    (
        crate::catalog_confirmation::action_kind(tile, name, &data),
        ServiceCall::new(tile.domain.service_domain(), name, data),
    )
}

fn require_offered(tile: &EntityTile, key: &str, argument: &Value) -> Result<(), ActionError> {
    if !available(tile) {
        return Err(ActionError::Unavailable);
    }
    validate_control_argument(tile, key, argument)
}

fn plan_alarm(
    tile: &EntityTile,
    key: &str,
    argument: &Value,
) -> Result<(ActionKind, ServiceCall), ActionError> {
    let modes = [
        ("arm_home", "alarm_arm_home", "armed_home", ALARM_HOME),
        ("arm_away", "alarm_arm_away", "armed_away", ALARM_AWAY),
        ("arm_night", "alarm_arm_night", "armed_night", ALARM_NIGHT),
        (
            "arm_vacation",
            "alarm_arm_vacation",
            "armed_vacation",
            ALARM_VACATION,
        ),
        (
            "arm_custom_bypass",
            "alarm_arm_custom_bypass",
            "armed_custom_bypass",
            ALARM_CUSTOM_BYPASS,
        ),
    ];
    if let Some((_, svc, state, _)) = modes.iter().find(|(action, _, _, _)| *action == key) {
        let code_required = bool_attr(tile, "code_arm_required");
        let formatted = attribute(tile, "code_format").is_some_and(|value| !value.is_null());
        let numeric_format = str_attr(tile, "code_format") == Some("number");
        let data = if code_required || formatted {
            let map = object(
                argument,
                &["code"],
                if code_required { &["code"] } else { &[] },
            )?;
            let code = optional_code(map, code_required, numeric_format)?;
            code.map_or_else(|| json!({}), |code| json!({"code": code}))
        } else {
            if !argument.is_null() && argument.as_object().is_some_and(|map| !map.is_empty()) {
                return Err(ActionError::InvalidValue);
            }
            json!({})
        };
        return Ok((
            ActionKind::Alarm((*state).to_owned()),
            ServiceCall::new("alarm_control_panel", svc, data),
        ));
    }
    if key == "disarm" {
        let formatted = attribute(tile, "code_format").is_some_and(|value| !value.is_null());
        let numeric_format = str_attr(tile, "code_format") == Some("number");
        let data = if formatted {
            let map = object(argument, &["code"], &["code"])?;
            json!({"code": code_value(&map["code"], numeric_format)?})
        } else {
            json!({})
        };
        return Ok((
            ActionKind::Alarm("disarmed".into()),
            ServiceCall::new("alarm_control_panel", "alarm_disarm", data),
        ));
    }
    if key == "trigger" {
        if !argument.is_null() {
            return Err(ActionError::InvalidValue);
        }
        return Ok(service(tile, "alarm_trigger", json!({})));
    }
    Err(ActionError::Unsupported)
}

/// Return the controls supported by this entity snapshot.
#[must_use]
pub fn controls(tile: &EntityTile, german: bool) -> Vec<ControlSpec> {
    build_controls(tile, german)
}

/// Validate an offered operation and produce a bounded Home Assistant service call.
///
/// # Errors
/// Returns an error when the entity is unavailable, the control is not offered, or the
/// supplied JSON does not satisfy the control's typed schema.
// Domain dispatch keeps each service payload adjacent to its validation contract.
#[allow(clippy::too_many_lines)]
pub fn plan(
    tile: &EntityTile,
    key: &str,
    argument: &Value,
) -> Result<(ActionKind, ServiceCall), ActionError> {
    require_offered(tile, key, argument)?;
    match tile.domain {
        D::Light => match key {
            "turn_on" => Ok(service(tile, "turn_on", json!({}))),
            "turn_off" => Ok(service(tile, "turn_off", json!({}))),
            "brightness" => {
                let value = valid_number(argument, 1.0, 100.0, 1.0)?;
                let percent = valid_integer(&json!(value), 1, 100)?;
                let action_percent =
                    u8::try_from(percent).map_err(|_| ActionError::InvalidValue)?;
                Ok((
                    ActionKind::SetBrightness(action_percent),
                    ServiceCall::new("light", "turn_on", json!({"brightness_pct": percent})),
                ))
            }
            "brightness_step_pct" | "brightness_step" => {
                let map = object(argument, &["step"], &["step"])?;
                let (field, min, max) = if key == "brightness_step_pct" {
                    ("brightness_step_pct", -100, 100)
                } else {
                    ("brightness_step", -225, 255)
                };
                let step = valid_signed_integer(&map["step"], min, max)?;
                if step == 0 {
                    return Err(ActionError::InvalidValue);
                }
                Ok(service(tile, "turn_on", json!({field:step})))
            }
            "color_temp" => {
                let (min, max) = range(
                    tile,
                    "min_color_temp_kelvin",
                    "max_color_temp_kelvin",
                    2000.0,
                    6500.0,
                );
                let value = valid_number(argument, min, max, 1.0)?;
                let minimum = integer_bound(min)?;
                let maximum = integer_bound(max)?;
                Ok(service(
                    tile,
                    "turn_on",
                    json!({"color_temp_kelvin": valid_integer(&json!(value), minimum, maximum)?}),
                ))
            }
            "rgb_color" => {
                let map = object(
                    argument,
                    &["red", "green", "blue"],
                    &["red", "green", "blue"],
                )?;
                let red = valid_integer(&map["red"], 0, 255)?;
                let green = valid_integer(&map["green"], 0, 255)?;
                let blue = valid_integer(&map["blue"], 0, 255)?;
                Ok(service(
                    tile,
                    "turn_on",
                    json!({"rgb_color": [red, green, blue]}),
                ))
            }
            "color_name" => {
                let map = object(argument, &["color_name"], &["color_name"])?;
                let color = required_text(map, "color_name", 64)?;
                if !color
                    .bytes()
                    .all(|byte| byte.is_ascii_alphabetic() || byte == b' ')
                {
                    return Err(ActionError::InvalidValue);
                }
                Ok(service(tile, "turn_on", json!({"color_name":color})))
            }
            "hs_color" => {
                let map = object(argument, &["hue", "saturation"], &["hue", "saturation"])?;
                let hue = valid_integer(&map["hue"], 0, 360)?;
                let saturation = valid_integer(&map["saturation"], 0, 100)?;
                Ok(service(
                    tile,
                    "turn_on",
                    json!({"hs_color": [hue, saturation]}),
                ))
            }
            "xy_color" => {
                let map = object(argument, &["x", "y"], &["x", "y"])?;
                let x = valid_number(&map["x"], 0.0, 1.0, 0.001)?;
                let y = valid_number(&map["y"], 0.0, 1.0, 0.001)?;
                Ok(service(tile, "turn_on", json!({"xy_color": [x, y]})))
            }
            "rgbw_color" => {
                let map = object(
                    argument,
                    &["red", "green", "blue", "white"],
                    &["red", "green", "blue", "white"],
                )?;
                let color = ["red", "green", "blue", "white"]
                    .map(|key| valid_integer(&map[key], 0, 255))
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(service(tile, "turn_on", json!({"rgbw_color": color})))
            }
            "rgbww_color" => {
                let map = object(
                    argument,
                    &["red", "green", "blue", "cold_white", "warm_white"],
                    &["red", "green", "blue", "cold_white", "warm_white"],
                )?;
                let color = ["red", "green", "blue", "cold_white", "warm_white"]
                    .map(|key| valid_integer(&map[key], 0, 255))
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(service(tile, "turn_on", json!({"rgbww_color": color})))
            }
            "white" => Ok(service(
                tile,
                "turn_on",
                json!({"white": valid_integer(argument, 0, 255)?}),
            )),
            "effect" => {
                let value = valid_string(argument, &string_options(tile, "effect_list"))?;
                Ok(service(tile, "turn_on", json!({"effect": value})))
            }
            "transition" => {
                let value = valid_number(argument, 0.0, 300.0, 1.0)?;
                Ok(service(
                    tile,
                    "turn_on",
                    json!({"transition": valid_integer(&json!(value), 0, 300)?}),
                ))
            }
            "flash" => {
                let value = valid_string(argument, &["short".into(), "long".into()])?;
                Ok(service(tile, "turn_on", json!({"flash": value})))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::Fan => match key {
            "turn_on" => Ok(service(tile, "turn_on", json!({}))),
            "turn_off" => Ok(service(tile, "turn_off", json!({}))),
            "percentage" => {
                let value = valid_number(argument, 0.0, 100.0, 1.0)?;
                let percent = u8::try_from(valid_integer(&json!(value), 0, 100)?)
                    .map_err(|_| ActionError::InvalidValue)?;
                Ok((
                    ActionKind::SetFanSpeed(percent),
                    ServiceCall::new("fan", "set_percentage", json!({"percentage": percent})),
                ))
            }
            "increase_speed" | "decrease_speed" => {
                let map = object(argument, &["percentage_step"], &[])?;
                let mut data = json!({});
                if let Some(step) = map.get("percentage_step") {
                    let step = valid_integer(step, 0, 100)?;
                    if step > 0 {
                        data["percentage_step"] = json!(step);
                    }
                }
                let service_name = if key == "increase_speed" {
                    "increase_speed"
                } else {
                    "decrease_speed"
                };
                Ok(service(tile, service_name, data))
            }
            "preset_mode" => Ok(service(
                tile,
                "set_preset_mode",
                json!({"preset_mode": valid_string(argument, &string_options(tile, "preset_modes"))?}),
            )),
            "direction" => Ok(service(
                tile,
                "set_direction",
                json!({"direction": valid_string(argument, &["forward".into(), "reverse".into()])?}),
            )),
            "oscillate" => Ok(service(
                tile,
                "oscillate",
                json!({"oscillating": argument.as_bool().ok_or(ActionError::InvalidValue)?}),
            )),
            _ => Err(ActionError::Unsupported),
        },
        D::Lock => {
            let has_code = attribute(tile, "code_format").is_some_and(|value| !value.is_null());
            let data = if matches!(key, "lock" | "unlock" | "open") && has_code {
                let map = object(argument, &["code"], &["code"])?;
                json!({"code": code_value(&map["code"], str_attr(tile, "code_format") == Some("number"))?})
            } else {
                json!({})
            };
            match key {
                "lock" => Ok((
                    ActionKind::Lock(crate::LockTarget::Locked),
                    ServiceCall::new("lock", "lock", data),
                )),
                "unlock" => Ok((
                    ActionKind::Lock(crate::LockTarget::Unlocked),
                    ServiceCall::new("lock", "unlock", data),
                )),
                "open" => Ok(service(tile, "open", data)),
                _ => Err(ActionError::Unsupported),
            }
        }
        D::AlarmControlPanel => plan_alarm(tile, key, argument),
        D::Number | D::InputNumber => {
            if tile.domain == D::InputNumber && matches!(key, "increment" | "decrement") {
                let service_name = if key == "increment" {
                    "increment"
                } else {
                    "decrement"
                };
                return Ok(service(tile, service_name, json!({})));
            }
            let min = numeric_attr(tile, &["min"]).unwrap_or(0.0);
            let max = numeric_attr(tile, &["max"]).unwrap_or(100.0);
            let step = numeric_attr(tile, &["step"]).unwrap_or(1.0);
            let value = if key == "set_value_exact" {
                let map = object(argument, &["value"], &["value"])?;
                valid_number(&map["value"], min, max, step)?
            } else if key == "set_value" {
                valid_number(argument, min, max, step)?
            } else {
                return Err(ActionError::Unsupported);
            };
            let exact = if key == "set_value_exact" {
                argument["value"].to_string()
            } else {
                argument.to_string()
            };
            Ok((
                ActionKind::SetNumberExact(exact),
                ServiceCall::new(
                    tile.domain.service_domain(),
                    "set_value",
                    json!({"value": value}),
                ),
            ))
        }
        D::Select | D::InputSelect => match key {
            "select_option" => {
                let choice = valid_string(argument, &string_options(tile, "options"))?;
                Ok((
                    ActionKind::SelectOption(choice.to_owned()),
                    ServiceCall::new(
                        tile.domain.service_domain(),
                        "select_option",
                        json!({"option": choice}),
                    ),
                ))
            }
            "select_first" => Ok(service(tile, "select_first", json!({}))),
            "select_last" => Ok(service(tile, "select_last", json!({}))),
            "select_next" | "select_previous" => {
                let map = object(argument, &["cycle"], &[])?;
                let cycle = map.get("cycle").map_or(Ok(true), |value| {
                    value.as_bool().ok_or(ActionError::InvalidValue)
                })?;
                let service_name = if key == "select_next" {
                    "select_next"
                } else {
                    "select_previous"
                };
                Ok(service(tile, service_name, json!({"cycle":cycle})))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::Switch | D::InputBoolean => match key {
            "turn_on" => Ok(service(tile, "turn_on", json!({}))),
            "turn_off" => Ok(service(tile, "turn_off", json!({}))),
            _ => Err(ActionError::Unsupported),
        },
        D::Scene if key == "activate" => Ok(service(tile, "turn_on", json!({}))),
        D::Button | D::InputButton if key == "press" => Ok(service(tile, "press", json!({}))),
        D::Script => match key {
            "start" => {
                let (fields, _, has_schema) = script_fields(tile, false);
                if !has_schema || fields.is_empty() {
                    if !argument.is_null() && !argument.as_object().is_some_and(Map::is_empty) {
                        return Err(ActionError::InvalidValue);
                    }
                    return Ok(service(tile, "turn_on", json!({})));
                }
                let keys = fields
                    .iter()
                    .map(|field| field.input.key)
                    .collect::<Vec<_>>();
                let values = object(argument, &keys, &[])?;
                let mut variables = Map::new();
                for field in fields {
                    if let Some(value) = values.get(field.input.key) {
                        variables.insert(field.name, value.clone());
                    }
                }
                let mut data = Map::new();
                if !variables.is_empty() {
                    data.insert("variables".into(), Value::Object(variables));
                }
                Ok(service(tile, "turn_on", Value::Object(data)))
            }
            "stop" => Ok(service(tile, "turn_off", json!({}))),
            _ => Err(ActionError::Unsupported),
        },
        D::Automation => match key {
            "enable" => Ok(service(tile, "turn_on", json!({}))),
            "disable" => Ok(service(tile, "turn_off", json!({}))),
            "run" => {
                let map = object(argument, &["skip_condition"], &["skip_condition"])?;
                Ok(service(
                    tile,
                    "trigger",
                    json!({"skip_condition":map["skip_condition"].as_bool().ok_or(ActionError::InvalidValue)?}),
                ))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::Climate => match key {
            "hvac_mode" => {
                let modes = string_options(tile, "hvac_modes");
                Ok(service(
                    tile,
                    "set_hvac_mode",
                    json!({"hvac_mode": valid_string(argument, &modes)?}),
                ))
            }
            "turn_on" => Ok(service(tile, "turn_on", json!({}))),
            "turn_off" => Ok(service(tile, "turn_off", json!({}))),
            "set_temperature" => {
                let map = object(argument, &["temperature"], &["temperature"])?;
                let (min, max) = range(tile, "min_temp", "max_temp", 7.0, 35.0);
                let step = numeric_attr(tile, &["target_temp_step", "precision"])
                    .unwrap_or(0.5)
                    .abs()
                    .max(0.1);
                let temperature = valid_number(&map["temperature"], min, max, step)?;
                Ok(service(
                    tile,
                    "set_temperature",
                    json!({"temperature": temperature}),
                ))
            }
            "set_temperature_range" => {
                let map = object(
                    argument,
                    &["target_temp_low", "target_temp_high"],
                    &["target_temp_low", "target_temp_high"],
                )?;
                let (min, max) = range(tile, "min_temp", "max_temp", 7.0, 35.0);
                let step = numeric_attr(tile, &["target_temp_step", "precision"])
                    .unwrap_or(0.5)
                    .abs()
                    .max(0.1);
                let low = valid_number(&map["target_temp_low"], min, max, step)?;
                let high = valid_number(&map["target_temp_high"], min, max, step)?;
                if low >= high {
                    return Err(ActionError::InvalidValue);
                }
                Ok(service(
                    tile,
                    "set_temperature",
                    json!({"target_temp_low": low, "target_temp_high": high}),
                ))
            }
            "set_humidity" => {
                let (min, max) = range(tile, "min_humidity", "max_humidity", 0.0, 100.0);
                let step = numeric_attr(tile, &["target_humidity_step"])
                    .unwrap_or(1.0)
                    .max(1.0);
                let humidity = valid_integer(argument, integer_bound(min)?, integer_bound(max)?)?;
                let humidity_value =
                    f64::from(u32::try_from(humidity).map_err(|_| ActionError::InvalidValue)?);
                if ((humidity_value - min) / step - ((humidity_value - min) / step).round()).abs()
                    > 1e-6
                {
                    return Err(ActionError::InvalidValue);
                }
                Ok(service(tile, "set_humidity", json!({"humidity": humidity})))
            }
            "preset_mode" | "fan_mode" | "swing_mode" | "swing_horizontal_mode" => {
                let attr = match key {
                    "preset_mode" => "preset_modes",
                    "fan_mode" => "fan_modes",
                    "swing_mode" => "swing_modes",
                    _ => "swing_horizontal_modes",
                };
                let value = valid_string(argument, &string_options(tile, attr))?;
                let svc = match key {
                    "preset_mode" => "set_preset_mode",
                    "fan_mode" => "set_fan_mode",
                    "swing_mode" => "set_swing_mode",
                    _ => "set_swing_horizontal_mode",
                };
                Ok(service(tile, svc, json!({key: value})))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::Cover => match key {
            "open" => Ok(service(tile, "open_cover", json!({}))),
            "close" => Ok(service(tile, "close_cover", json!({}))),
            "stop" => Ok(service(tile, "stop_cover", json!({}))),
            "open_tilt" => Ok(service(tile, "open_cover_tilt", json!({}))),
            "close_tilt" => Ok(service(tile, "close_cover_tilt", json!({}))),
            "stop_tilt" => Ok(service(tile, "stop_cover_tilt", json!({}))),
            "position" => Ok(service(
                tile,
                "set_cover_position",
                json!({"position": valid_number(argument, 0.0, 100.0, 1.0)?}),
            )),
            "tilt_position" => Ok(service(
                tile,
                "set_cover_tilt_position",
                json!({"tilt_position": valid_number(argument, 0.0, 100.0, 1.0)?}),
            )),
            "open_speed" | "close_speed" => {
                let map = object(argument, &["speed"], &["speed"])?;
                let speed = valid_string(&map["speed"], &string_options(tile, "supported_speeds"))?;
                let service_name = if key == "open_speed" {
                    "open_cover"
                } else {
                    "close_cover"
                };
                Ok(service(tile, service_name, json!({"speed":speed})))
            }
            "position_speed" => {
                let map = object(argument, &["position", "speed"], &["position", "speed"])?;
                let position = valid_number(&map["position"], 0.0, 100.0, 1.0)?;
                let speed = valid_string(&map["speed"], &string_options(tile, "supported_speeds"))?;
                Ok(service(
                    tile,
                    "set_cover_position",
                    json!({"position":position,"speed":speed}),
                ))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::Humidifier => match key {
            "turn_on" => Ok(service(tile, "turn_on", json!({}))),
            "turn_off" => Ok(service(tile, "turn_off", json!({}))),
            "humidity" => {
                let (min, max) = range(tile, "min_humidity", "max_humidity", 0.0, 100.0);
                let humidity = valid_integer(argument, integer_bound(min)?, integer_bound(max)?)?;
                Ok(service(tile, "set_humidity", json!({"humidity":humidity})))
            }
            "mode" => Ok(service(
                tile,
                "set_mode",
                json!({"mode":valid_string(argument,&string_options(tile,"available_modes"))?}),
            )),
            _ => Err(ActionError::Unsupported),
        },
        D::WaterHeater => match key {
            "turn_on" => Ok(service(tile, "turn_on", json!({}))),
            "turn_off" => Ok(service(tile, "turn_off", json!({}))),
            "temperature" => {
                let (min, max) = range(tile, "min_temp", "max_temp", 0.0, 100.0);
                Ok(service(
                    tile,
                    "set_temperature",
                    json!({"temperature":valid_number(argument,min,max,numeric_attr(tile,&["target_temp_step"]).unwrap_or(1.0))?}),
                ))
            }
            "operation_mode" => Ok(service(
                tile,
                "set_operation_mode",
                json!({"operation_mode":valid_string(argument,&string_options(tile,"operation_list"))?}),
            )),
            "away_mode" => Ok(service(
                tile,
                "set_away_mode",
                json!({"away_mode":argument.as_bool().ok_or(ActionError::InvalidValue)?}),
            )),
            _ => Err(ActionError::Unsupported),
        },
        D::Valve => match key {
            "open" => Ok(service(tile, "open_valve", json!({}))),
            "close" => Ok(service(tile, "close_valve", json!({}))),
            "stop" => Ok(service(tile, "stop_valve", json!({}))),
            "position" => Ok(service(
                tile,
                "set_valve_position",
                json!({"position":valid_number(argument,0.0,100.0,1.0)?}),
            )),
            _ => Err(ActionError::Unsupported),
        },
        D::Siren => match key {
            "turn_off" => Ok(service(tile, "turn_off", json!({}))),
            "turn_on" => {
                let allowed = ["tone", "volume_level", "duration"];
                let map = if argument.is_null() {
                    &Map::new()
                } else {
                    object(argument, &allowed, &[])?
                };
                let mut data = Map::new();
                if let Some(tone) = map.get("tone") {
                    let tone = valid_string(tone, &feature_list(tile, "available_tones"))?;
                    data.insert("tone".into(), json!(tone));
                }
                if let Some(volume) = map.get("volume_level") {
                    data.insert(
                        "volume_level".into(),
                        json!(valid_number(volume, 0.0, 1.0, 0.01)?),
                    );
                }
                if let Some(duration) = map.get("duration") {
                    data.insert(
                        "duration".into(),
                        json!(valid_number(duration, 1.0, 86_400.0, 1.0)?),
                    );
                }
                Ok(service(tile, "turn_on", Value::Object(data)))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::LawnMower => {
            let svc = match key {
                "start" => "start_mowing",
                "pause" => "pause",
                "stop" => "stop",
                "dock" => "dock",
                _ => return Err(ActionError::Unsupported),
            };
            Ok(service(tile, svc, json!({})))
        }
        D::Vacuum => match key {
            "start" => Ok(service(tile, "start", json!({}))),
            "pause" => Ok(service(tile, "pause", json!({}))),
            "stop" => Ok(service(tile, "stop", json!({}))),
            "return_home" => Ok(service(tile, "return_to_base", json!({}))),
            "locate" => Ok(service(tile, "locate", json!({}))),
            "clean_spot" => Ok(service(tile, "clean_spot", json!({}))),
            "clean_area" => Ok(service(
                tile,
                "clean_area",
                json!({"cleaning_area_id": vacuum_area_ids(argument)?}),
            )),
            "fan_speed" => Ok(service(
                tile,
                "set_fan_speed",
                json!({"fan_speed":valid_string(argument,&string_options(tile,"fan_speed_list"))?}),
            )),
            _ => Err(ActionError::Unsupported),
        },
        D::Text | D::InputText => {
            if key != "set_value" {
                return Err(ActionError::Unsupported);
            }
            let map = object(argument, &["value"], &["value"])?;
            let value = map
                .get("value")
                .and_then(Value::as_str)
                .ok_or(ActionError::InvalidValue)?;
            let min = format!(
                "{:.0}",
                numeric_attr(tile, &["min"])
                    .unwrap_or(0.0)
                    .clamp(0.0, 255.0)
            )
            .parse::<usize>()
            .map_err(|_| ActionError::InvalidValue)?;
            let max = bounded_text_length(
                numeric_attr(tile, &["max"])
                    .unwrap_or(255.0)
                    .clamp(1.0, 255.0),
            );
            if value.chars().count() < min || value.chars().count() > max {
                return Err(ActionError::InvalidValue);
            }
            Ok(service(tile, "set_value", json!({"value":value})))
        }
        D::Date => match key {
            "set_value" => {
                let map = object(argument, &["date"], &["date"])?;
                let date = required_text(map, "date", 10)?;
                if !valid_date(date) {
                    return Err(ActionError::InvalidValue);
                }
                Ok(service(tile, "set_value", json!({"date":date})))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::Time => match key {
            "set_value" => {
                let map = object(argument, &["time"], &["time"])?;
                let time_value = required_text(map, "time", 8)?;
                if !valid_time(time_value) {
                    return Err(ActionError::InvalidValue);
                }
                Ok(service(tile, "set_value", json!({"time":time_value})))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::Datetime => match key {
            "set_value" => {
                let map = object(argument, &["datetime"], &["datetime"])?;
                let datetime = required_text(map, "datetime", 64)?;
                if !valid_datetime(datetime) {
                    return Err(ActionError::InvalidValue);
                }
                Ok(service(tile, "set_value", json!({"datetime":datetime})))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::InputDatetime => match key {
            "set_datetime" => {
                let has_date = attribute(tile, "has_date")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let has_time = attribute(tile, "has_time")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let allowed: &[&str] = &["date", "time"];
                let required: &[&str] = if has_date && has_time {
                    &["date", "time"]
                } else if has_date {
                    &["date"]
                } else {
                    &["time"]
                };
                let map = object(argument, allowed, required)?;
                let mut data = Map::new();
                if has_date {
                    let date_value = required_text(map, "date", 10)?;
                    if !valid_date(date_value) {
                        return Err(ActionError::InvalidValue);
                    }
                    data.insert("date".into(), json!(date_value));
                }
                if has_time {
                    let time_value = required_text(map, "time", 8)?;
                    if !valid_time(time_value) {
                        return Err(ActionError::InvalidValue);
                    }
                    data.insert("time".into(), json!(time_value));
                }
                Ok(service(tile, "set_datetime", Value::Object(data)))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::Counter => match key {
            "increment" => Ok(service(tile, "increment", json!({}))),
            "decrement" => Ok(service(tile, "decrement", json!({}))),
            "reset" => Ok(service(tile, "reset", json!({}))),
            "set_value" | "set_value_exact" => {
                let (min, max) = range(tile, "minimum", "maximum", -1_000_000.0, 1_000_000.0);
                let step = numeric_attr(tile, &["step"]).unwrap_or(1.0).abs().max(1.0);
                let (value, exact) = if key == "set_value_exact" {
                    let map = object(argument, &["value"], &["value"])?;
                    (
                        valid_number(&map["value"], min, max, step)?,
                        map["value"].to_string(),
                    )
                } else {
                    (
                        valid_number(argument, min, max, step)?,
                        argument.to_string(),
                    )
                };
                let integer = valid_signed_integer(
                    &json!(value),
                    signed_integer_bound(min)?,
                    signed_integer_bound(max)?,
                )?;
                Ok((
                    ActionKind::SetNumberExact(exact),
                    ServiceCall::new("counter", "set_value", json!({"value":integer})),
                ))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::Timer => match key {
            "start" => {
                let map = object(argument, &["duration"], &[])?;
                let duration = optional_text(map, "duration", 16)?;
                if duration
                    .as_deref()
                    .is_some_and(|duration| !valid_duration(duration, false))
                {
                    return Err(ActionError::InvalidValue);
                }
                let data =
                    duration.map_or_else(|| json!({}), |duration| json!({"duration":duration}));
                Ok(service(tile, "start", data))
            }
            "cancel" => Ok(service(tile, "cancel", json!({}))),
            "pause" => Ok(service(tile, "pause", json!({}))),
            "finish" => Ok(service(tile, "finish", json!({}))),
            "change" => {
                let map = object(argument, &["duration"], &["duration"])?;
                let d = required_text(map, "duration", 16)?;
                if !valid_duration(d, true) {
                    return Err(ActionError::InvalidValue);
                }
                Ok(service(tile, "change", json!({"duration":d})))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::Update => match key {
            "install" => Ok(service(tile, "install", json!({}))),
            "install_version" => {
                let map = object(argument, &["version"], &["version"])?;
                let version = required_text(map, "version", 64)?;
                Ok(service(tile, "install", json!({"version":version})))
            }
            "install_with_backup" => Ok(service(
                tile,
                "install",
                json!({"backup":argument.as_bool().ok_or(ActionError::InvalidValue)?}),
            )),
            "skip" => Ok(service(tile, "skip", json!({}))),
            "clear_skipped" => Ok(service(tile, "clear_skipped", json!({}))),
            "release_notes" => Ok(service(tile, "release_notes", json!({}))),
            _ => Err(ActionError::Unsupported),
        },
        D::Weather => {
            if key != "get_forecasts" {
                return Err(ActionError::Unsupported);
            }
            let forecast_type = valid_string(
                argument,
                &["daily".into(), "hourly".into(), "twice_daily".into()],
            )?;
            Ok(service(
                tile,
                "get_forecasts",
                json!({"type":forecast_type}),
            ))
        }
        D::Calendar => match key {
            "get_events" => {
                let map = object(
                    argument,
                    &["start_date_time", "end_date_time", "duration"],
                    &[],
                )?;
                let start =
                    optional_text(map, "start_date_time", 64)?.filter(|value| !value.is_empty());
                let end =
                    optional_text(map, "end_date_time", 64)?.filter(|value| !value.is_empty());
                let duration =
                    optional_text(map, "duration", 16)?.filter(|value| !value.is_empty());
                if end.is_some() == duration.is_some()
                    || start.as_deref().is_some_and(|value| !valid_datetime(value))
                    || end.as_deref().is_some_and(|value| !valid_datetime(value))
                    || start
                        .as_deref()
                        .zip(end.as_deref())
                        .is_some_and(|(start, end)| start >= end)
                    || duration
                        .as_deref()
                        .is_some_and(|value| !valid_duration(value, false))
                {
                    return Err(ActionError::InvalidValue);
                }
                let mut data = json!({});
                if let Some(start) = start {
                    data["start_date_time"] = json!(start);
                }
                if let Some(end) = end {
                    data["end_date_time"] = json!(end);
                }
                if let Some(duration) = duration {
                    data["duration"] = json!(duration);
                }
                Ok(service(tile, "get_events", data))
            }
            "create_event" => {
                let map = object(
                    argument,
                    &[
                        "summary",
                        "start_date_time",
                        "end_date_time",
                        "start_date",
                        "end_date",
                        "description",
                        "location",
                    ],
                    &["summary"],
                )?;
                let summary = required_text(map, "summary", 255)?;
                let start_datetime =
                    optional_text(map, "start_date_time", 64)?.filter(|value| !value.is_empty());
                let end_datetime =
                    optional_text(map, "end_date_time", 64)?.filter(|value| !value.is_empty());
                let start_date =
                    optional_text(map, "start_date", 10)?.filter(|value| !value.is_empty());
                let end_date =
                    optional_text(map, "end_date", 10)?.filter(|value| !value.is_empty());
                let has_datetime = start_datetime.is_some() || end_datetime.is_some();
                let has_date = start_date.is_some() || end_date.is_some();
                if has_datetime == has_date {
                    return Err(ActionError::InvalidValue);
                }
                let mut data = json!({"summary":summary});
                if has_datetime {
                    let (Some(start), Some(end)) = (start_datetime, end_datetime) else {
                        return Err(ActionError::InvalidValue);
                    };
                    if !valid_datetime(&start) || !valid_datetime(&end) || start >= end {
                        return Err(ActionError::InvalidValue);
                    }
                    data["start_date_time"] = json!(start);
                    data["end_date_time"] = json!(end);
                } else {
                    let (Some(start), Some(end)) = (start_date, end_date) else {
                        return Err(ActionError::InvalidValue);
                    };
                    if !valid_date(&start) || !valid_date(&end) || start >= end {
                        return Err(ActionError::InvalidValue);
                    }
                    data["start_date"] = json!(start);
                    data["end_date"] = json!(end);
                }
                let description = optional_text(map, "description", 4096)?;
                let location = optional_text(map, "location", 255)?;
                if let Some(value) = description {
                    data["description"] = json!(value);
                }
                if let Some(value) = location {
                    data["location"] = json!(value);
                }
                Ok(service(tile, "create_event", data))
            }
            "create_event_relative" => {
                let map = object(
                    argument,
                    &[
                        "summary",
                        "relative_unit",
                        "relative_value",
                        "description",
                        "location",
                    ],
                    &["summary", "relative_unit", "relative_value"],
                )?;
                let unit = valid_string(&map["relative_unit"], &["days".into(), "weeks".into()])?;
                let relative_value = valid_integer(&map["relative_value"], 1, 3650)?;
                let mut data = json!({
                    "summary":required_text(map,"summary",255)?,
                    "in":{unit:relative_value},
                });
                for (field, limit) in [("description", 4096), ("location", 255)] {
                    if let Some(value) =
                        optional_text(map, field, limit)?.filter(|value| !value.is_empty())
                    {
                        data[field] = json!(value);
                    }
                }
                Ok(service(tile, "create_event", data))
            }
            "delete_event" => {
                let map = object(
                    argument,
                    &["uid", "recurrence_id", "recurrence_range"],
                    &["uid"],
                )?;
                let mut data = json!({"uid":required_text(map,"uid",255)?});
                for field in ["recurrence_id", "recurrence_range"] {
                    if let Some(value) =
                        optional_text(map, field, 255)?.filter(|value| !value.is_empty())
                    {
                        data[field] = json!(value);
                    }
                }
                Ok(service(tile, "delete_event", data))
            }
            "update_event" => {
                let map = object(
                    argument,
                    &[
                        "uid",
                        "recurrence_id",
                        "recurrence_range",
                        "summary",
                        "start",
                        "end",
                        "description",
                        "location",
                        "rrule",
                    ],
                    &["uid", "summary", "start", "end"],
                )?;
                let start = required_text(map, "start", 64)?;
                let end = required_text(map, "end", 64)?;
                let dates = valid_date(start) && valid_date(end);
                let date_times = valid_datetime(start) && valid_datetime(end);
                if (!dates && !date_times) || start >= end {
                    return Err(ActionError::InvalidValue);
                }
                let mut event = json!({
                    "summary":required_text(map,"summary",255)?,
                    "start":start,
                    "end":end,
                });
                for (field, limit) in [("description", 4096), ("location", 255), ("rrule", 1024)] {
                    if let Some(value) =
                        optional_text(map, field, limit)?.filter(|value| !value.is_empty())
                    {
                        event[field] = json!(value);
                    }
                }
                let mut data = json!({
                    "uid":required_text(map,"uid",255)?,
                    "event":event,
                });
                for field in ["recurrence_id", "recurrence_range"] {
                    if let Some(value) =
                        optional_text(map, field, 255)?.filter(|value| !value.is_empty())
                    {
                        data[field] = json!(value);
                    }
                }
                Ok(service(tile, "update_event", data))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::Todo => match key {
            "get_items" => Ok(service(tile, "get_items", json!({}))),
            "remove_completed_items" => Ok(service(tile, "remove_completed_items", json!({}))),
            "move_item" => {
                let map = object(argument, &["uid", "previous_uid"], &["uid"])?;
                let uid = required_text(map, "uid", 255)?;
                let previous_uid = optional_text(map, "previous_uid", 255)?;
                if previous_uid.as_deref() == Some(uid) {
                    return Err(ActionError::InvalidValue);
                }
                let mut data = json!({"uid":uid});
                if let Some(previous_uid) = previous_uid.filter(|value| !value.is_empty()) {
                    data["previous_uid"] = json!(previous_uid);
                }
                Ok(service(tile, "move_item", data))
            }
            "add_item" => {
                let allowed = ["item", "due_date", "due_datetime", "description"];
                let map = object(argument, &allowed, &["item"])?;
                let mut data = json!({"item":required_text(map,"item",255)?});
                let destination = data.as_object_mut().ok_or(ActionError::InvalidValue)?;
                add_todo_optional_fields(tile, map, destination)?;
                Ok(service(tile, "add_item", data))
            }
            "update_item" => {
                let map = object(
                    argument,
                    &[
                        "item",
                        "status",
                        "rename",
                        "due_date",
                        "due_datetime",
                        "description",
                    ],
                    &["item"],
                )?;
                let item = required_text(map, "item", 255)?;
                let status = map
                    .get("status")
                    .map(|status| {
                        valid_string(status, &["needs_action".into(), "completed".into()])
                    })
                    .transpose()?;
                let rename = optional_text(map, "rename", 255)?;
                let mut data = json!({"item":item});
                if let Some(status) = status {
                    data["status"] = json!(status);
                }
                if let Some(rename) = &rename {
                    data["rename"] = json!(rename);
                }
                let destination = data.as_object_mut().ok_or(ActionError::InvalidValue)?;
                let changed = status.is_some()
                    || rename.is_some()
                    || add_todo_optional_fields(tile, map, destination)?;
                if !changed {
                    return Err(ActionError::InvalidValue);
                }
                Ok(service(tile, "update_item", data))
            }
            "remove_item" => {
                let map = object(argument, &["item"], &["item"])?;
                Ok(service(
                    tile,
                    "remove_item",
                    json!({"item":required_text(map,"item",255)?}),
                ))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::Notify => {
            let allowed = if feature(tile, NOTIFY_TITLE) {
                &["message", "title"][..]
            } else {
                &["message"][..]
            };
            let map = object(argument, allowed, &["message"])?;
            let mut data = json!({"message":required_text(map,"message",4096)?});
            if let Some(title) = optional_text(map, "title", 255)? {
                data["title"] = json!(title);
            }
            Ok(service(tile, "send_message", data))
        }
        D::Tts => {
            let map = object(
                argument,
                &["media_player_entity_id", "message", "language", "cache"],
                &["media_player_entity_id", "message"],
            )?;
            let player = required_text(map, "media_player_entity_id", 255)?;
            if !player.starts_with("media_player.") {
                return Err(ActionError::InvalidValue);
            }
            let message = required_text(map, "message", 4096)?;
            let mut data = json!({"media_player_entity_id":player,"message":message});
            if let Some(language) =
                optional_text(map, "language", 64)?.filter(|value| !value.is_empty())
            {
                data["language"] = json!(language);
            }
            if let Some(cache) = map.get("cache") {
                data["cache"] = json!(cache.as_bool().ok_or(ActionError::InvalidValue)?);
            }
            Ok((ActionKind::RunScene, ServiceCall::new("tts", "speak", data)))
        }
        D::Conversation => {
            let map = object(argument, &["text"], &["text"])?;
            Ok(service(
                tile,
                "process",
                json!({"text":required_text(map,"text",4096)?}),
            ))
        }
        D::AssistSatellite => {
            let map = object(argument, &["message"], &["message"])?;
            Ok(service(
                tile,
                "announce",
                json!({"message":required_text(map,"message",4096)?}),
            ))
        }
        D::Remote => match key {
            "turn_on" => Ok(service(tile, "turn_on", json!({}))),
            "turn_off" => Ok(service(tile, "turn_off", json!({}))),
            "toggle" => Ok(service(tile, "toggle", json!({}))),
            "activity" => Ok(service(
                tile,
                "turn_on",
                json!({"activity":valid_string(argument,&string_options(tile,"activity_list"))?}),
            )),
            "send_command" => {
                let allowed = [
                    "command",
                    "device",
                    "num_repeats",
                    "delay_secs",
                    "hold_secs",
                ];
                let map = object(argument, &allowed, &["command"])?;
                let command =
                    command_list(map, "command", true)?.ok_or(ActionError::InvalidValue)?;
                let mut data = json!({"command":command});
                if let Some(device) = optional_text(map, "device", 255)? {
                    data["device"] = json!(device);
                }
                if let Some(repeats) = map.get("num_repeats") {
                    data["num_repeats"] = json!(valid_integer(repeats, 0, 255)?);
                }
                if let Some(delay) = map.get("delay_secs") {
                    data["delay_secs"] = json!(valid_number(delay, 0.0, 60.0, 0.1)?);
                }
                if let Some(hold) = map.get("hold_secs") {
                    data["hold_secs"] = json!(valid_number(hold, 0.0, 60.0, 0.1)?);
                }
                Ok(service(tile, "send_command", data))
            }
            "learn_command" => {
                let map = object(
                    argument,
                    &[
                        "device",
                        "command",
                        "command_type",
                        "alternative",
                        "timeout",
                    ],
                    &[],
                )?;
                let mut data = Map::new();
                if let Some(device) = optional_text(map, "device", 255)? {
                    data.insert("device".into(), json!(device));
                }
                if let Some(command) = command_list(map, "command", false)? {
                    data.insert("command".into(), json!(command));
                }
                if let Some(command_type) = map.get("command_type") {
                    data.insert(
                        "command_type".into(),
                        json!(valid_string(command_type, &["ir".into(), "rf".into()])?),
                    );
                }
                if let Some(alternative) = map.get("alternative") {
                    data.insert(
                        "alternative".into(),
                        json!(alternative.as_bool().ok_or(ActionError::InvalidValue)?),
                    );
                }
                if let Some(timeout) = map.get("timeout") {
                    data.insert("timeout".into(), json!(valid_integer(timeout, 0, 60)?));
                }
                Ok(service(tile, "learn_command", Value::Object(data)))
            }
            "delete_command" => {
                let map = object(argument, &["device", "command"], &["command"])?;
                let commands =
                    command_list(map, "command", true)?.ok_or(ActionError::InvalidValue)?;
                let mut data = json!({"command":commands});
                if let Some(device) = optional_text(map, "device", 255)? {
                    data["device"] = json!(device);
                }
                Ok(service(tile, "delete_command", data))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::Camera => match key {
            "turn_on" | "turn_off" | "enable_motion_detection" | "disable_motion_detection" => {
                if !argument.is_null() && !argument.as_object().is_some_and(Map::is_empty) {
                    return Err(ActionError::InvalidValue);
                }
                let service_name = match key {
                    "enable_motion_detection" => "enable_motion_detection",
                    "disable_motion_detection" => "disable_motion_detection",
                    "turn_on" => "turn_on",
                    _ => "turn_off",
                };
                Ok(service(tile, service_name, json!({})))
            }
            "snapshot" => {
                let map = object(argument, &["filename"], &["filename"])?;
                Ok(service(
                    tile,
                    "snapshot",
                    json!({"filename":required_text(map,"filename",512)?}),
                ))
            }
            "record" => {
                let map = object(
                    argument,
                    &["filename", "duration", "lookback"],
                    &["filename"],
                )?;
                let mut data = json!({"filename":required_text(map,"filename",512)?});
                if let Some(duration) = map.get("duration") {
                    data["duration"] = json!(valid_integer(duration, 1, 86_400)?);
                }
                if let Some(lookback) = map.get("lookback") {
                    data["lookback"] = json!(valid_integer(lookback, 0, 300)?);
                }
                Ok(service(tile, "record", data))
            }
            "play_stream" => {
                let map = object(argument, &["media_player"], &["media_player"])?;
                let media_player = required_text(map, "media_player", 255)?;
                if !media_player.starts_with("media_player.") {
                    return Err(ActionError::InvalidValue);
                }
                Ok(service(
                    tile,
                    "play_stream",
                    json!({"media_player":media_player}),
                ))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::Group => match key {
            "turn_on" | "turn_off" | "toggle" => {
                if !argument.is_null() && !argument.as_object().is_some_and(Map::is_empty) {
                    return Err(ActionError::InvalidValue);
                }
                let service_name = match key {
                    "turn_on" => "turn_on",
                    "turn_off" => "turn_off",
                    _ => "toggle",
                };
                Ok((
                    ActionKind::RunScene,
                    ServiceCall::new("homeassistant", service_name, json!({})),
                ))
            }
            _ => Err(ActionError::Unsupported),
        },
        D::Image if key == "snapshot" => {
            let map = object(argument, &["filename"], &["filename"])?;
            Ok(service(
                tile,
                "snapshot",
                json!({"filename":required_text(map,"filename",512)?}),
            ))
        }
        D::ImageProcessing if key == "scan" => Ok(service(tile, "scan", json!({}))),
        D::Schedule if key == "get_schedule" => Ok(service(tile, "get_schedule", json!({}))),
        _ => Err(ActionError::Unsupported),
    }
}

fn valid_date(value: &str) -> bool {
    if !value.is_ascii()
        || value.len() != 10
        || value.as_bytes().get(4) != Some(&b'-')
        || value.as_bytes().get(7) != Some(&b'-')
    {
        return false;
    }
    let mut parts = value.split('-');
    let (Some(y), Some(m), Some(d), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Ok(year), Ok(month), Ok(day)) = (y.parse::<u32>(), m.parse::<u32>(), d.parse::<u32>())
    else {
        return false;
    };
    if year == 0 || !(1..=12).contains(&month) {
        return false;
    }
    let leap_year = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap_year => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=days_in_month).contains(&day)
}

fn valid_time(value: &str) -> bool {
    if !value.is_ascii()
        || value.len() != 8
        || value.as_bytes().get(2) != Some(&b':')
        || value.as_bytes().get(5) != Some(&b':')
    {
        return false;
    }
    let mut parts = value.split(':');
    let (Some(h), Some(m), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Ok(hour), Ok(minute), Ok(second)) = (h.parse::<u32>(), m.parse::<u32>(), s.parse::<u32>())
    else {
        return false;
    };
    hour < 24 && minute < 60 && second < 60
}

fn valid_datetime(value: &str) -> bool {
    if !value.is_ascii() || value.len() < 19 || value.len() > 64 || !valid_date(&value[..10]) {
        return false;
    }
    let bytes = value.as_bytes();
    if !matches!(bytes[10], b'T' | b' ') || !valid_time(&value[11..19]) {
        return false;
    }
    let timezone = &value[19..];
    if timezone.is_empty() || timezone == "Z" {
        return true;
    }
    if timezone.len() != 6
        || !matches!(timezone.as_bytes()[0], b'+' | b'-')
        || timezone.as_bytes()[3] != b':'
    {
        return false;
    }
    let Ok(hours) = timezone[1..3].parse::<u32>() else {
        return false;
    };
    let Ok(minutes) = timezone[4..6].parse::<u32>() else {
        return false;
    };
    hours <= 23 && minutes < 60
}

fn valid_duration(value: &str, allow_negative: bool) -> bool {
    let (negative, value) = value
        .strip_prefix('-')
        .map_or((false, value), |value| (true, value));
    if negative && !allow_negative {
        return false;
    }
    if let Ok(seconds) = value.parse::<u64>() {
        return seconds > 0 || allow_negative;
    }
    let mut parts = value.split(':');
    let (Some(hours), Some(minutes), Some(seconds), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Ok(_hours), Ok(minutes), Ok(seconds)) = (
        hours.parse::<u64>(),
        minutes.parse::<u32>(),
        seconds.parse::<u32>(),
    ) else {
        return false;
    };
    minutes < 60 && seconds < 60
}
