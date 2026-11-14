//! Observable targets for generic entity-catalog service calls.
//!
//! The service acknowledgement only means Home Assistant accepted the request. These targets
//! keep the entity action pending until a state update shows the requested value.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::{ActionKind, EntityDomain, EntityTile};

/// Return the state target that can safely confirm a catalog service call.
///
/// Services with transient or provider-specific results use `RunScene`, which reports receipt
/// without claiming that the requested state was observed.
#[must_use]
#[allow(clippy::too_many_lines)] // Keep the safe service-to-target table adjacent and auditable.
#[allow(clippy::match_same_arms)] // Keep each domain’s observable contract readable beside its service name.
pub fn action_kind(tile: &EntityTile, service: &str, data: &Value) -> ActionKind {
    let domain = tile.domain;
    let data = data.as_object();
    let empty = data.is_some_and(serde_json::Map::is_empty);

    match (domain, service) {
        (
            EntityDomain::Light
            | EntityDomain::Switch
            | EntityDomain::Fan
            | EntityDomain::InputBoolean
            | EntityDomain::Automation
            | EntityDomain::Group
            | EntityDomain::Humidifier
            | EntityDomain::Siren,
            "turn_on",
        ) if empty => observe_state("on"),
        (
            EntityDomain::Light
            | EntityDomain::Switch
            | EntityDomain::Fan
            | EntityDomain::InputBoolean
            | EntityDomain::Automation
            | EntityDomain::Group
            | EntityDomain::Humidifier
            | EntityDomain::Siren,
            "turn_off",
        ) if empty => observe_state("off"),
        (EntityDomain::WaterHeater | EntityDomain::Climate, "turn_off") if empty => {
            observe_state("off")
        }
        (EntityDomain::Climate, "set_hvac_mode") => data
            .and_then(|data| data.get("hvac_mode"))
            .and_then(Value::as_str)
            .map_or(ActionKind::RunScene, observe_state),
        (EntityDomain::Select | EntityDomain::InputSelect, "select_option") => data
            .and_then(|data| data.get("option"))
            .and_then(Value::as_str)
            .map_or(ActionKind::RunScene, observe_state),
        (EntityDomain::Text | EntityDomain::InputText, "set_value") => data
            .and_then(|data| data.get("value"))
            .and_then(Value::as_str)
            .map_or(ActionKind::RunScene, observe_state),
        (EntityDomain::Date, "set_value") => data
            .and_then(|data| data.get("date"))
            .and_then(Value::as_str)
            .map_or(ActionKind::RunScene, observe_state),
        (EntityDomain::Number | EntityDomain::InputNumber, "set_value") => data
            .and_then(|data| data.get("value"))
            .and_then(Value::as_number)
            .map_or(ActionKind::RunScene, |value| {
                ActionKind::SetNumberExact(value.to_string())
            }),
        (EntityDomain::Climate, "set_temperature") => match data {
            Some(data) if data.contains_key("temperature") => {
                observe_attribute_in_data(data, "temperature", "temperature")
            }
            Some(data)
                if data.contains_key("target_temp_low")
                    && data.contains_key("target_temp_high") =>
            {
                observe_attributes(
                    data,
                    &[
                        ("target_temp_low", "target_temp_low"),
                        ("target_temp_high", "target_temp_high"),
                    ],
                )
            }
            _ => ActionKind::RunScene,
        },
        (EntityDomain::Climate | EntityDomain::Humidifier, "set_humidity") => {
            let attribute = humidity_attribute(tile);
            observe_attribute_from(data, "humidity", attribute)
        }
        (EntityDomain::Climate, "set_preset_mode") => {
            observe_attribute_from(data, "preset_mode", "preset_mode")
        }
        (EntityDomain::Climate, "set_fan_mode") => {
            observe_attribute_from(data, "fan_mode", "fan_mode")
        }
        (EntityDomain::Climate, "set_swing_mode") => {
            observe_attribute_from(data, "swing_mode", "swing_mode")
        }
        (EntityDomain::Climate, "set_swing_horizontal_mode") => {
            observe_attribute_from(data, "swing_horizontal_mode", "swing_horizontal_mode")
        }
        (EntityDomain::Fan, "set_percentage") => {
            observe_attribute_from(data, "percentage", "percentage")
        }
        (EntityDomain::Fan, "set_preset_mode") => {
            observe_attribute_from(data, "preset_mode", "preset_mode")
        }
        (EntityDomain::Fan, "set_direction") => {
            observe_attribute_from(data, "direction", "direction")
        }
        (EntityDomain::Fan, "oscillate") => {
            observe_attribute_from(data, "oscillating", "oscillating")
        }
        (EntityDomain::Cover, "set_cover_position") => {
            observe_attribute_from(data, "position", "current_position")
        }
        (EntityDomain::Cover, "set_cover_tilt_position") => {
            observe_attribute_from(data, "tilt_position", "current_tilt_position")
        }
        (EntityDomain::Humidifier, "set_mode") => observe_attribute_from(data, "mode", "mode"),
        (EntityDomain::WaterHeater, "set_temperature") => {
            observe_attribute_from(data, "temperature", "temperature")
        }
        (EntityDomain::WaterHeater, "set_operation_mode") => {
            observe_attribute_from(data, "operation_mode", "operation_mode")
        }
        (EntityDomain::WaterHeater, "set_away_mode") => observe_away_mode(tile, data),
        (EntityDomain::Valve, "set_valve_position") => {
            observe_attribute_from(data, "position", "current_position")
        }
        (EntityDomain::Valve, "open_valve") => observe_state("open"),
        (EntityDomain::Valve, "close_valve") => observe_state("closed"),
        _ => ActionKind::RunScene,
    }
}

fn observe_state(state: &str) -> ActionKind {
    ActionKind::Observe {
        state: Some(state.to_owned()),
        attributes: BTreeMap::new(),
    }
}

fn observe_attribute_from(
    data: Option<&serde_json::Map<String, Value>>,
    data_key: &str,
    attribute_key: &str,
) -> ActionKind {
    data.and_then(|data| data.get(data_key))
        .map_or(ActionKind::RunScene, |value| {
            observe_value(attribute_key, value.clone())
        })
}

fn observe_attribute_in_data(
    data: &serde_json::Map<String, Value>,
    data_key: &str,
    attribute_key: &str,
) -> ActionKind {
    data.get(data_key).map_or(ActionKind::RunScene, |value| {
        observe_value(attribute_key, value.clone())
    })
}

fn observe_attributes(data: &serde_json::Map<String, Value>, pairs: &[(&str, &str)]) -> ActionKind {
    let mut attributes = BTreeMap::new();
    for (data_key, attribute_key) in pairs {
        let Some(value) = data.get(*data_key) else {
            return ActionKind::RunScene;
        };
        if !(value.is_string() || value.is_boolean() || value.is_number()) {
            return ActionKind::RunScene;
        }
        attributes.insert((*attribute_key).to_owned(), value.clone());
    }
    ActionKind::Observe {
        state: None,
        attributes,
    }
}

fn observe_value(attribute_key: &str, value: Value) -> ActionKind {
    if !(value.is_string() || value.is_boolean() || value.is_number()) {
        return ActionKind::RunScene;
    }
    ActionKind::Observe {
        state: None,
        attributes: BTreeMap::from([(attribute_key.to_owned(), value)]),
    }
}

fn humidity_attribute(tile: &EntityTile) -> &'static str {
    let Some(attributes) = tile.state.as_ref().map(|state| &state.attributes) else {
        return "humidity";
    };
    if attributes.contains_key("humidity") || !attributes.contains_key("target_humidity") {
        "humidity"
    } else {
        // Some older integrations exposed the requested target under this legacy name.
        "target_humidity"
    }
}

fn observe_away_mode(
    tile: &EntityTile,
    data: Option<&serde_json::Map<String, Value>>,
) -> ActionKind {
    let Some(target) = data
        .and_then(|data| data.get("away_mode"))
        .and_then(Value::as_bool)
    else {
        return ActionKind::RunScene;
    };
    let current = tile
        .state
        .as_ref()
        .and_then(|state| state.attributes.get("away_mode"));
    match current {
        Some(Value::Bool(_)) => observe_value("away_mode", Value::Bool(target)),
        Some(Value::String(value)) if value == "on" || value == "off" => observe_value(
            "away_mode",
            Value::String(if target { "on" } else { "off" }.into()),
        ),
        _ => ActionKind::RunScene,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::action_kind;
    use crate::{
        ActionAcknowledgement, ActionFeedback, ActionKind, AppCore, ConnectionStatus,
        DashboardOrder, DetailAction, EntityDomain, EntityRegistryEntry, EntityTile, HaEntityState,
        PrimaryTap,
    };
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    fn tile(domain: EntityDomain, state: &str) -> EntityTile {
        EntityTile {
            entity_id: format!("{}.sample", domain.service_domain()),
            name: "Sample".into(),
            icon: None,
            domain,
            state: Some(HaEntityState {
                entity_id: format!("{}.sample", domain.service_domain()),
                state: state.into(),
                attributes: BTreeMap::new(),
                last_updated: None,
            }),
            is_stale: false,
            command_enabled: true,
            action_feedback: None,
        }
    }

    #[test]
    fn observable_operations_wait_for_exact_supported_targets() {
        assert_eq!(
            action_kind(&tile(EntityDomain::Switch, "off"), "turn_on", &json!({})),
            ActionKind::Observe {
                state: Some("on".into()),
                attributes: BTreeMap::new(),
            }
        );
        assert_eq!(
            action_kind(
                &tile(EntityDomain::Climate, "heat"),
                "set_temperature",
                &json!({"temperature":21.5}),
            ),
            ActionKind::Observe {
                state: None,
                attributes: BTreeMap::from([("temperature".into(), json!(21.5))]),
            }
        );
        assert_eq!(
            action_kind(
                &tile(EntityDomain::Light, "off"),
                "turn_on",
                &json!({"rgb_color":[255,0,0]}),
            ),
            ActionKind::RunScene,
            "light providers may quantize or transform color values",
        );
        assert_eq!(
            action_kind(&tile(EntityDomain::Script, "off"), "turn_on", &Value::Null),
            ActionKind::RunScene,
        );

        let mut climate = tile(EntityDomain::Climate, "heat");
        assert_eq!(
            action_kind(&climate, "set_humidity", &json!({"humidity": 48})),
            ActionKind::Observe {
                state: None,
                attributes: BTreeMap::from([("humidity".into(), json!(48))]),
            },
        );
        climate
            .state
            .as_mut()
            .expect("state")
            .attributes
            .insert("target_humidity".into(), json!(45));
        assert_eq!(
            action_kind(&climate, "set_humidity", &json!({"humidity": 48})),
            ActionKind::Observe {
                state: None,
                attributes: BTreeMap::from([("target_humidity".into(), json!(48))]),
            },
            "retain matching behavior for older integrations that expose target_humidity",
        );

        let mut water_heater = tile(EntityDomain::WaterHeater, "electric");
        water_heater
            .state
            .as_mut()
            .expect("state")
            .attributes
            .insert("away_mode".into(), json!("off"));
        assert_eq!(
            action_kind(&water_heater, "set_away_mode", &json!({"away_mode": true}),),
            ActionKind::Observe {
                state: None,
                attributes: BTreeMap::from([("away_mode".into(), json!("on"))]),
            },
            "preserve the integration's observed away_mode representation",
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Keep the complete confirmation table auditable in one test.
    fn every_observable_catalog_mapping_preserves_its_exact_target_shape() {
        let observed_state = |state: &str| ActionKind::Observe {
            state: Some(state.into()),
            attributes: BTreeMap::new(),
        };
        let observed_attribute = |key: &str, value: Value| ActionKind::Observe {
            state: None,
            attributes: BTreeMap::from([(key.into(), value)]),
        };

        assert_eq!(
            action_kind(&tile(EntityDomain::Switch, "on"), "turn_off", &json!({}),),
            observed_state("off")
        );
        assert_eq!(
            action_kind(&tile(EntityDomain::Climate, "heat"), "turn_off", &json!({}),),
            observed_state("off")
        );
        assert_eq!(
            action_kind(
                &tile(EntityDomain::Climate, "heat"),
                "set_hvac_mode",
                &json!({"hvac_mode":"cool"}),
            ),
            observed_state("cool")
        );
        assert_eq!(
            action_kind(
                &tile(EntityDomain::Select, "eco"),
                "select_option",
                &json!({"option":"boost"}),
            ),
            observed_state("boost")
        );
        assert_eq!(
            action_kind(
                &tile(EntityDomain::Text, "old"),
                "set_value",
                &json!({"value":"new"}),
            ),
            observed_state("new")
        );
        assert_eq!(
            action_kind(
                &tile(EntityDomain::Date, "2026-11-14"),
                "set_value",
                &json!({"date":"2026-11-15"}),
            ),
            observed_state("2026-11-15")
        );
        assert_eq!(
            action_kind(
                &tile(EntityDomain::Number, "1"),
                "set_value",
                &json!({"value":1.25}),
            ),
            ActionKind::SetNumberExact("1.25".into())
        );
        assert_eq!(
            action_kind(
                &tile(EntityDomain::Climate, "heat_cool"),
                "set_temperature",
                &json!({"target_temp_low":18,"target_temp_high":22}),
            ),
            ActionKind::Observe {
                state: None,
                attributes: BTreeMap::from([
                    ("target_temp_low".into(), json!(18)),
                    ("target_temp_high".into(), json!(22)),
                ]),
            }
        );

        for (domain, service, data_key, attribute, value) in [
            (
                EntityDomain::Climate,
                "set_preset_mode",
                "preset_mode",
                "preset_mode",
                json!("eco"),
            ),
            (
                EntityDomain::Climate,
                "set_fan_mode",
                "fan_mode",
                "fan_mode",
                json!("low"),
            ),
            (
                EntityDomain::Climate,
                "set_swing_mode",
                "swing_mode",
                "swing_mode",
                json!("vertical"),
            ),
            (
                EntityDomain::Climate,
                "set_swing_horizontal_mode",
                "swing_horizontal_mode",
                "swing_horizontal_mode",
                json!("left"),
            ),
            (
                EntityDomain::Fan,
                "set_percentage",
                "percentage",
                "percentage",
                json!(55),
            ),
            (
                EntityDomain::Fan,
                "set_preset_mode",
                "preset_mode",
                "preset_mode",
                json!("sleep"),
            ),
            (
                EntityDomain::Fan,
                "set_direction",
                "direction",
                "direction",
                json!("reverse"),
            ),
            (
                EntityDomain::Fan,
                "oscillate",
                "oscillating",
                "oscillating",
                json!(true),
            ),
            (
                EntityDomain::Cover,
                "set_cover_position",
                "position",
                "current_position",
                json!(70),
            ),
            (
                EntityDomain::Cover,
                "set_cover_tilt_position",
                "tilt_position",
                "current_tilt_position",
                json!(30),
            ),
            (
                EntityDomain::Humidifier,
                "set_mode",
                "mode",
                "mode",
                json!("sleep"),
            ),
            (
                EntityDomain::WaterHeater,
                "set_temperature",
                "temperature",
                "temperature",
                json!(55),
            ),
            (
                EntityDomain::WaterHeater,
                "set_operation_mode",
                "operation_mode",
                "operation_mode",
                json!("eco"),
            ),
            (
                EntityDomain::Valve,
                "set_valve_position",
                "position",
                "current_position",
                json!(40),
            ),
        ] {
            let data = Value::Object(serde_json::Map::from_iter([(
                data_key.to_owned(),
                value.clone(),
            )]));
            assert_eq!(
                action_kind(&tile(domain, "on"), service, &data),
                observed_attribute(attribute, value),
            );
        }

        assert_eq!(
            action_kind(
                &tile(EntityDomain::Valve, "closed"),
                "open_valve",
                &json!({}),
            ),
            observed_state("open")
        );
        assert_eq!(
            action_kind(
                &tile(EntityDomain::Valve, "open"),
                "close_valve",
                &json!({}),
            ),
            observed_state("closed")
        );

        let mut water_heater = tile(EntityDomain::WaterHeater, "electric");
        water_heater
            .state
            .as_mut()
            .expect("state")
            .attributes
            .insert("away_mode".into(), json!(false));
        assert_eq!(
            action_kind(&water_heater, "set_away_mode", &json!({"away_mode":true}),),
            observed_attribute("away_mode", json!(true))
        );

        assert_eq!(
            action_kind(
                &tile(EntityDomain::Climate, "heat"),
                "set_temperature",
                &json!({"target_temp_low":[],"target_temp_high":22}),
            ),
            ActionKind::RunScene
        );
        assert_eq!(
            action_kind(&tile(EntityDomain::Fan, "on"), "set_percentage", &json!({}),),
            ActionKind::RunScene
        );
    }

    #[test]
    fn observed_catalog_state_before_ack_stays_pending_until_the_service_result() {
        let entity = EntityRegistryEntry {
            entity_id: "climate.room".into(),
            area_id: None,
            device_id: None,
            disabled_by: None,
            hidden_by: None,
            entity_category: None,
            name: None,
            icon: None,
        };
        let state = |temperature: Option<f64>| HaEntityState {
            entity_id: "climate.room".into(),
            state: "heat".into(),
            attributes: BTreeMap::from([
                ("supported_features".into(), json!(1)),
                ("min_temp".into(), json!(7)),
                ("max_temp".into(), json!(35)),
                ("target_temp_step".into(), json!(0.5)),
            ])
            .into_iter()
            .chain(temperature.map(|value| ("temperature".into(), json!(value))))
            .collect(),
            last_updated: None,
        };
        let mut core = AppCore::new();
        core.set_connection(ConnectionStatus::Ready);
        core.refresh_from_ha(
            &[],
            &[],
            &[entity],
            &[state(None)],
            &DashboardOrder::default(),
        )
        .expect("climate snapshot");
        assert!(matches!(
            core.request_action(
                "climate.room",
                &DetailAction::Entity {
                    key: "set_temperature".into(),
                    argument: json!({"temperature":21.5}),
                },
            ),
            Ok(PrimaryTap::Call { .. })
        ));

        core.apply_state(state(Some(21.5)))
            .expect("matching authoritative state");
        assert_eq!(
            core.dashboard()
                .entity("climate.room")
                .unwrap()
                .action_feedback,
            Some(ActionFeedback::AwaitingAcknowledgement {
                action: ActionKind::Observe {
                    state: None,
                    attributes: BTreeMap::from([("temperature".into(), json!(21.5))]),
                },
            })
        );

        core.acknowledge("climate.room", ActionAcknowledgement::Accepted)
            .expect("accepted service result still has its request");
        assert_eq!(
            core.dashboard()
                .entity("climate.room")
                .unwrap()
                .action_feedback,
            None,
            "the previously observed target confirms after the ACK",
        );
    }
}
