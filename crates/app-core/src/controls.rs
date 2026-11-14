//! Capability-aware controls shared by native frontends and action validation.

use crate::{ActionError, ActionKind, EntityTile, ServiceCall};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Button,
    Slider,
    Select,
    Toggle,
    Form,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Text,
    Password,
    Number,
    Select,
    Toggle,
    Entity,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InputSpec {
    pub key: &'static str,
    pub label: String,
    pub kind: InputKind,
    pub value: Value,
    pub options: Vec<String>,
    pub minimum: f64,
    pub maximum: f64,
    pub step: f64,
    pub required: bool,
}

impl InputSpec {
    #[must_use]
    pub fn text(key: &'static str, label: &str, required: bool) -> Self {
        Self {
            key,
            label: label.into(),
            kind: InputKind::Text,
            value: Value::String(String::new()),
            options: Vec::new(),
            minimum: 0.0,
            maximum: 100.0,
            step: 1.0,
            required,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ControlSpec {
    pub key: &'static str,
    pub label: String,
    pub kind: ControlKind,
    pub value: Value,
    pub options: Vec<String>,
    pub minimum: f64,
    pub maximum: f64,
    pub step: f64,
    pub unit: String,
    pub inputs: Vec<InputSpec>,
    pub enabled: bool,
    pub dangerous: bool,
    pub icon: &'static str,
    pub group: &'static str,
}

impl ControlSpec {
    #[must_use]
    pub fn button(key: &'static str, label: &str) -> Self {
        Self {
            key,
            label: label.into(),
            kind: ControlKind::Button,
            value: Value::Null,
            options: Vec::new(),
            minimum: 0.0,
            maximum: 100.0,
            step: 1.0,
            unit: String::new(),
            inputs: Vec::new(),
            enabled: true,
            dangerous: false,
            icon: "",
            group: "",
        }
    }
}

#[must_use]
pub fn text(german: bool, english: &str, german_text: &str) -> String {
    if german { german_text } else { english }.to_owned()
}

#[must_use]
pub fn attribute<'a>(tile: &'a EntityTile, key: &str) -> Option<&'a Value> {
    tile.state.as_ref()?.attributes.get(key)
}

#[must_use]
pub fn feature(tile: &EntityTile, bit: u64) -> bool {
    attribute(tile, "supported_features")
        .and_then(Value::as_u64)
        .is_some_and(|bits| bits & bit != 0)
}

#[must_use]
pub fn options(tile: &EntityTile, key: &str) -> Vec<String> {
    attribute(tile, key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[must_use]
pub fn for_entity(tile: &EntityTile, german: bool) -> Vec<ControlSpec> {
    if crate::entity_extensions::handles(tile.domain) {
        crate::entity_extensions::controls(tile, german)
    } else if matches!(
        tile.domain,
        crate::EntityDomain::Sensor | crate::EntityDomain::BinarySensor
    ) {
        vec![ControlSpec::button(
            "history",
            &text(
                german,
                "History · Last 24 hours",
                "Verlauf · Letzte 24 Stunden",
            ),
        )]
    } else if tile.domain == crate::EntityDomain::MediaPlayer {
        crate::media::controls(tile, german)
    } else {
        crate::entity_catalog::controls(tile, german)
    }
}

/// Build a capability-checked entity command.
///
/// # Errors
/// Rejects unsupported controls, invalid inputs and an unusable system clock.
pub fn plan(
    tile: &EntityTile,
    key: &str,
    argument: &Value,
) -> Result<(ActionKind, ServiceCall), ActionError> {
    if crate::entity_extensions::handles(tile.domain) {
        crate::entity_extensions::plan(tile, key, argument)
    } else if key == "history"
        && matches!(
            tile.domain,
            crate::EntityDomain::Sensor | crate::EntityDomain::BinarySensor
        )
    {
        if !argument.is_null() {
            return Err(ActionError::InvalidValue);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| ActionError::InvalidValue)?
            .as_secs();
        let now = i64::try_from(now).map_err(|_| ActionError::InvalidValue)?;
        Ok((
            ActionKind::RunScene,
            ServiceCall::new(
                "history",
                "history_during_period",
                serde_json::json!({
                    "start_time": crate::presentation::unix_timestamp(now - 86400),
                    "end_time": crate::presentation::unix_timestamp(now),
                    "minimal_response": true, "no_attributes": true,
                }),
            ),
        ))
    } else if tile.domain == crate::EntityDomain::MediaPlayer {
        crate::media::plan(tile, key, argument)
    } else {
        crate::entity_catalog::plan(tile, key, argument)
    }
}
