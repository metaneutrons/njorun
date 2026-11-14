use std::collections::BTreeMap;

use njorun_app_core::controls::{self, ControlKind};
use njorun_app_core::{ActionError, ActionKind, EntityDomain as D, EntityTile, HaEntityState};
use serde_json::{Value, json};

// Each fixture owns its JSON literal, which keeps each case self-contained.
#[allow(clippy::needless_pass_by_value)]
fn tile(domain: D, state: &str, attributes: Value) -> EntityTile {
    let attributes = attributes
        .as_object()
        .expect("object attrs")
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    EntityTile {
        entity_id: format!("{}.sample", domain.service_domain()),
        name: "Sample".into(),
        icon: None,
        domain,
        state: Some(HaEntityState {
            entity_id: format!("{}.sample", domain.service_domain()),
            state: state.into(),
            attributes,
            last_updated: None,
        }),
        is_stale: false,
        command_enabled: true,
        action_feedback: None,
    }
}

// Own the small JSON argument in test call sites to keep fixtures concise.
#[allow(clippy::needless_pass_by_value)]
fn plan(
    domain_tile: &EntityTile,
    key: &str,
    argument: Value,
) -> (ActionKind, String, String, Value) {
    let (kind, call) = controls::plan(domain_tile, key, &argument).expect("valid operation");
    (kind, call.domain.into(), call.service.into(), call.data)
}

#[test]
fn light_catalog_uses_supported_modes_features_and_exact_service_fields() {
    let light = tile(
        D::Light,
        "on",
        json!({
            "brightness": 127,
            "supported_color_modes": ["color_temp", "rgb", "hs"],
            "min_color_temp_kelvin": 2700,
            "max_color_temp_kelvin": 6500,
            "effect_list": ["pulse", "rainbow"],
            "supported_features": 4 | 8 | 32,
        }),
    );
    let controls = controls::for_entity(&light, false);
    let keys = controls
        .iter()
        .map(|control| control.key)
        .collect::<Vec<_>>();
    for key in [
        "brightness",
        "color_temp",
        "rgb_color",
        "hs_color",
        "effect",
        "transition",
        "flash",
    ] {
        assert!(keys.contains(&key), "missing {key}");
    }
    assert_eq!(
        plan(&light, "color_temp", json!(3100)).3,
        json!({"color_temp_kelvin":3100})
    );
    assert_eq!(
        plan(&light, "rgb_color", json!({"red":10,"green":20,"blue":30})).3,
        json!({"rgb_color":[10,20,30]})
    );
    assert_eq!(
        controls::plan(&light, "color_temp", &json!(9999)),
        Err(ActionError::InvalidValue)
    );
    assert_eq!(
        controls::plan(
            &light,
            "rgb_color",
            &json!({"red":1,"green":2,"blue":3,"arbitrary":"x"})
        ),
        Err(ActionError::InvalidValue)
    );

    let simple = tile(
        D::Light,
        "on",
        json!({"supported_color_modes":["onoff"], "supported_features":0}),
    );
    let keys = controls::for_entity(&simple, false)
        .iter()
        .map(|control| control.key)
        .collect::<Vec<_>>();
    assert!(!keys.contains(&"brightness"));
    assert!(!keys.contains(&"effect"));
}

#[test]
fn advanced_light_color_modes_render_and_map_to_exact_ha_payloads() {
    let light = tile(
        D::Light,
        "on",
        json!({
            "supported_color_modes": ["color_temp", "rgb", "hs", "xy", "rgbw", "rgbww", "white"],
            "min_color_temp_kelvin": 2000,
            "max_color_temp_kelvin": 6500,
            "effect_list": ["pulse", "rainbow"],
            "effect": "pulse",
            "supported_features": 4 | 8 | 32,
        }),
    );
    let keys = controls::for_entity(&light, true)
        .iter()
        .map(|control| control.key)
        .collect::<Vec<_>>();
    for key in [
        "xy_color",
        "rgbw_color",
        "rgbww_color",
        "white",
        "effect",
        "transition",
        "flash",
    ] {
        assert!(keys.contains(&key), "missing {key}");
    }

    assert_eq!(
        plan(&light, "hs_color", json!({"hue":180,"saturation":75})).3,
        json!({"hs_color":[180,75]})
    );
    assert_eq!(
        plan(&light, "xy_color", json!({"x":0.25,"y":0.75})).3,
        json!({"xy_color":[0.25,0.75]})
    );
    assert_eq!(
        plan(
            &light,
            "rgbw_color",
            json!({"red":10,"green":20,"blue":30,"white":40}),
        )
        .3,
        json!({"rgbw_color":[10,20,30,40]})
    );
    assert_eq!(
        plan(
            &light,
            "rgbww_color",
            json!({"red":10,"green":20,"blue":30,"cold_white":40,"warm_white":50}),
        )
        .3,
        json!({"rgbww_color":[10,20,30,40,50]})
    );
    assert_eq!(plan(&light, "white", json!(128)).3, json!({"white":128}));
    assert_eq!(
        plan(&light, "effect", json!("rainbow")).3,
        json!({"effect":"rainbow"})
    );
    assert_eq!(
        plan(&light, "transition", json!(12)).3,
        json!({"transition":12})
    );
    assert_eq!(
        plan(&light, "flash", json!("short")).3,
        json!({"flash":"short"})
    );
}

#[test]
#[allow(clippy::too_many_lines)] // Capability cases stay together so feature masks remain comparable.
fn fan_and_lock_controls_are_capability_and_state_gated() {
    let fan = tile(
        D::Fan,
        "on",
        json!({
            "supported_features": 1 | 2 | 4 | 8,
            "percentage": 40,
            "preset_modes": ["auto", "sleep"],
            "preset_mode": "auto",
            "oscillating": false,
            "direction": "forward",
        }),
    );
    assert_eq!(
        plan(&fan, "percentage", json!(55)).3,
        json!({"percentage":55})
    );
    assert_eq!(
        plan(&fan, "preset_mode", json!("sleep")).3,
        json!({"preset_mode":"sleep"})
    );
    assert_eq!(
        plan(&fan, "direction", json!("reverse")).3,
        json!({"direction":"reverse"})
    );
    assert_eq!(
        plan(&fan, "oscillate", json!(true)).3,
        json!({"oscillating":true})
    );
    assert_eq!(
        controls::plan(&fan, "direction", &json!("up")),
        Err(ActionError::InvalidValue)
    );
    let fan_controls = controls::for_entity(&fan, false);
    assert!(!fan_controls.iter().any(|control| control.key == "turn_on"));
    assert!(!fan_controls.iter().any(|control| control.key == "turn_off"));
    let fan_can_turn_off = tile(D::Fan, "on", json!({"supported_features":FAN_TURN_OFF}));
    let controls = controls::for_entity(&fan_can_turn_off, false);
    assert!(!controls.iter().any(|control| control.key == "turn_on"));
    assert!(controls.iter().any(|control| control.key == "turn_off"));
    let fan_can_turn_on = tile(D::Fan, "off", json!({"supported_features":FAN_TURN_ON}));
    let controls = controls::for_entity(&fan_can_turn_on, false);
    assert!(controls.iter().any(|control| control.key == "turn_on"));
    assert!(!controls.iter().any(|control| control.key == "turn_off"));

    let unsupported_fan = tile(
        D::Fan,
        "on",
        json!({"supported_features":0, "percentage":20}),
    );
    assert!(
        !controls::for_entity(&unsupported_fan, false)
            .iter()
            .any(|control| control.key == "direction")
    );
    assert!(
        !controls::for_entity(&unsupported_fan, false)
            .iter()
            .any(|control| control.key == "percentage")
    );
    assert!(
        !controls::for_entity(&unsupported_fan, false)
            .iter()
            .any(|control| control.key == "preset_mode")
    );
    assert_eq!(
        controls::plan(&unsupported_fan, "direction", &json!("reverse")),
        Err(ActionError::Unsupported)
    );
    assert_eq!(
        controls::plan(&unsupported_fan, "turn_on", &Value::Null),
        Err(ActionError::Unsupported)
    );
    assert_eq!(
        controls::plan(&unsupported_fan, "turn_off", &Value::Null),
        Err(ActionError::Unsupported)
    );

    let lock = tile(
        D::Lock,
        "locked",
        json!({"code_format":"number","supported_features":1}),
    );
    let action = controls::for_entity(&lock, false)
        .into_iter()
        .find(|item| item.key == "unlock")
        .unwrap();
    assert_eq!(action.kind, ControlKind::Form);
    assert_eq!(
        action.inputs[0].kind,
        njorun_app_core::controls::InputKind::Password
    );
    assert_eq!(plan(&lock, "unlock", json!({"code":"1234"})).2, "unlock");
    let open = controls::for_entity(&lock, false)
        .into_iter()
        .find(|item| item.key == "open")
        .unwrap();
    assert_eq!(open.kind, ControlKind::Form);
    assert_eq!(
        plan(&lock, "open", json!({"code":"0012"})).3,
        json!({"code":"0012"})
    );
    assert_eq!(
        controls::plan(&lock, "unlock", &json!({"code":"1234","other":true})),
        Err(ActionError::InvalidValue)
    );
    assert_eq!(
        controls::plan(&lock, "unlock", &json!({"code":"12x4"})),
        Err(ActionError::InvalidValue)
    );

    let moving = tile(D::Lock, "unlocking", json!({}));
    assert!(
        !controls::for_entity(&moving, false)
            .iter()
            .find(|item| item.key == "lock")
            .unwrap()
            .enabled
    );
    assert_eq!(
        controls::plan(&moving, "lock", &Value::Null),
        Err(ActionError::Unsupported)
    );
}

#[test]
fn alarm_modes_require_capability_and_code_contract() {
    let alarm = tile(
        D::AlarmControlPanel,
        "disarmed",
        json!({
            "supported_features": 1 | 4,
            "code_format":"number",
            "code_arm_required":true,
        }),
    );
    let keys = controls::for_entity(&alarm, true)
        .iter()
        .map(|control| control.key)
        .collect::<Vec<_>>();
    assert!(keys.contains(&"arm_home"));
    assert!(keys.contains(&"arm_night"));
    assert!(!keys.contains(&"arm_away"));
    assert_eq!(
        plan(&alarm, "arm_home", json!({"code":"0012"})).0,
        ActionKind::Alarm("armed_home".into())
    );
    assert_eq!(
        controls::plan(&alarm, "arm_home", &json!({})),
        Err(ActionError::InvalidValue)
    );
    assert_eq!(
        controls::plan(&alarm, "arm_away", &json!({"code":"1234"})),
        Err(ActionError::Unsupported)
    );
    assert_eq!(
        plan(&alarm, "disarm", json!({"code":"0012"})).3,
        json!({"code":"0012"})
    );
}

#[test]
fn numeric_select_and_climate_inputs_validate_ranges_options_and_pairs() {
    let number = tile(
        D::InputNumber,
        "1.5",
        json!({"min":0,"max":10,"step":0.5,"unit_of_measurement":"kWh"}),
    );
    assert_eq!(
        plan(&number, "set_value", json!(2.5)).3,
        json!({"value":2.5})
    );
    assert_eq!(
        controls::plan(&number, "set_value", &json!(2.2)),
        Err(ActionError::InvalidValue)
    );
    assert_eq!(
        controls::plan(&number, "set_value", &json!(11)),
        Err(ActionError::InvalidValue)
    );

    let select_tile = tile(D::Select, "eco", json!({"options":["eco","boost"]}));
    assert_eq!(
        plan(&select_tile, "select_option", json!("boost")).3,
        json!({"option":"boost"})
    );
    assert_eq!(
        controls::plan(&select_tile, "select_option", &json!("unlisted")),
        Err(ActionError::InvalidValue)
    );

    let climate = tile(
        D::Climate,
        "heat_cool",
        json!({
            "supported_features": CLIMATE_RANGE
                | CLIMATE_HUMIDITY
                | CLIMATE_PRESET
                | CLIMATE_FAN
                | CLIMATE_SWING_HORIZONTAL,
            "hvac_modes":["off","heat","heat_cool"],
            "preset_modes":["eco","comfort"],
            "fan_modes":["auto","low"],
            "min_temp":10,
            "max_temp":30,
            "target_temp_step":0.5,
            "target_temp_low":18,
            "target_temp_high":22,
            "min_humidity":30,
            "max_humidity":80,
            "humidity":55,
            "target_humidity":40,
            "swing_horizontal_modes":["left","right"],
        }),
    );
    let keys = controls::for_entity(&climate, false)
        .iter()
        .map(|control| control.key)
        .collect::<Vec<_>>();
    assert!(keys.contains(&"hvac_mode"));
    assert!(keys.contains(&"set_temperature_range"));
    assert!(keys.contains(&"set_humidity"));
    assert!(keys.contains(&"preset_mode"));
    assert!(!keys.contains(&"swing_mode"));
    assert!(keys.contains(&"swing_horizontal_mode"));
    assert_eq!(
        controls::for_entity(&climate, false)
            .iter()
            .find(|control| control.key == "set_humidity")
            .unwrap()
            .value
            .as_f64(),
        Some(55.0)
    );
    assert_eq!(
        plan(
            &climate,
            "set_temperature_range",
            json!({"target_temp_low":18.5,"target_temp_high":23})
        )
        .3,
        json!({"target_temp_low":18.5,"target_temp_high":23.0})
    );
    assert_eq!(
        controls::plan(
            &climate,
            "set_temperature_range",
            &json!({"target_temp_low":24,"target_temp_high":23})
        ),
        Err(ActionError::InvalidValue)
    );
    assert_eq!(
        controls::plan(
            &climate,
            "set_temperature_range",
            &json!({"target_temp_low":24,"target_temp_high":25,"hvac_mode":"heat_cool"})
        ),
        Err(ActionError::InvalidValue)
    );
}

#[test]
#[allow(clippy::too_many_lines)] // One table-driven integration test covers the everyday domains.
fn cover_and_everyday_domains_map_to_documented_service_schemas() {
    let cover = tile(
        D::Cover,
        "open",
        json!({"supported_features":COVER_OPEN | COVER_CLOSE | COVER_SET_POSITION | COVER_STOP | COVER_SET_TILT,"current_position":35,"current_tilt_position":20}),
    );
    assert_eq!(
        plan(&cover, "position", json!(65)).3,
        json!({"position":65.0})
    );
    assert_eq!(
        plan(&cover, "tilt_position", json!(50)).2,
        "set_cover_tilt_position"
    );
    assert_eq!(plan(&cover, "open", Value::Null).2, "open_cover");
    assert_eq!(
        controls::plan(&cover, "open_tilt", &Value::Null),
        Err(ActionError::Unsupported)
    );

    let humidifier = tile(
        D::Humidifier,
        "on",
        json!({"supported_features":HUMIDIFIER_MODES,"min_humidity":20,"max_humidity":80,"humidity":40,"available_modes":["normal","sleep"]}),
    );
    let humidifier_keys = controls::for_entity(&humidifier, false)
        .iter()
        .map(|control| control.key)
        .collect::<Vec<_>>();
    assert!(!humidifier_keys.contains(&"turn_on"));
    assert!(humidifier_keys.contains(&"turn_off"));
    assert_eq!(
        plan(&humidifier, "humidity", json!(55)).3,
        json!({"humidity":55})
    );
    assert_eq!(
        controls::plan(&humidifier, "humidity", &json!(90)),
        Err(ActionError::InvalidValue)
    );

    let valve = tile(
        D::Valve,
        "open",
        json!({"supported_features":VALVE_SET_POSITION,"current_position":50}),
    );
    assert_eq!(
        plan(&valve, "position", json!(25)).3,
        json!({"position":25.0})
    );
    assert_eq!(
        controls::plan(&valve, "close", &Value::Null),
        Err(ActionError::Unsupported)
    );

    let mower = tile(
        D::LawnMower,
        "mowing",
        json!({"supported_features":LAWN_START | LAWN_PAUSE | LAWN_STOP | LAWN_DOCK}),
    );
    assert_eq!(plan(&mower, "dock", Value::Null).2, "dock");
    assert_eq!(plan(&mower, "stop", Value::Null).2, "stop");
    let dock_only = tile(
        D::LawnMower,
        "idle",
        json!({"supported_features":LAWN_DOCK}),
    );
    assert!(
        controls::for_entity(&dock_only, false)
            .iter()
            .any(|item| item.key == "dock")
    );
    assert!(
        !controls::for_entity(&dock_only, false)
            .iter()
            .any(|item| item.key == "stop")
    );
    let stop_only = tile(
        D::LawnMower,
        "idle",
        json!({"supported_features":LAWN_STOP}),
    );
    assert!(
        controls::for_entity(&stop_only, false)
            .iter()
            .any(|item| item.key == "stop")
    );
    assert!(
        !controls::for_entity(&stop_only, false)
            .iter()
            .any(|item| item.key == "dock")
    );

    let siren = tile(
        D::Siren,
        "off",
        json!({"supported_features":SIREN_ON | SIREN_OFF | SIREN_TONE | SIREN_VOLUME | SIREN_DURATION,"available_tones":["alarm","chime"]}),
    );
    assert_eq!(
        plan(
            &siren,
            "turn_on",
            json!({"tone":"alarm","volume_level":0.75,"duration":30})
        )
        .3,
        json!({"tone":"alarm","volume_level":0.75,"duration":30.0})
    );
    assert_eq!(
        controls::plan(&siren, "turn_on", &json!({"tone":"unknown"})),
        Err(ActionError::InvalidValue)
    );
}

#[test]
fn helper_entities_and_observations_reject_arbitrary_service_data() {
    let automation = tile(D::Automation, "on", json!({}));
    assert_eq!(
        plan(&automation, "run", json!({"skip_condition":false})).3,
        json!({"skip_condition":false})
    );
    assert_eq!(
        controls::plan(
            &automation,
            "run",
            &json!({"skip_condition":true,"other":1})
        ),
        Err(ActionError::InvalidValue)
    );

    let weather = tile(
        D::Weather,
        "sunny",
        json!({"temperature":22,"supported_features":WEATHER_DAILY | WEATHER_HOURLY}),
    );
    let (kind, domain, service, data) = plan(&weather, "get_forecasts", json!("daily"));
    assert_eq!(kind, ActionKind::RunScene);
    assert_eq!(
        (domain.as_str(), service.as_str()),
        ("weather", "get_forecasts")
    );
    assert_eq!(data, json!({"type":"daily"}));
    assert_eq!(
        controls::plan(&weather, "get_forecasts", &json!("yearly")),
        Err(ActionError::InvalidValue)
    );

    let notify = tile(D::Notify, "", json!({}));
    assert_eq!(
        plan(&notify, "send_message", json!({"message":"Door opened"})).3,
        json!({"message":"Door opened"})
    );
    assert_eq!(
        controls::plan(
            &notify,
            "send_message",
            &json!({"message":"x","arbitrary":"payload"})
        ),
        Err(ActionError::InvalidValue)
    );

    let update = tile(
        D::Update,
        "on",
        json!({"supported_features":UPDATE_INSTALL | UPDATE_RELEASE_NOTES}),
    );
    assert_eq!(plan(&update, "install", Value::Null).2, "install");
    assert_eq!(plan(&update, "skip", Value::Null).2, "skip");
    let (_, release_notes) = controls::plan(&update, "release_notes", &Value::Null).unwrap();
    assert_eq!(release_notes.service, "release_notes");
    assert_eq!(
        controls::plan(&update, "install_version", &json!({"version":"1.2"})),
        Err(ActionError::Unsupported)
    );
    let with_backup = tile(
        D::Update,
        "on",
        json!({"supported_features":UPDATE_INSTALL | UPDATE_BACKUP}),
    );
    assert_eq!(
        plan(&with_backup, "install_with_backup", json!(false)).3,
        json!({"backup":false})
    );
    let skipped = tile(D::Update, "off", json!({"skipped_version":"1.2"}));
    assert_eq!(
        plan(&skipped, "clear_skipped", Value::Null).2,
        "clear_skipped"
    );
    assert_eq!(
        controls::plan(&skipped, "skip", &Value::Null),
        Err(ActionError::Unsupported)
    );
}

#[test]
fn simple_power_controls_follow_known_state_and_keep_power_icons() {
    let light_on = tile(
        D::Light,
        "on",
        json!({"brightness":128,"supported_color_modes":["brightness"]}),
    );
    let controls = controls::for_entity(&light_on, false);
    let keys = controls.iter().map(|item| item.key).collect::<Vec<_>>();
    assert!(!keys.contains(&"turn_on"));
    assert!(keys.contains(&"turn_off"));
    assert!(keys.contains(&"brightness"));
    assert_eq!(
        controls
            .iter()
            .find(|item| item.key == "turn_off")
            .unwrap()
            .icon,
        "power"
    );
    assert_eq!(
        controls::plan(&light_on, "turn_on", &Value::Null),
        Err(ActionError::Unsupported)
    );

    let fan_off = tile(D::Fan, "off", json!({"supported_features":FAN_TURN_ON}));
    let controls = controls::for_entity(&fan_off, false);
    assert!(controls.iter().any(|item| item.key == "turn_on"));
    assert!(!controls.iter().any(|item| item.key == "turn_off"));
    assert_eq!(
        controls
            .iter()
            .find(|item| item.key == "turn_on")
            .unwrap()
            .icon,
        "power"
    );

    let switch_unusual_state = tile(D::Switch, "idle", json!({}));
    let controls = controls::for_entity(&switch_unusual_state, false);
    assert!(controls.iter().any(|item| item.key == "turn_on"));
    assert!(controls.iter().any(|item| item.key == "turn_off"));

    let climate_off = tile(
        D::Climate,
        "off",
        json!({
            "supported_features":CLIMATE_TURN_ON | CLIMATE_TURN_OFF,
            "swing_horizontal_modes":["left","right"]
        }),
    );
    let controls = controls::for_entity(&climate_off, false);
    assert!(controls.iter().any(|item| item.key == "turn_on"));
    assert!(!controls.iter().any(|item| item.key == "turn_off"));
    assert!(
        !controls
            .iter()
            .any(|item| item.key == "swing_horizontal_mode")
    );
    assert_eq!(plan(&climate_off, "turn_on", Value::Null).2, "turn_on");

    let climate_horizontal = tile(
        D::Climate,
        "heat",
        json!({
            "supported_features":CLIMATE_SWING_HORIZONTAL,
            "swing_horizontal_modes":["left","right"]
        }),
    );
    assert_eq!(
        plan(&climate_horizontal, "swing_horizontal_mode", json!("left")).3,
        json!({"swing_horizontal_mode":"left"})
    );

    let vacuum = tile(D::Vacuum, "idle", json!({"supported_features":0}));
    let controls = controls::for_entity(&vacuum, false);
    assert!(
        !controls
            .iter()
            .any(|item| item.key == "turn_on" || item.key == "turn_off")
    );
    assert_eq!(
        controls::plan(&vacuum, "turn_on", &Value::Null),
        Err(ActionError::Unsupported)
    );

    let mapped_vacuum = tile(D::Vacuum, "idle", json!({"supported_features":16_384}));
    assert!(
        controls::for_entity(&mapped_vacuum, false)
            .iter()
            .any(|item| item.key == "clean_area")
    );
    assert_eq!(
        plan(
            &mapped_vacuum,
            "clean_area",
            json!({"area_ids":"living_room, kitchen"})
        )
        .3,
        json!({"cleaning_area_id":["living_room","kitchen"]})
    );
    for invalid in ["", "living-room", "living_room,living_room", "Living_room"] {
        assert_eq!(
            controls::plan(&mapped_vacuum, "clean_area", &json!({"area_ids":invalid})),
            Err(ActionError::InvalidValue)
        );
    }
}

#[test]
fn script_parameters_are_typed_and_mapped_to_declared_variable_names() {
    let script = tile(
        D::Script,
        "off",
        json!({
            "njorun_script_fields": {
                "enabled": {"required": false, "selector": {"boolean": {}}},
                "message": {"name": "Message", "required": true, "selector": {"text": {"max": 20}}},
                "mode": {"required": false, "selector": {"select": {"options": ["quiet", "loud"]}}}
            }
        }),
    );
    let control = controls::for_entity(&script, false)
        .into_iter()
        .find(|item| item.key == "start")
        .unwrap();
    assert_eq!(control.kind, ControlKind::Form);
    assert_eq!(
        control
            .inputs
            .iter()
            .map(|input| input.key)
            .collect::<Vec<_>>(),
        ["field_0", "field_1", "field_2"]
    );
    let (_, domain, service, data) = plan(
        &script,
        "start",
        json!({"field_0":true,"field_1":"Hello","field_2":"loud"}),
    );
    assert_eq!((domain.as_str(), service.as_str()), ("script", "turn_on"));
    assert_eq!(
        data,
        json!({"variables":{"enabled":true,"message":"Hello","mode":"loud"}})
    );
    assert_eq!(
        controls::plan(
            &script,
            "start",
            &json!({"field_0":true,"field_1":"This message exceeds the limit","field_2":"loud"})
        ),
        Err(ActionError::InvalidValue)
    );
    assert_eq!(
        controls::plan(
            &script,
            "start",
            &json!({"field_1":"ok","field_2":"unknown"})
        ),
        Err(ActionError::InvalidValue)
    );

    let clipped = tile(
        D::Script,
        "off",
        json!({
            "njorun_script_fields_omitted": 2,
            "njorun_script_fields": {"message":{"required":true,"selector":{"text":{"max":20}}}}
        }),
    );
    let start = controls::for_entity(&clipped, false)
        .into_iter()
        .find(|control| control.key == "start")
        .unwrap();
    assert!(!start.enabled);
    assert_eq!(start.group, "explanation");
    assert!(
        start
            .label
            .contains("Configure a wrapper script in Home Assistant")
    );
    assert_eq!(
        controls::plan(&clipped, "start", &json!({"field_0":"hello"})),
        Err(ActionError::Unsupported)
    );

    let missing = tile(D::Script, "off", json!({}));
    let start = controls::for_entity(&missing, false)
        .into_iter()
        .find(|control| control.key == "start")
        .unwrap();
    assert!(!start.enabled);
    assert_eq!(start.group, "explanation");
    assert!(start.label.contains("parameters could not be read"));
}

#[test]
fn script_number_entity_defaults_and_structured_selectors_are_preserved() {
    use njorun_app_core::controls::InputKind;

    let script = tile(
        D::Script,
        "off",
        json!({
            "njorun_script_fields": {
                "audience": {
                    "name": "Audience",
                    "required": true,
                    "selector": {"entity": {"domain": ["light", "switch"]}}
                },
                "count": {
                    "required": true,
                    "default": 2.5,
                    "selector": {"number": {"min": 1, "max": 5, "step": 0.5}}
                },
                "mode": {
                    "default": "quiet",
                    "selector": {"select": {"options": [
                        {"label": "Quiet", "value": "quiet"},
                        "loud"
                    ]}}
                },
                "title": {
                    "default": "Hello",
                    "selector": {"text": {"max": 40}}
                }
            }
        }),
    );
    let start = controls::for_entity(&script, false)
        .into_iter()
        .find(|control| control.key == "start")
        .expect("script start control");
    assert!(start.enabled);
    assert_eq!(
        start
            .inputs
            .iter()
            .map(|input| input.kind)
            .collect::<Vec<_>>(),
        [
            InputKind::Entity,
            InputKind::Number,
            InputKind::Select,
            InputKind::Text,
        ]
    );
    assert_eq!(start.inputs[0].options, ["light", "switch"]);
    assert_eq!(start.inputs[1].value, json!(2.5));
    assert_eq!(start.inputs[2].options, ["quiet", "loud"]);
    assert_eq!(start.inputs[2].value, json!("quiet"));
    assert_eq!(start.inputs[3].value, json!("Hello"));

    assert_eq!(
        plan(
            &script,
            "start",
            json!({
                "field_0":"light.kitchen",
                "field_1":3.5,
                "field_2":"loud",
                "field_3":"Dinner"
            }),
        )
        .3,
        json!({"variables":{
            "audience":"light.kitchen",
            "count":3.5,
            "mode":"loud",
            "title":"Dinner"
        }})
    );

    let unsupported = tile(
        D::Script,
        "off",
        json!({
            "njorun_script_fields": {
                "file": {"required": true, "selector": {"file": {}}}
            }
        }),
    );
    let start = controls::for_entity(&unsupported, false)
        .into_iter()
        .find(|control| control.key == "start")
        .expect("disabled script start control");
    assert!(!start.enabled);
    assert_eq!(start.group, "explanation");
}

#[test]
#[allow(clippy::too_many_lines)]
fn remote_camera_group_and_schedule_use_verified_service_schemas() {
    let remote = tile(
        D::Remote,
        "on",
        json!({
            "supported_features": REMOTE_LEARN | REMOTE_DELETE | REMOTE_ACTIVITY,
            "activity_list": ["Watch TV"]
        }),
    );
    let (_, domain, service, data) = plan(
        &remote,
        "send_command",
        json!({
            "command":"volume_up", "device":"television", "num_repeats":2, "delay_secs":0.5, "hold_secs":0.2
        }),
    );
    assert_eq!(
        (domain.as_str(), service.as_str()),
        ("remote", "send_command")
    );
    assert_eq!(
        data,
        json!({"command":"volume_up","device":"television","num_repeats":2,"delay_secs":0.5,"hold_secs":0.2})
    );
    assert_eq!(
        plan(&remote, "activity", json!("Watch TV")).3,
        json!({"activity":"Watch TV"})
    );
    assert_eq!(plan(&remote,"learn_command",json!({"device":"tv","command":"mute","command_type":"ir","alternative":false,"timeout":30})).2,"learn_command");
    assert_eq!(
        plan(&remote, "delete_command", json!({"command":"mute"})).3,
        json!({"command":"mute"})
    );
    assert_eq!(
        controls::plan(
            &remote,
            "send_command",
            &json!({"command":"volume_up","num_repeats":0})
        ),
        Err(ActionError::InvalidValue)
    );

    let camera = tile(
        D::Camera,
        "idle",
        json!({"supported_features":CAMERA_ON_OFF | CAMERA_STREAM,"motion_detection":false}),
    );
    assert_eq!(
        plan(
            &camera,
            "snapshot",
            json!({"filename":"/config/www/camera.jpg"})
        )
        .3,
        json!({"filename":"/config/www/camera.jpg"})
    );
    assert_eq!(
        plan(
            &camera,
            "record",
            json!({"filename":"/config/www/camera.mp4","duration":30,"lookback":5})
        )
        .3,
        json!({"filename":"/config/www/camera.mp4","duration":30,"lookback":5})
    );
    assert_eq!(
        plan(
            &camera,
            "play_stream",
            json!({"media_player":"media_player.living_room"})
        )
        .2,
        "play_stream"
    );
    assert_eq!(
        plan(&camera, "enable_motion_detection", Value::Null).2,
        "enable_motion_detection"
    );
    assert_eq!(
        controls::plan(
            &camera,
            "play_stream",
            &json!({"media_player":"switch.lamp"})
        ),
        Err(ActionError::InvalidValue)
    );

    let group = tile(
        D::Group,
        "off",
        json!({"entity_id":["light.kitchen","switch.pump"]}),
    );
    let (_, domain, service, _) = plan(&group, "turn_on", Value::Null);
    assert_eq!(
        (domain.as_str(), service.as_str()),
        ("homeassistant", "turn_on")
    );
    let unsupported_group = tile(D::Group, "off", json!({"entity_id":["sensor.temperature"]}));
    assert_eq!(controls::for_entity(&unsupported_group, false), Vec::new());
    assert_eq!(
        controls::plan(&unsupported_group, "turn_on", &Value::Null),
        Err(ActionError::Unsupported)
    );

    let schedule = tile(D::Schedule, "on", json!({}));
    let (_, _, _, call_data) = plan(&schedule, "get_schedule", Value::Null);
    assert_eq!(call_data, json!({}));
}

#[test]
#[allow(clippy::too_many_lines)] // The response and mutation schemas are validated as one contract.
fn calendar_todo_and_counter_validate_optional_fields_and_exact_values() {
    let calendar = tile(
        D::Calendar,
        "on",
        json!({"supported_features":CALENDAR_CREATE}),
    );
    let (kind, _, service, data) = plan(
        &calendar,
        "get_events",
        json!({"start_date_time":"2026-11-15T00:00:00","end_date_time":"2026-11-16T00:00:00"}),
    );
    assert_eq!(kind, ActionKind::RunScene);
    assert_eq!(service, "get_events");
    assert_eq!(
        data,
        json!({"start_date_time":"2026-11-15T00:00:00","end_date_time":"2026-11-16T00:00:00"})
    );
    let (_, _, service, data) = plan(
        &calendar,
        "create_event",
        json!({"summary":"Holiday","start_date":"2026-11-15","end_date":"2026-11-16","location":"Berlin"}),
    );
    assert_eq!(service, "create_event");
    assert_eq!(
        data,
        json!({"summary":"Holiday","start_date":"2026-11-15","end_date":"2026-11-16","location":"Berlin"})
    );
    assert_eq!(
        controls::plan(
            &calendar,
            "create_event",
            &json!({"summary":"Bad","start_date":"2026-11-15","end_date_time":"2026-11-16T00:00:00"})
        ),
        Err(ActionError::InvalidValue)
    );
    assert_eq!(
        controls::plan(&calendar, "delete_event", &Value::Null),
        Err(ActionError::Unsupported)
    );

    let todo = tile(
        D::Todo,
        "on",
        json!({"supported_features":TODO_CREATE | TODO_DELETE | TODO_UPDATE | TODO_MOVE | TODO_DUE_DATE | TODO_DUE_DATETIME | TODO_DESCRIPTION}),
    );
    let (_, _, service, data) = plan(
        &todo,
        "add_item",
        json!({"item":"Buy milk","due_date":"2026-11-15","description":"Oat milk"}),
    );
    assert_eq!(service, "add_item");
    assert_eq!(
        data,
        json!({"item":"Buy milk","due_date":"2026-11-15","description":"Oat milk"})
    );
    let (_, _, service, data) = plan(
        &todo,
        "update_item",
        json!({"item":"uid-1","status":"completed","rename":"Done"}),
    );
    assert_eq!(service, "update_item");
    assert_eq!(
        data,
        json!({"item":"uid-1","status":"completed","rename":"Done"})
    );
    let (_, domain, service, data) = plan(
        &todo,
        "move_item",
        json!({"uid":"todo-2","previous_uid":"todo-1"}),
    );
    assert_eq!((domain.as_str(), service.as_str()), ("todo", "move_item"));
    assert_eq!(data, json!({"uid":"todo-2","previous_uid":"todo-1"}));
    assert_eq!(
        controls::plan(
            &todo,
            "move_item",
            &json!({"uid":"todo-2","previous_uid":"todo-2"})
        ),
        Err(ActionError::InvalidValue)
    );
    let todo_without_move = tile(D::Todo, "on", json!({"supported_features":TODO_CREATE}));
    assert_eq!(
        controls::plan(&todo_without_move, "move_item", &json!({"uid":"todo-2"})),
        Err(ActionError::Unsupported)
    );
    assert_eq!(
        plan(&todo, "remove_completed_items", Value::Null).2,
        "remove_completed_items"
    );
    assert_eq!(
        controls::plan(
            &todo,
            "add_item",
            &json!({"item":"Bad","due_date":"2026-11-15","due_datetime":"2026-11-15T09:00:00Z"})
        ),
        Err(ActionError::InvalidValue)
    );

    let counter = tile(
        D::Counter,
        "4",
        json!({"minimum":-10,"maximum":10,"step":1}),
    );
    let (kind, _, service, data) = plan(&counter, "set_value_exact", json!({"value":-2}));
    assert_eq!(kind, ActionKind::SetNumberExact("-2".into()));
    assert_eq!(service, "set_value");
    assert_eq!(data, json!({"value":-2}));
    assert_eq!(
        controls::plan(&counter, "set_value_exact", &json!({"value":11})),
        Err(ActionError::InvalidValue)
    );
}

const CLIMATE_RANGE: u64 = 2;
const CLIMATE_HUMIDITY: u64 = 4;
const CLIMATE_PRESET: u64 = 16;
const CLIMATE_FAN: u64 = 8;
const CLIMATE_TURN_ON: u64 = 256;
const CLIMATE_TURN_OFF: u64 = 128;
const CLIMATE_SWING_HORIZONTAL: u64 = 512;
const FAN_TURN_OFF: u64 = 16;
const FAN_TURN_ON: u64 = 32;
const COVER_OPEN: u64 = 1;
const COVER_CLOSE: u64 = 2;
const COVER_SET_POSITION: u64 = 4;
const COVER_STOP: u64 = 8;
const COVER_SET_TILT: u64 = 128;
const HUMIDIFIER_MODES: u64 = 1;
const VALVE_SET_POSITION: u64 = 4;
const LAWN_START: u64 = 1;
const LAWN_PAUSE: u64 = 2;
const LAWN_DOCK: u64 = 4;
const LAWN_STOP: u64 = 8;
const SIREN_ON: u64 = 1;
const SIREN_OFF: u64 = 2;
const SIREN_TONE: u64 = 4;
const SIREN_VOLUME: u64 = 8;
const SIREN_DURATION: u64 = 16;
const UPDATE_INSTALL: u64 = 1;
const UPDATE_BACKUP: u64 = 8;
const UPDATE_RELEASE_NOTES: u64 = 16;
const REMOTE_LEARN: u64 = 1;
const REMOTE_DELETE: u64 = 2;
const REMOTE_ACTIVITY: u64 = 4;
const CAMERA_ON_OFF: u64 = 1;
const CAMERA_STREAM: u64 = 2;
const CALENDAR_CREATE: u64 = 1;
const TODO_CREATE: u64 = 1;
const TODO_DELETE: u64 = 2;
const TODO_UPDATE: u64 = 4;
const TODO_MOVE: u64 = 8;
const TODO_DUE_DATE: u64 = 16;
const TODO_DUE_DATETIME: u64 = 32;
const TODO_DESCRIPTION: u64 = 64;
const WEATHER_DAILY: u64 = 1;
const WEATHER_HOURLY: u64 = 2;
