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
    const fn new(domain: &'static str, service: &'static str, data: Value) -> Self {
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
        let mut message = json!({
            "type": "call_service",
            "domain": self.domain,
            "service": self.service,
            "target": {"entity_id": entity_id},
        });
        if let Some(data) = self.data.as_object().filter(|data| !data.is_empty()) {
            message["service_data"] = Value::Object(data.clone());
        }
        message
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
}

impl DetailAction {
    /// Builds the action a front end reported as `key` with `argument`, or `None` when the
    /// key is not an action for this domain (for example `refresh_camera`).
    #[must_use]
    pub fn from_ui(key: &str, argument: &str, domain: EntityDomain) -> Option<Self> {
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
            | Self::SelectOption(_) => self.plan_value(domain),
            Self::Lock | Self::Unlock => self.plan_lock(domain),
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
            Self::AlarmArm { .. } | Self::AlarmDisarm { .. } => self.plan_alarm(domain),
        }
    }

    fn plan_value(&self, domain: EntityDomain) -> Result<(ActionKind, ServiceCall), ActionError> {
        match self {
            Self::SetBrightness(percent) if domain == EntityDomain::Light => {
                let percent = (*percent).clamp(1, 100);
                Ok((
                    ActionKind::SetBrightness(percent),
                    ServiceCall::new("light", "turn_on", json!({"brightness_pct": percent})),
                ))
            }
            Self::SetFanSpeed(percent) if domain == EntityDomain::Fan => {
                let percent = (*percent).min(100);
                Ok((
                    ActionKind::SetFanSpeed(percent),
                    ServiceCall::new("fan", "set_percentage", json!({"percentage": percent})),
                ))
            }
            Self::SetVolume(percent) if domain == EntityDomain::MediaPlayer => {
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
                Ok((
                    ActionKind::SetNumber(milli(*value)),
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

    fn plan_lock(&self, domain: EntityDomain) -> Result<(ActionKind, ServiceCall), ActionError> {
        if domain != EntityDomain::Lock {
            return Err(ActionError::Unsupported);
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
            Self::NextTrack => (ActionKind::RunScene, "media_next_track"),
            Self::PreviousTrack => (ActionKind::RunScene, "media_previous_track"),
            _ => {
                let target = if tile.state_value() == Some("playing") {
                    PlaybackTarget::Paused
                } else {
                    PlaybackTarget::Playing
                };
                (ActionKind::Playback(target), "media_play_pause")
            }
        };
        Ok((kind, ServiceCall::new("media_player", service, json!({}))))
    }

    fn plan_alarm(&self, domain: EntityDomain) -> Result<(ActionKind, ServiceCall), ActionError> {
        if domain != EntityDomain::AlarmControlPanel {
            return Err(ActionError::Unsupported);
        }
        match self {
            Self::AlarmArm { mode, code } => Ok((
                ActionKind::Alarm(mode.state().to_owned()),
                ServiceCall::new(
                    "alarm_control_panel",
                    mode.service(),
                    code_data(code.as_ref())?,
                ),
            )),
            Self::AlarmDisarm { code } => Ok((
                ActionKind::Alarm("disarmed".to_owned()),
                ServiceCall::new(
                    "alarm_control_panel",
                    "alarm_disarm",
                    code_data(code.as_ref())?,
                ),
            )),
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

    fn tile(domain: EntityDomain, state: &str) -> EntityTile {
        EntityTile {
            entity_id: "test.entity".to_owned(),
            name: "Test".to_owned(),
            icon: None,
            domain,
            state: Some(HaEntityState {
                entity_id: "test.entity".to_owned(),
                state: state.to_owned(),
                attributes: std::collections::BTreeMap::new(),
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
                ActionKind::SetNumber(21_500),
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
