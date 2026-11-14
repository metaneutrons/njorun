//! Service calls and the controls that produce them.
//!
//! The core decides *what* to ask Home Assistant for; the front ends only send the
//! resulting [`ServiceCall`] with the entity as target, so the simulator and the device
//! cannot drift apart.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ActionError, ActionKind, EntityDomain, EntityTile, LockTarget, PlaybackTarget};

/// A concrete Home Assistant service call, without its target entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCall {
    pub domain: &'static str,
    pub service: &'static str,
    /// Extra service data as a JSON object; the front end adds `target.entity_id`.
    pub data: Value,
}

impl ServiceCall {
    pub(crate) const fn new(domain: &'static str, service: &'static str, data: Value) -> Self {
        Self {
            domain,
            service,
            data,
        }
    }

    /// The `call_service` WebSocket message for this call, with a request id.
    #[must_use]
    pub fn message(&self, id: u64, entity_id: &str) -> Value {
        let mut message = self.payload(entity_id);
        message["id"] = json!(id);
        message
    }

    /// The `call_service` message without a request id, for clients that assign their own.
    #[must_use]
    pub fn payload(&self, entity_id: &str) -> Value {
        if self.domain == "media_player" && matches!(self.service, "browse_media" | "search_media")
        {
            let mut message = json!({
                "type": format!("media_player/{}", self.service),
                "entity_id": entity_id,
            });
            if let (Some(message), Some(data)) = (message.as_object_mut(), self.data.as_object()) {
                message.extend(data.clone());
            }
            return message;
        }
        if self.domain == "todo" && self.service == "move_item" {
            let mut message = json!({"type":"todo/item/move", "entity_id":entity_id});
            if let (Some(message), Some(data)) = (message.as_object_mut(), self.data.as_object()) {
                message.extend(data.clone());
            }
            return message;
        }
        if self.domain == "calendar" && matches!(self.service, "delete_event" | "update_event") {
            let mut message = json!({
                "type": format!("calendar/event/{}", self.service.trim_end_matches("_event")),
                "entity_id": entity_id,
            });
            if let (Some(message), Some(data)) = (message.as_object_mut(), self.data.as_object()) {
                message.extend(data.clone());
            }
            return message;
        }
        if self.domain == "update" && self.service == "release_notes" {
            return json!({"type":"update/release_notes", "entity_id":entity_id});
        }
        if self.domain == "history" && self.service == "history_during_period" {
            let mut message = json!({
                "type": "history/history_during_period",
                "entity_ids": [entity_id],
            });
            if let (Some(message), Some(data)) = (message.as_object_mut(), self.data.as_object()) {
                message.extend(data.clone());
            }
            return message;
        }
        let mut message = json!({
            "type": "call_service",
            "domain": self.domain,
            "service": self.service,
            "target": {"entity_id": entity_id},
        });
        if let Some(data) = self.data.as_object().filter(|data| !data.is_empty()) {
            message["service_data"] = Value::Object(data.clone());
        }
        if (self.domain == "ai_task"
            || self.domain == "conversation"
            || (self.domain == "assist_satellite" && self.service == "ask_question"))
            && let Some(object) = message.as_object_mut()
        {
            object.remove("target");
        }
        if self.response_required() {
            message["return_response"] = Value::Bool(true);
        }
        message
    }

    /// Whether the service call expects a structured Home Assistant result.
    #[must_use]
    pub fn response_required(&self) -> bool {
        matches!(
            (self.domain, self.service),
            ("weather", "get_forecasts")
                | ("calendar", "get_events")
                | ("todo", "get_items")
                | ("schedule", "get_schedule")
                | ("update", "release_notes")
                | ("media_player", "browse_media" | "search_media")
                | ("history", "history_during_period")
                | ("ai_task", "generate_data" | "generate_image" | "evaluate")
                | ("assist_satellite", "ask_question")
                | ("conversation", "process")
        )
    }
}

/// Alarm control panel arming modes, in the order the details sheet lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlarmMode {
    Home,
    Away,
    Night,
    Vacation,
    CustomBypass,
}

impl AlarmMode {
    pub const ALL: [Self; 5] = [
        Self::Home,
        Self::Away,
        Self::Night,
        Self::Vacation,
        Self::CustomBypass,
    ];

    #[must_use]
    pub const fn service(self) -> &'static str {
        match self {
            Self::Home => "alarm_arm_home",
            Self::Away => "alarm_arm_away",
            Self::Night => "alarm_arm_night",
            Self::Vacation => "alarm_arm_vacation",
            Self::CustomBypass => "alarm_arm_custom_bypass",
        }
    }

    /// The panel state that confirms the mode.
    #[must_use]
    pub const fn state(self) -> &'static str {
        match self {
            Self::Home => "armed_home",
            Self::Away => "armed_away",
            Self::Night => "armed_night",
            Self::Vacation => "armed_vacation",
            Self::CustomBypass => "armed_custom_bypass",
        }
    }

    /// Bit in the entity's `supported_features` attribute.
    #[must_use]
    pub const fn feature_bit(self) -> u64 {
        match self {
            Self::Home => 1,
            Self::Away => 2,
            Self::Night => 4,
            Self::Vacation => 32,
            Self::CustomBypass => 16,
        }
    }

    /// Stable key used by front ends to name the action.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Home => "arm_home",
            Self::Away => "arm_away",
            Self::Night => "arm_night",
            Self::Vacation => "arm_vacation",
            Self::CustomBypass => "arm_custom_bypass",
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.key() == key)
    }
}

/// An action chosen in the details sheet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetailAction {
    SetBrightness(u8),
    SetFanSpeed(u8),
    SetVolume(u8),
    SetNumber(f64),
    SelectOption(String),
    Lock,
    Unlock,
    PlayPause,
    NextTrack,
    PreviousTrack,
    Trigger,
    AlarmArm {
        mode: AlarmMode,
        code: Option<String>,
    },
    AlarmDisarm {
        code: Option<String>,
    },
    /// Capability-aware operation keyed by the shared entity catalog.
    Entity {
        key: String,
        argument: Value,
    },
}

impl DetailAction {
    /// Builds the action a front end reported as `key` with `argument`, or `None` when the
    /// key is not an action for this domain (for example `refresh_camera`).
    #[must_use]
    pub fn from_ui(key: &str, argument: &str, domain: EntityDomain) -> Option<Self> {
        if let Some(key) = key.strip_prefix("entity:") {
            if key.is_empty() {
                return None;
            }
            return Some(Self::Entity {
                key: key.to_owned(),
                argument: serde_json::from_str(argument)
                    .unwrap_or_else(|_| Value::String(argument.to_owned())),
            });
        }
        let code = || (!argument.is_empty()).then(|| argument.to_owned());
        if let Some(mode) = AlarmMode::from_key(key) {
            return Some(Self::AlarmArm { mode, code: code() });
        }
        let level = || {
            argument
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let percent = || level().map(|value| value.round().clamp(0.0, 100.0) as u8);
        match key {
            "level" => match domain {
                EntityDomain::Light => percent().map(Self::SetBrightness),
                EntityDomain::Fan => percent().map(Self::SetFanSpeed),
                EntityDomain::MediaPlayer => percent().map(Self::SetVolume),
                EntityDomain::Number | EntityDomain::InputNumber => level().map(Self::SetNumber),
                _ => None,
            },
            "option" => Some(Self::SelectOption(argument.to_owned())),
            "lock" => Some(Self::Lock),
            "unlock" => Some(Self::Unlock),
            "play_pause" => Some(Self::PlayPause),
            "next" => Some(Self::NextTrack),
            "previous" => Some(Self::PreviousTrack),
            "trigger" => Some(Self::Trigger),
            "disarm" => Some(Self::AlarmDisarm { code: code() }),
            _ => None,
        }
    }
}

const MAX_OPTION_CHARS: usize = 255;
const MAX_CODE_CHARS: usize = 32;
const FAN_FEATURE_SET_SPEED: u64 = 1;
pub(crate) const MEDIA_FEATURE_PAUSE: u64 = 1;
const MEDIA_FEATURE_PREVIOUS_TRACK: u64 = 16;
const MEDIA_FEATURE_NEXT_TRACK: u64 = 32;
const MEDIA_FEATURE_VOLUME_SET: u64 = 4;
pub(crate) const MEDIA_FEATURE_PLAY: u64 = 16_384;

fn attribute<'a>(tile: &'a EntityTile, key: &str) -> Option<&'a Value> {
    tile.state.as_ref()?.attributes.get(key)
}

fn supports_light_brightness(tile: &EntityTile) -> bool {
    if let Some(modes) = attribute(tile, "supported_color_modes") {
        return modes.as_array().is_some_and(|modes| {
            modes
                .iter()
                .filter_map(Value::as_str)
                .any(|mode| mode != "onoff")
        });
    }
    attribute(tile, "brightness")
        .and_then(Value::as_f64)
        .is_some()
}

fn supports_fan_speed(tile: &EntityTile) -> bool {
    if let Some(features) = attribute(tile, "supported_features") {
        return features
            .as_u64()
            .is_some_and(|features| features & FAN_FEATURE_SET_SPEED != 0);
    }
    attribute(tile, "percentage")
        .and_then(Value::as_f64)
        .is_some()
}

fn supports_media_feature(tile: &EntityTile, feature: u64, fallback_attribute: &str) -> bool {
    if let Some(features) = attribute(tile, "supported_features") {
        return features
            .as_u64()
            .is_some_and(|features| features & feature != 0);
    }
    if fallback_attribute.is_empty() {
        // Older HA integrations omit supported_features. Preserve their play/pause and
        // track-step behaviour; explicit feature masks above always take precedence.
        return true;
    }
    attribute(tile, fallback_attribute).is_some()
}

fn validate_number(tile: &EntityTile, value: f64) -> Result<(), ActionError> {
    let Some(state) = tile.state.as_ref() else {
        return Err(ActionError::Unavailable);
    };
    let numeric_attribute = |key: &str| -> Result<Option<f64>, ActionError> {
        state.attributes.get(key).map_or(Ok(None), |value| {
            value
                .as_f64()
                .filter(|value| value.is_finite())
                .map(Some)
                .ok_or(ActionError::InvalidValue)
        })
    };
    let minimum = numeric_attribute("min")?;
    let maximum = numeric_attribute("max")?;
    let step = numeric_attribute("step")?;
    if minimum.zip(maximum).is_some_and(|(min, max)| min > max)
        || minimum.is_some_and(|min| value < min)
        || maximum.is_some_and(|max| value > max)
    {
        return Err(ActionError::InvalidValue);
    }
    if let Some(step) = step {
        if step <= 0.0 {
            return Err(ActionError::InvalidValue);
        }
        let base = minimum.unwrap_or(0.0);
        let increments = (value - base) / step;
        let nearest = increments.round();
        // The tolerance covers only binary floating point error in the division.
        let magnitude = value.abs().max(base.abs()).max(1.0);
        if step <= f64::EPSILON * magnitude * 8.0 {
            return Err(ActionError::InvalidValue);
        }
        let tolerance = (f64::EPSILON * magnitude / step)
            .mul_add(2.0, f64::EPSILON * increments.abs().max(1.0) * 8.0);
        if (increments - nearest).abs() > tolerance {
            return Err(ActionError::InvalidValue);
        }
    }
    Ok(())
}

fn code_data(code: Option<&String>) -> Result<Value, ActionError> {
    match code {
        Some(code) if code.chars().count() > MAX_CODE_CHARS => Err(ActionError::InvalidValue),
        Some(code) if !code.is_empty() => Ok(json!({"code": code})),
        _ => Ok(json!({})),
    }
}

impl DetailAction {
    /// Validates the action for this tile and builds the call plus the state that confirms it.
    pub(crate) fn plan(&self, tile: &EntityTile) -> Result<(ActionKind, ServiceCall), ActionError> {
        let domain = tile.domain;
        match self {
            Self::SetBrightness(_)
            | Self::SetFanSpeed(_)
            | Self::SetVolume(_)
            | Self::SetNumber(_)
            | Self::SelectOption(_) => self.plan_value(tile),
            Self::Entity { key, argument } => crate::controls::plan(tile, key, argument),
            Self::Lock | Self::Unlock => self.plan_lock(tile),
            Self::PlayPause | Self::NextTrack | Self::PreviousTrack => {
                self.plan_media(domain, tile)
            }
            Self::Trigger => {
                if domain != EntityDomain::Automation {
                    return Err(ActionError::Unsupported);
                }
                Ok((
                    ActionKind::RunScene,
                    ServiceCall::new("automation", "trigger", json!({"skip_condition": false})),
                ))
            }
            Self::AlarmArm { .. } | Self::AlarmDisarm { .. } => self.plan_alarm(tile),
        }
    }

    fn plan_value(&self, tile: &EntityTile) -> Result<(ActionKind, ServiceCall), ActionError> {
        let domain = tile.domain;
        match self {
            Self::SetBrightness(percent) if domain == EntityDomain::Light => {
                if !supports_light_brightness(tile) {
                    return Err(ActionError::Unsupported);
                }
                let percent = (*percent).clamp(1, 100);
                Ok((
                    ActionKind::SetBrightness(percent),
                    ServiceCall::new("light", "turn_on", json!({"brightness_pct": percent})),
                ))
            }
            Self::SetFanSpeed(percent) if domain == EntityDomain::Fan => {
                if !supports_fan_speed(tile) {
                    return Err(ActionError::Unsupported);
                }
                let percent = (*percent).min(100);
                Ok((
                    ActionKind::SetFanSpeed(percent),
                    ServiceCall::new("fan", "set_percentage", json!({"percentage": percent})),
                ))
            }
            Self::SetVolume(percent) if domain == EntityDomain::MediaPlayer => {
                if !supports_media_feature(tile, MEDIA_FEATURE_VOLUME_SET, "volume_level") {
                    return Err(ActionError::Unsupported);
                }
                let percent = (*percent).min(100);
                Ok((
                    ActionKind::SetVolume(percent),
                    ServiceCall::new(
                        "media_player",
                        "volume_set",
                        json!({"volume_level": f64::from(percent) / 100.0}),
                    ),
                ))
            }
            Self::SetNumber(value)
                if matches!(domain, EntityDomain::Number | EntityDomain::InputNumber) =>
            {
                if !value.is_finite() {
                    return Err(ActionError::InvalidValue);
                }
                validate_number(tile, *value)?;
                Ok((
                    ActionKind::SetNumberExact(value.to_string()),
                    ServiceCall::new(
                        domain.service_domain(),
                        "set_value",
                        json!({"value": value}),
                    ),
                ))
            }
            Self::SelectOption(option)
                if matches!(domain, EntityDomain::Select | EntityDomain::InputSelect) =>
            {
                if option.is_empty() || option.chars().count() > MAX_OPTION_CHARS {
                    return Err(ActionError::InvalidValue);
                }
                if !attribute(tile, "options")
                    .and_then(Value::as_array)
                    .is_some_and(|options| {
                        options.iter().any(|value| value.as_str() == Some(option))
                    })
                {
                    return Err(ActionError::InvalidValue);
                }
                Ok((
                    ActionKind::SelectOption(option.clone()),
                    ServiceCall::new(
                        domain.service_domain(),
                        "select_option",
                        json!({"option": option}),
                    ),
                ))
            }
            _ => Err(ActionError::Unsupported),
        }
    }

    fn plan_lock(&self, tile: &EntityTile) -> Result<(ActionKind, ServiceCall), ActionError> {
        let domain = tile.domain;
        if domain != EntityDomain::Lock {
            return Err(ActionError::Unsupported);
        }
        if attribute(tile, "code_format")
            .and_then(Value::as_str)
            .is_some_and(|format| !format.is_empty())
        {
            return Err(ActionError::InvalidValue);
        }
        let (target, service) = if *self == Self::Lock {
            (LockTarget::Locked, "lock")
        } else {
            (LockTarget::Unlocked, "unlock")
        };
        Ok((
            ActionKind::Lock(target),
            ServiceCall::new("lock", service, json!({})),
        ))
    }

    fn plan_media(
        &self,
        domain: EntityDomain,
        tile: &EntityTile,
    ) -> Result<(ActionKind, ServiceCall), ActionError> {
        if domain != EntityDomain::MediaPlayer {
            return Err(ActionError::Unsupported);
        }
        let (kind, service) = match self {
            Self::NextTrack => {
                if !supports_media_feature(tile, MEDIA_FEATURE_NEXT_TRACK, "") {
                    return Err(ActionError::Unsupported);
                }
                (ActionKind::RunScene, "media_next_track")
            }
            Self::PreviousTrack => {
                if !supports_media_feature(tile, MEDIA_FEATURE_PREVIOUS_TRACK, "") {
                    return Err(ActionError::Unsupported);
                }
                (ActionKind::RunScene, "media_previous_track")
            }
            _ => {
                let (target, feature) = match tile.state_value() {
                    Some("playing" | "buffering") => (PlaybackTarget::Paused, MEDIA_FEATURE_PAUSE),
                    Some("paused" | "idle" | "on" | "standby") => {
                        (PlaybackTarget::Playing, MEDIA_FEATURE_PLAY)
                    }
                    _ => return Err(ActionError::Unsupported),
                };
                if !supports_media_feature(tile, feature, "") {
                    return Err(ActionError::Unsupported);
                }
                (ActionKind::Playback(target), "media_play_pause")
            }
        };
        Ok((kind, ServiceCall::new("media_player", service, json!({}))))
    }

    fn plan_alarm(&self, tile: &EntityTile) -> Result<(ActionKind, ServiceCall), ActionError> {
        let domain = tile.domain;
        if domain != EntityDomain::AlarmControlPanel {
            return Err(ActionError::Unsupported);
        }
        match self {
            Self::AlarmArm { mode, code } => {
                if let Some(features) = attribute(tile, "supported_features")
                    && features.as_u64().ok_or(ActionError::InvalidValue)? & mode.feature_bit() == 0
                {
                    return Err(ActionError::Unsupported);
                }
                let data = code_data(code.as_ref())?;
                if attribute(tile, "code_arm_required").and_then(Value::as_bool) == Some(true)
                    && data.get("code").is_none()
                {
                    return Err(ActionError::InvalidValue);
                }
                Ok((
                    ActionKind::Alarm(mode.state().to_owned()),
                    ServiceCall::new("alarm_control_panel", mode.service(), data),
                ))
            }
            Self::AlarmDisarm { code } => {
                let data = code_data(code.as_ref())?;
                if attribute(tile, "code_format")
                    .and_then(Value::as_str)
                    .is_some_and(|format| !format.is_empty())
                    && data.get("code").is_none()
                {
                    return Err(ActionError::InvalidValue);
                }
                Ok((
                    ActionKind::Alarm("disarmed".to_owned()),
                    ServiceCall::new("alarm_control_panel", "alarm_disarm", data),
                ))
            }
            _ => Err(ActionError::Unsupported),
        }
    }
}

/// Thousandths of a unit, so numeric targets stay comparable and hashable.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn milli(value: f64) -> i64 {
    (value * 1000.0).round().clamp(-9.0e15, 9.0e15) as i64
}

#[cfg(test)]
#[allow(clippy::too_many_lines)] // Case tables.
mod tests {
    use super::{AlarmMode, DetailAction};
    use crate::{
        ActionError, ActionKind, EntityDomain, EntityTile, HaEntityState, LockTarget,
        PlaybackTarget,
    };
    use serde_json::json;

    fn tile(domain: EntityDomain, state: &str) -> EntityTile {
        let attributes = match domain {
            EntityDomain::Light => json!({
                "supported_color_modes": ["brightness"],
                "brightness": 255
            }),
            EntityDomain::Fan => json!({"percentage": 50}),
            EntityDomain::MediaPlayer => json!({
                "supported_features": 1 | 4 | 16 | 32 | 16_384,
                "volume_level": 0.5
            }),
            EntityDomain::Number | EntityDomain::InputNumber => {
                json!({"min": -100, "max": 100, "step": 0.001})
            }
            EntityDomain::Select | EntityDomain::InputSelect => {
                json!({"options": ["Eco", "x", "Away"]})
            }
            EntityDomain::AlarmControlPanel => json!({"supported_features": 0xffff}),
            _ => json!({}),
        };
        EntityTile {
            entity_id: "test.entity".to_owned(),
            name: "Test".to_owned(),
            icon: None,
            domain,
            state: Some(HaEntityState {
                entity_id: "test.entity".to_owned(),
                state: state.to_owned(),
                attributes: attributes
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
                last_updated: None,
            }),
            is_stale: false,
            command_enabled: true,
            action_feedback: None,
        }
    }

    fn plan(action: &DetailAction, domain: EntityDomain, state: &str) -> (ActionKind, String) {
        let (kind, call) = action.plan(&tile(domain, state)).expect("supported");
        (
            kind,
            format!("{}.{} {}", call.domain, call.service, call.data),
        )
    }

    #[test]
    fn alarm_modes_have_distinct_services_states_bits_and_keys() {
        for mode in AlarmMode::ALL {
            assert_eq!(AlarmMode::from_key(mode.key()), Some(mode));
            assert!(mode.service().ends_with(mode.key()));
            assert_eq!(mode.state(), mode.key().replace("arm_", "armed_"));
            assert_eq!(mode.feature_bit().count_ones(), 1);
        }
        let bits: u64 = AlarmMode::ALL.iter().map(|mode| mode.feature_bit()).sum();
        assert_eq!(bits, 1 | 2 | 4 | 16 | 32);
        assert_eq!(AlarmMode::from_key("disarm"), None);
    }

    #[test]
    fn ui_keys_become_actions_for_their_domain() {
        use EntityDomain as D;
        let cases = [
            (
                "level",
                "42.4",
                D::Light,
                Some(DetailAction::SetBrightness(42)),
            ),
            ("level", "150", D::Fan, Some(DetailAction::SetFanSpeed(100))),
            (
                "level",
                "-3",
                D::MediaPlayer,
                Some(DetailAction::SetVolume(0)),
            ),
            (
                "level",
                "2.5",
                D::InputNumber,
                Some(DetailAction::SetNumber(2.5)),
            ),
            ("level", "NaN", D::Number, None),
            ("level", "x", D::Light, None),
            ("level", "5", D::Lock, None),
            (
                "option",
                "Eco",
                D::Select,
                Some(DetailAction::SelectOption("Eco".into())),
            ),
            ("lock", "", D::Lock, Some(DetailAction::Lock)),
            ("unlock", "", D::Lock, Some(DetailAction::Unlock)),
            (
                "play_pause",
                "",
                D::MediaPlayer,
                Some(DetailAction::PlayPause),
            ),
            ("next", "", D::MediaPlayer, Some(DetailAction::NextTrack)),
            (
                "previous",
                "",
                D::MediaPlayer,
                Some(DetailAction::PreviousTrack),
            ),
            ("trigger", "", D::Automation, Some(DetailAction::Trigger)),
            (
                "disarm",
                "",
                D::AlarmControlPanel,
                Some(DetailAction::AlarmDisarm { code: None }),
            ),
            (
                "disarm",
                "1234",
                D::AlarmControlPanel,
                Some(DetailAction::AlarmDisarm {
                    code: Some("1234".into()),
                }),
            ),
            (
                "arm_night",
                "",
                D::AlarmControlPanel,
                Some(DetailAction::AlarmArm {
                    mode: AlarmMode::Night,
                    code: None,
                }),
            ),
            ("refresh_camera", "", D::Camera, None),
        ];
        for (key, argument, domain, expected) in cases {
            assert_eq!(
                DetailAction::from_ui(key, argument, domain),
                expected,
                "{key} {argument}"
            );
        }
    }

    fn service_cases() -> Vec<(
        DetailAction,
        EntityDomain,
        &'static str,
        ActionKind,
        &'static str,
    )> {
        use EntityDomain as D;
        vec![
            (
                DetailAction::SetBrightness(0),
                D::Light,
                "on",
                ActionKind::SetBrightness(1),
                r#"light.turn_on {"brightness_pct":1}"#,
            ),
            (
                DetailAction::SetFanSpeed(120),
                D::Fan,
                "on",
                ActionKind::SetFanSpeed(100),
                r#"fan.set_percentage {"percentage":100}"#,
            ),
            (
                DetailAction::SetVolume(30),
                D::MediaPlayer,
                "playing",
                ActionKind::SetVolume(30),
                r#"media_player.volume_set {"volume_level":0.3}"#,
            ),
            (
                DetailAction::SetNumber(21.5),
                D::InputNumber,
                "20",
                ActionKind::SetNumberExact("21.5".to_owned()),
                r#"input_number.set_value {"value":21.5}"#,
            ),
            (
                DetailAction::SelectOption("Eco".into()),
                D::Select,
                "Comfort",
                ActionKind::SelectOption("Eco".into()),
                r#"select.select_option {"option":"Eco"}"#,
            ),
            (
                DetailAction::Lock,
                D::Lock,
                "unlocked",
                ActionKind::Lock(LockTarget::Locked),
                "lock.lock {}",
            ),
            (
                DetailAction::Unlock,
                D::Lock,
                "locked",
                ActionKind::Lock(LockTarget::Unlocked),
                "lock.unlock {}",
            ),
            (
                DetailAction::PlayPause,
                D::MediaPlayer,
                "playing",
                ActionKind::Playback(PlaybackTarget::Paused),
                "media_player.media_play_pause {}",
            ),
            (
                DetailAction::PlayPause,
                D::MediaPlayer,
                "paused",
                ActionKind::Playback(PlaybackTarget::Playing),
                "media_player.media_play_pause {}",
            ),
            (
                DetailAction::NextTrack,
                D::MediaPlayer,
                "playing",
                ActionKind::RunScene,
                "media_player.media_next_track {}",
            ),
            (
                DetailAction::PreviousTrack,
                D::MediaPlayer,
                "playing",
                ActionKind::RunScene,
                "media_player.media_previous_track {}",
            ),
            (
                DetailAction::Trigger,
                D::Automation,
                "on",
                ActionKind::RunScene,
                r#"automation.trigger {"skip_condition":false}"#,
            ),
            (
                DetailAction::AlarmArm {
                    mode: AlarmMode::Away,
                    code: Some("12".into()),
                },
                D::AlarmControlPanel,
                "disarmed",
                ActionKind::Alarm("armed_away".into()),
                r#"alarm_control_panel.alarm_arm_away {"code":"12"}"#,
            ),
            (
                DetailAction::AlarmDisarm {
                    code: Some(String::new()),
                },
                D::AlarmControlPanel,
                "armed_home",
                ActionKind::Alarm("disarmed".into()),
                "alarm_control_panel.alarm_disarm {}",
            ),
        ]
    }

    #[test]
    fn each_action_builds_its_service_call() {
        for (action, domain, state, kind, call) in service_cases() {
            assert_eq!(
                plan(&action, domain, state),
                (kind, call.to_owned()),
                "{action:?}"
            );
        }
    }

    #[test]
    fn wrong_domains_and_bad_values_are_refused() {
        use EntityDomain as D;
        let refused = [
            (
                DetailAction::SetBrightness(50),
                D::Switch,
                ActionError::Unsupported,
            ),
            (
                DetailAction::SetFanSpeed(50),
                D::Light,
                ActionError::Unsupported,
            ),
            (
                DetailAction::SetVolume(50),
                D::Fan,
                ActionError::Unsupported,
            ),
            (
                DetailAction::SetNumber(1.0),
                D::Sensor,
                ActionError::Unsupported,
            ),
            (
                DetailAction::SetNumber(f64::INFINITY),
                D::Number,
                ActionError::InvalidValue,
            ),
            (
                DetailAction::SelectOption("x".into()),
                D::Number,
                ActionError::Unsupported,
            ),
            (
                DetailAction::SelectOption(String::new()),
                D::InputSelect,
                ActionError::InvalidValue,
            ),
            (
                DetailAction::SelectOption("x".repeat(256)),
                D::Select,
                ActionError::InvalidValue,
            ),
            (DetailAction::Lock, D::Switch, ActionError::Unsupported),
            (DetailAction::PlayPause, D::Light, ActionError::Unsupported),
            (DetailAction::Trigger, D::Script, ActionError::Unsupported),
            (
                DetailAction::AlarmDisarm { code: None },
                D::Lock,
                ActionError::Unsupported,
            ),
            (
                DetailAction::AlarmArm {
                    mode: AlarmMode::Home,
                    code: Some("9".repeat(33)),
                },
                D::AlarmControlPanel,
                ActionError::InvalidValue,
            ),
        ];
        for (action, domain, error) in refused {
            assert_eq!(
                action.plan(&tile(domain, "on")).err(),
                Some(error),
                "{action:?}"
            );
        }
    }
}
