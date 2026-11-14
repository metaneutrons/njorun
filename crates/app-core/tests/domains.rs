//! Fans, locks, media players, alarm panels, scripts, helpers and the details-sheet actions.
#![allow(clippy::needless_pass_by_value, clippy::float_cmp)] // Test fixtures favour brevity.

use njorun_app_core::presentation::{
    Control, detail_controls, detail_view, state_value, tile_active,
};
use njorun_app_core::{
    ActionAcknowledgement, ActionError, ActionFeedback, ActionKind, AlarmMode, AppCore,
    AreaRegistryEntry, ConnectionStatus, DashboardOrder, DetailAction, EntityDomain,
    EntityRegistryEntry, HaEntityState, HomeAssistantService, LockTarget, PlaybackTarget,
    PrimaryTap, TapBehavior,
};
use serde_json::{Value, json};

fn state(entity_id: &str, value: &str, attributes: Value) -> HaEntityState {
    HaEntityState {
        entity_id: entity_id.to_owned(),
        state: value.to_owned(),
        attributes: attributes
            .as_object()
            .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default(),
        last_updated: None,
    }
}

fn registry(entity_id: &str) -> EntityRegistryEntry {
    EntityRegistryEntry {
        entity_id: entity_id.to_owned(),
        area_id: Some("living".to_owned()),
        device_id: None,
        disabled_by: None,
        hidden_by: None,
        entity_category: None,
        name: None,
        icon: None,
    }
}

fn core_with(states: Vec<HaEntityState>) -> AppCore {
    let entities: Vec<_> = states.iter().map(|s| registry(&s.entity_id)).collect();
    let areas = vec![AreaRegistryEntry {
        area_id: "living".to_owned(),
        name: "Living room".to_owned(),
        floor_id: None,
    }];
    let mut core = AppCore::default();
    core.set_connection(ConnectionStatus::Ready);
    core.refresh_from_ha(&areas, &[], &entities, &states, &DashboardOrder::default())
        .expect("snapshot is valid");
    core
}

fn tile(core: &AppCore, entity_id: &str) -> njorun_app_core::EntityTile {
    core.dashboard().entity(entity_id).expect("tile").clone()
}

fn call_of(tap: PrimaryTap) -> (String, String, String, Value) {
    let (entity_id, call) = tap.into_call().expect("a service call");
    (
        entity_id,
        call.domain.to_owned(),
        call.service.to_owned(),
        call.data,
    )
}

#[test]
fn tap_behaviors_cover_every_domain() {
    use EntityDomain::*;
    for domain in [Light, Switch, Fan, InputBoolean, Automation] {
        assert_eq!(domain.tap_behavior(), TapBehavior::Toggle, "{domain:?}");
    }
    for domain in [Scene, Script, Button, InputButton] {
        assert_eq!(domain.tap_behavior(), TapBehavior::Press, "{domain:?}");
    }
    assert_eq!(MediaPlayer.tap_behavior(), TapBehavior::PlayPause);
    for domain in [
        Sensor,
        BinarySensor,
        Camera,
        Lock,
        AlarmControlPanel,
        Number,
        InputNumber,
        Select,
        InputSelect,
    ] {
        assert_eq!(domain.tap_behavior(), TapBehavior::Inspect, "{domain:?}");
    }
}

#[test]
fn toggling_domains_use_their_own_service() {
    let mut core = core_with(vec![
        state("fan.bedroom", "on", json!({"percentage": 40})),
        state("input_boolean.guest", "off", json!({})),
        state("automation.lights", "on", json!({})),
    ]);
    let expected = [
        ("fan.bedroom", HomeAssistantService::FanToggle),
        (
            "input_boolean.guest",
            HomeAssistantService::InputBooleanToggle,
        ),
        ("automation.lights", HomeAssistantService::AutomationToggle),
    ];
    for (entity_id, service) in expected {
        assert_eq!(
            core.primary_tap(entity_id),
            Ok(PrimaryTap::CallService {
                entity_id: entity_id.to_owned(),
                service
            })
        );
    }
}

#[test]
fn fan_primary_toggle_respects_target_feature_bits_when_advertised() {
    let mut core = core_with(vec![
        state("fan.turn_on", "off", json!({"supported_features": 32})),
        state("fan.turn_off", "on", json!({"supported_features": 16})),
        state("fan.wrong_on_bit", "off", json!({"supported_features": 16})),
        state("fan.wrong_off_bit", "on", json!({"supported_features": 32})),
        state("fan.no_features", "off", json!({"supported_features": 0})),
        state("fan.legacy", "on", json!({"percentage": 40})),
    ]);

    for entity_id in ["fan.turn_on", "fan.turn_off", "fan.legacy"] {
        assert!(tile(&core, entity_id).can_primary_action(), "{entity_id}");
        assert!(matches!(
            core.primary_tap(entity_id),
            Ok(PrimaryTap::CallService {
                service: HomeAssistantService::FanToggle,
                ..
            })
        ));
        core.acknowledge(entity_id, ActionAcknowledgement::Accepted)
            .unwrap();
    }

    for entity_id in ["fan.wrong_on_bit", "fan.wrong_off_bit", "fan.no_features"] {
        assert!(!tile(&core, entity_id).can_primary_action(), "{entity_id}");
        assert!(matches!(
            core.primary_tap(entity_id),
            Ok(PrimaryTap::Inspect { .. })
        ));
    }
}

#[test]
fn press_domains_work_while_their_state_is_unknown() {
    let mut core = core_with(vec![
        state("scene.movie", "unknown", json!({})),
        state(
            "script.goodnight",
            "off",
            json!({"njorun_script_fields": {}}),
        ),
        state("button.doorbell", "unknown", json!({})),
        state("input_button.reset", "unknown", json!({})),
    ]);
    for (entity_id, service) in [
        ("scene.movie", HomeAssistantService::SceneTurnOn),
        ("script.goodnight", HomeAssistantService::ScriptTurnOn),
        ("button.doorbell", HomeAssistantService::ButtonPress),
        ("input_button.reset", HomeAssistantService::InputButtonPress),
    ] {
        assert!(tile(&core, entity_id).is_available(), "{entity_id}");
        assert_eq!(
            core.primary_tap(entity_id),
            Ok(PrimaryTap::CallService {
                entity_id: entity_id.to_owned(),
                service
            })
        );
        core.acknowledge(entity_id, ActionAcknowledgement::Accepted)
            .unwrap();
        assert_eq!(
            tile(&core, entity_id).action_feedback,
            Some(ActionFeedback::SceneRequested)
        );
    }
}

#[test]
fn service_calls_have_domain_service_and_data() {
    let call = HomeAssistantService::MediaPlayPause.call();
    assert_eq!(
        (call.domain, call.service),
        ("media_player", "media_play_pause")
    );
    let message = call.message(7, "media_player.kitchen");
    assert_eq!(message["id"], 7);
    assert_eq!(message["type"], "call_service");
    assert_eq!(message["target"]["entity_id"], "media_player.kitchen");
    assert!(message.get("service_data").is_none());

    let mut core = core_with(vec![state(
        "light.desk",
        "on",
        json!({"supported_color_modes": ["brightness"], "brightness": 255}),
    )]);
    let tap = core
        .request_action("light.desk", &DetailAction::SetBrightness(40))
        .unwrap();
    let (entity_id, call) = tap.into_call().unwrap();
    let message = call.message(9, &entity_id);
    assert_eq!(message["service_data"]["brightness_pct"], 40);
}

#[test]
fn brightness_is_clamped_and_confirmed_by_the_new_state() {
    let mut core = core_with(vec![state(
        "light.desk",
        "on",
        json!({"supported_color_modes": ["brightness"], "brightness": 255}),
    )]);
    let tap = core
        .request_action("light.desk", &DetailAction::SetBrightness(0))
        .unwrap();
    let (_, domain, service, data) = call_of(tap);
    assert_eq!((domain.as_str(), service.as_str()), ("light", "turn_on"));
    assert_eq!(data["brightness_pct"], 1, "zero would be an off request");
    assert_eq!(
        core.request_action("light.desk", &DetailAction::SetBrightness(50)),
        Err(ActionError::AlreadyPending)
    );
    core.acknowledge("light.desk", ActionAcknowledgement::Accepted)
        .unwrap();
    assert!(matches!(
        tile(&core, "light.desk").action_feedback,
        Some(ActionFeedback::AcceptedAwaitingConfirmation {
            action: ActionKind::SetBrightness(1)
        })
    ));
    core.apply_state(state(
        "light.desk",
        "on",
        json!({"supported_color_modes": ["brightness"], "brightness": 3}),
    ))
    .unwrap();
    assert_eq!(tile(&core, "light.desk").action_feedback, None);
    assert_eq!(
        core.request_action("light.desk", &DetailAction::SetBrightness(50))
            .map(|tap| tap.into_call().is_some()),
        Ok(true),
        "confirmation frees the entity for the next action"
    );
}

#[test]
fn an_action_that_already_holds_confirms_on_acknowledgement() {
    let mut core = core_with(vec![state(
        "light.desk",
        "on",
        json!({"supported_color_modes": ["brightness"], "brightness": 128}),
    )]);
    core.request_action("light.desk", &DetailAction::SetBrightness(50))
        .unwrap();
    core.acknowledge("light.desk", ActionAcknowledgement::Accepted)
        .unwrap();
    assert_eq!(tile(&core, "light.desk").action_feedback, None);
    assert_eq!(
        core.mark_unconfirmed("light.desk"),
        Err(ActionError::NoPendingAction),
        "no pending action is left to time out"
    );
}

#[test]
fn lock_needs_the_details_sheet_and_waits_for_the_lock_state() {
    let mut core = core_with(vec![state("lock.front", "locked", json!({}))]);
    assert!(matches!(
        core.primary_tap("lock.front"),
        Ok(PrimaryTap::Inspect { .. })
    ));
    let tap = core
        .request_action("lock.front", &DetailAction::Unlock)
        .unwrap();
    let (_, domain, service, _) = call_of(tap);
    assert_eq!((domain.as_str(), service.as_str()), ("lock", "unlock"));
    assert!(matches!(
        tile(&core, "lock.front").action_feedback,
        Some(ActionFeedback::AwaitingAcknowledgement {
            action: ActionKind::Lock(LockTarget::Unlocked)
        })
    ));
    core.acknowledge("lock.front", ActionAcknowledgement::Accepted)
        .unwrap();
    core.apply_state(state("lock.front", "unlocking", json!({})))
        .unwrap();
    assert!(tile(&core, "lock.front").action_feedback.is_some());
    core.apply_state(state("lock.front", "unlocked", json!({})))
        .unwrap();
    assert_eq!(tile(&core, "lock.front").action_feedback, None);
}

#[test]
fn media_player_tap_pauses_and_resumes() {
    let mut core = core_with(vec![
        state(
            "media_player.tv",
            "playing",
            json!({"media_title": "News", "supported_features": 1}),
        ),
        state("media_player.radio", "off", json!({})),
    ]);
    assert!(matches!(
        core.primary_tap("media_player.tv"),
        Ok(PrimaryTap::CallService {
            service: HomeAssistantService::MediaPlayPause,
            ..
        })
    ));
    assert!(matches!(
        tile(&core, "media_player.tv").action_feedback,
        Some(ActionFeedback::AwaitingAcknowledgement {
            action: ActionKind::Playback(PlaybackTarget::Paused)
        })
    ));
    core.acknowledge("media_player.tv", ActionAcknowledgement::Accepted)
        .unwrap();
    assert!(
        matches!(
            core.primary_tap("media_player.radio"),
            Ok(PrimaryTap::Inspect { .. })
        ),
        "an off player cannot be resumed with play/pause"
    );
    core.apply_state(state("media_player.tv", "paused", json!({})))
        .unwrap();
    assert_eq!(tile(&core, "media_player.tv").action_feedback, None);
}

#[test]
fn primary_media_tap_requires_an_explicit_valid_feature_mask() {
    let mut core = core_with(vec![
        state("media_player.missing", "playing", json!({})),
        state(
            "media_player.malformed",
            "paused",
            json!({"supported_features": "pause,play"}),
        ),
        state(
            "media_player.unsupported",
            "playing",
            json!({"supported_features": 0}),
        ),
    ]);
    for entity_id in [
        "media_player.missing",
        "media_player.malformed",
        "media_player.unsupported",
    ] {
        assert_eq!(
            core.primary_tap(entity_id),
            Ok(PrimaryTap::Inspect {
                entity_id: entity_id.to_owned(),
                unavailable: false,
            })
        );
        assert_eq!(tile(&core, entity_id).action_feedback, None);
    }
}

#[test]
fn script_primary_action_requires_complete_optional_field_metadata() {
    let mut core = core_with(vec![
        state("script.simple", "off", json!({"njorun_script_fields": {}})),
        state(
            "script.required",
            "off",
            json!({
                "njorun_script_fields": {
                    "room": {"required": true, "selector": {"text": {}}}
                }
            }),
        ),
        state(
            "script.incomplete",
            "off",
            json!({
                "njorun_script_fields": {"room": {"required": false}},
                "njorun_script_fields_omitted": 2
            }),
        ),
        state("script.unknown_schema", "off", json!({})),
    ]);

    let simple = tile(&core, "script.simple");
    assert!(simple.can_primary_action());
    assert!(matches!(
        core.primary_tap("script.simple"),
        Ok(PrimaryTap::CallService {
            service: HomeAssistantService::ScriptTurnOn,
            ..
        })
    ));
    core.acknowledge("script.simple", ActionAcknowledgement::Accepted)
        .unwrap();

    for entity_id in [
        "script.required",
        "script.incomplete",
        "script.unknown_schema",
    ] {
        assert!(!tile(&core, entity_id).can_primary_action(), "{entity_id}");
        assert!(matches!(
            core.primary_tap(entity_id),
            Ok(PrimaryTap::Inspect { .. })
        ));
    }
}

#[test]
fn numbers_selects_and_volume_build_their_calls() {
    let mut core = core_with(vec![
        state(
            "number.target",
            "21.5",
            json!({"min": 5, "max": 30, "step": 0.5}),
        ),
        state(
            "input_select.mode",
            "Home",
            json!({"options": ["Home", "Away"]}),
        ),
        state("media_player.tv", "playing", json!({"volume_level": 0.3})),
        state("fan.bedroom", "on", json!({"percentage": 40})),
    ]);
    let cases: [(&str, DetailAction, &str, &str, Value); 4] = [
        (
            "number.target",
            DetailAction::SetNumber(22.5),
            "number",
            "set_value",
            json!({"value": 22.5}),
        ),
        (
            "input_select.mode",
            DetailAction::SelectOption("Away".into()),
            "input_select",
            "select_option",
            json!({"option": "Away"}),
        ),
        (
            "media_player.tv",
            DetailAction::SetVolume(55),
            "media_player",
            "volume_set",
            json!({"volume_level": 0.55}),
        ),
        (
            "fan.bedroom",
            DetailAction::SetFanSpeed(70),
            "fan",
            "set_percentage",
            json!({"percentage": 70}),
        ),
    ];
    for (entity_id, action, domain, service, data) in cases {
        let tap = core.request_action(entity_id, &action).unwrap();
        assert_eq!(
            call_of(tap),
            (
                entity_id.to_owned(),
                domain.to_owned(),
                service.to_owned(),
                data
            )
        );
        core.acknowledge(entity_id, ActionAcknowledgement::Accepted)
            .unwrap();
    }
    core.apply_state(state("number.target", "22.5", json!({"min": 5, "max": 30})))
        .unwrap();
    core.apply_state(state("input_select.mode", "Away", json!({})))
        .unwrap();
    core.apply_state(state(
        "media_player.tv",
        "playing",
        json!({"volume_level": 0.55}),
    ))
    .unwrap();
    core.apply_state(state("fan.bedroom", "on", json!({"percentage": 70})))
        .unwrap();
    for entity_id in [
        "number.target",
        "input_select.mode",
        "media_player.tv",
        "fan.bedroom",
    ] {
        assert_eq!(tile(&core, entity_id).action_feedback, None, "{entity_id}");
    }
}

#[test]
fn alarm_actions_carry_the_code_and_wait_for_the_panel_state() {
    let mut core = core_with(vec![state(
        "alarm_control_panel.house",
        "disarmed",
        json!({"code_format": "number", "supported_features": 7}),
    )]);
    let tap = core
        .request_action(
            "alarm_control_panel.house",
            &DetailAction::AlarmArm {
                mode: AlarmMode::Away,
                code: Some("1234".into()),
            },
        )
        .unwrap();
    let (_, domain, service, data) = call_of(tap);
    assert_eq!(
        (domain.as_str(), service.as_str()),
        ("alarm_control_panel", "alarm_arm_away")
    );
    assert_eq!(data, json!({"code": "1234"}));
    core.acknowledge("alarm_control_panel.house", ActionAcknowledgement::Accepted)
        .unwrap();
    core.apply_state(state("alarm_control_panel.house", "arming", json!({})))
        .unwrap();
    assert!(
        tile(&core, "alarm_control_panel.house")
            .action_feedback
            .is_some()
    );
    core.apply_state(state("alarm_control_panel.house", "armed_away", json!({})))
        .unwrap();
    assert_eq!(
        tile(&core, "alarm_control_panel.house").action_feedback,
        None
    );

    assert_eq!(
        core.request_action(
            "alarm_control_panel.house",
            &DetailAction::AlarmDisarm {
                code: Some("x".repeat(40))
            }
        ),
        Err(ActionError::InvalidValue)
    );
}

#[test]
fn actions_are_refused_for_the_wrong_domain_value_or_connection() {
    let mut core = core_with(vec![
        state("switch.plug", "on", json!({})),
        state("number.target", "5", json!({})),
        state("light.gone", "unavailable", json!({})),
    ]);
    assert_eq!(
        core.request_action("switch.plug", &DetailAction::Lock),
        Err(ActionError::Unsupported)
    );
    assert_eq!(
        core.request_action("number.target", &DetailAction::SetNumber(f64::NAN)),
        Err(ActionError::InvalidValue)
    );
    assert_eq!(
        core.request_action("light.gone", &DetailAction::SetBrightness(50)),
        Err(ActionError::Unavailable)
    );
    assert_eq!(
        core.request_action("missing.entity", &DetailAction::Lock),
        Err(ActionError::UnknownEntity)
    );
    core.set_connection(ConnectionStatus::Offline);
    assert_eq!(
        core.request_action("number.target", &DetailAction::SetNumber(6.0)),
        Err(ActionError::Offline)
    );
}

#[test]
fn legacy_actions_cannot_bypass_advertised_capabilities_or_required_codes() {
    let mut core = core_with(vec![
        state(
            "light.plain",
            "on",
            json!({"supported_color_modes": ["onoff"]}),
        ),
        state(
            "fan.no_speed",
            "on",
            json!({"supported_features": 0, "percentage": 30}),
        ),
        state(
            "media_player.no_volume",
            "playing",
            json!({"supported_features": 1 | 16_384, "volume_level": 0.5}),
        ),
        state("lock.coded", "locked", json!({"code_format": "text"})),
        state(
            "alarm_control_panel.required",
            "disarmed",
            json!({
                "code_format": "text",
                "code_arm_required": true,
                "supported_features": 3
            }),
        ),
    ]);

    assert_eq!(
        core.request_action("light.plain", &DetailAction::SetBrightness(50)),
        Err(ActionError::Unsupported)
    );
    assert_eq!(
        core.request_action("fan.no_speed", &DetailAction::SetFanSpeed(50)),
        Err(ActionError::Unsupported)
    );
    assert_eq!(
        core.request_action("media_player.no_volume", &DetailAction::SetVolume(50)),
        Err(ActionError::Unsupported)
    );
    assert_eq!(
        core.request_action("lock.coded", &DetailAction::Unlock),
        Err(ActionError::InvalidValue)
    );
    assert_eq!(
        core.request_action(
            "alarm_control_panel.required",
            &DetailAction::AlarmArm {
                mode: AlarmMode::Home,
                code: None,
            }
        ),
        Err(ActionError::InvalidValue)
    );
    assert_eq!(
        core.request_action(
            "alarm_control_panel.required",
            &DetailAction::AlarmArm {
                mode: AlarmMode::Night,
                code: Some("Freitext".to_owned()),
            }
        ),
        Err(ActionError::Unsupported)
    );
    assert_eq!(
        core.request_action(
            "alarm_control_panel.required",
            &DetailAction::AlarmDisarm { code: None }
        ),
        Err(ActionError::InvalidValue)
    );
    let tap = core
        .request_action(
            "alarm_control_panel.required",
            &DetailAction::AlarmDisarm {
                code: Some("Freitext".to_owned()),
            },
        )
        .unwrap();
    assert_eq!(tap.into_call().unwrap().1.data, json!({"code": "Freitext"}));
}

#[test]
fn response_services_set_return_response_and_media_queries_use_websocket_messages() {
    let mut core = core_with(vec![state(
        "weather.local",
        "sunny",
        json!({"supported_features": 1}),
    )]);
    let tap = core
        .request_action(
            "weather.local",
            &DetailAction::Entity {
                key: "get_forecasts".to_owned(),
                argument: json!("daily"),
            },
        )
        .unwrap();
    let (entity_id, call) = tap.into_call().unwrap();
    assert!(call.response_required());
    let payload = call.payload(&entity_id);
    assert_eq!(payload["type"], "call_service");
    assert_eq!(payload["return_response"], true);
    assert_eq!(payload["service_data"]["type"], "daily");

    let mut media_core = core_with(vec![state(
        "media_player.receiver",
        "idle",
        json!({"supported_features": 131_072}),
    )]);
    let media_tap = media_core
        .request_action(
            "media_player.receiver",
            &DetailAction::Entity {
                key: "media_browse".to_owned(),
                argument: json!({
                    "media_content_type": "music",
                    "media_content_id": "library"
                }),
            },
        )
        .unwrap();
    let (entity_id, media_query) = media_tap.into_call().unwrap();
    assert!(media_query.response_required());
    let payload = media_query.payload(&entity_id);
    assert_eq!(payload["type"], "media_player/browse_media");
    assert_eq!(payload["entity_id"], "media_player.receiver");
    assert_eq!(payload["media_content_type"], "music");
    assert!(payload.get("return_response").is_none());

    let mut history_core = core_with(vec![state("sensor.temperature", "21.5", json!({}))]);
    let history_tap = history_core
        .request_action(
            "sensor.temperature",
            &DetailAction::Entity {
                key: "history".to_owned(),
                argument: Value::Null,
            },
        )
        .unwrap();
    let (entity_id, history_call) = history_tap.into_call().unwrap();
    assert!(history_call.response_required());
    let payload = history_call.payload(&entity_id);
    assert_eq!(payload["type"], "history/history_during_period");
    assert_eq!(payload["entity_ids"], json!(["sensor.temperature"]));
    assert!(payload["start_time"].is_string());
    assert!(payload["end_time"].is_string());
    assert!(payload.get("return_response").is_none());
}

#[test]
fn details_controls_follow_the_entity_capabilities() {
    let core = core_with(vec![
        state(
            "light.dimmer",
            "on",
            json!({"supported_color_modes": ["xy"], "brightness": 128}),
        ),
        state(
            "light.plain",
            "on",
            json!({"supported_color_modes": ["onoff"]}),
        ),
        state(
            "fan.bedroom",
            "on",
            json!({"percentage": 33, "percentage_step": 33.3}),
        ),
        state("select.mode", "b", json!({"options": ["a", "b", "c"]})),
        state("lock.front", "locked", json!({})),
        state("media_player.tv", "paused", json!({"volume_level": 0.5})),
        state("camera.door", "idle", json!({})),
        state("automation.lights", "on", json!({})),
        state("switch.plug", "on", json!({})),
    ]);
    let Control::Slider(slider) = detail_controls(&tile(&core, "light.dimmer")) else {
        panic!("a dimmable light has a slider");
    };
    assert_eq!(
        (slider.action, slider.min, slider.max, slider.value),
        ("brightness", 1.0, 100.0, 50.0)
    );
    assert_eq!(detail_controls(&tile(&core, "light.plain")), Control::None);
    assert!(matches!(
        detail_controls(&tile(&core, "fan.bedroom")),
        Control::Slider(slider) if slider.action == "fan" && slider.value == 33.0
    ));
    assert_eq!(
        detail_controls(&tile(&core, "select.mode")),
        Control::Options {
            options: vec!["a".into(), "b".into(), "c".into()],
            selected: Some("b".into())
        }
    );
    assert_eq!(
        detail_controls(&tile(&core, "lock.front")),
        Control::Lock { locked: true }
    );
    assert!(matches!(
        detail_controls(&tile(&core, "media_player.tv")),
        Control::Media {
            playing: false,
            volume: Some(_)
        }
    ));
    assert_eq!(
        detail_controls(&tile(&core, "camera.door")),
        Control::Camera
    );
    assert_eq!(
        detail_controls(&tile(&core, "automation.lights")),
        Control::Automation
    );
    assert_eq!(detail_controls(&tile(&core, "switch.plug")), Control::None);
}

#[test]
fn alarm_view_lists_supported_modes_or_only_disarm() {
    let core = core_with(vec![
        state(
            "alarm_control_panel.house",
            "disarmed",
            json!({"supported_features": 3, "code_format": "number"}),
        ),
        state("alarm_control_panel.garage", "armed_away", json!({})),
    ]);
    let view = detail_view(&tile(&core, "alarm_control_panel.house"), false);
    assert_eq!(view.control, "alarm");
    assert!(view.needs_code);
    assert_eq!(
        view.actions,
        vec![
            ("arm_home".to_owned(), "Arm home".to_owned()),
            ("arm_away".to_owned(), "Arm away".to_owned())
        ]
    );
    let armed = detail_view(&tile(&core, "alarm_control_panel.garage"), true);
    assert!(!armed.needs_code);
    assert_eq!(
        armed.actions,
        vec![("disarm".to_owned(), "Deaktivieren".to_owned())]
    );
}

#[test]
fn ui_action_keys_become_domain_checked_actions() {
    use EntityDomain::*;
    assert_eq!(
        DetailAction::from_ui("level", "42.4", Light),
        Some(DetailAction::SetBrightness(42))
    );
    assert_eq!(
        DetailAction::from_ui("level", "250", Fan),
        Some(DetailAction::SetFanSpeed(100))
    );
    assert_eq!(
        DetailAction::from_ui("level", "21.5", Number),
        Some(DetailAction::SetNumber(21.5))
    );
    assert_eq!(DetailAction::from_ui("level", "x", Light), None);
    assert_eq!(DetailAction::from_ui("level", "1", Switch), None);
    assert_eq!(
        DetailAction::from_ui("arm_night", "99", AlarmControlPanel),
        Some(DetailAction::AlarmArm {
            mode: AlarmMode::Night,
            code: Some("99".into())
        })
    );
    assert_eq!(
        DetailAction::from_ui("disarm", "", AlarmControlPanel),
        Some(DetailAction::AlarmDisarm { code: None })
    );
    assert_eq!(DetailAction::from_ui("refresh_camera", "", Camera), None);
}

#[test]
fn tile_text_covers_the_new_domains_in_both_languages() {
    let core = core_with(vec![
        state("fan.bedroom", "on", json!({"percentage": 40})),
        state("lock.front", "jammed", json!({})),
        state("alarm_control_panel.house", "armed_away", json!({})),
        state(
            "media_player.tv",
            "playing",
            json!({"media_title": "Tagesschau", "media_artist": "ARD"}),
        ),
        state(
            "number.target",
            "21.5",
            json!({"unit_of_measurement": "°C"}),
        ),
        state("select.mode", "Eco", json!({})),
        state("script.goodnight", "unknown", json!({})),
        state("automation.lights", "off", json!({})),
        state("button.doorbell", "unknown", json!({})),
    ]);
    let text = |id: &str, german| state_value(&tile(&core, id), german);
    assert_eq!(text("fan.bedroom", false), "On · 40%");
    assert_eq!(text("lock.front", true), "Blockiert");
    assert_eq!(text("alarm_control_panel.house", false), "Armed away");
    assert_eq!(text("alarm_control_panel.house", true), "Abwesend scharf");
    assert_eq!(text("media_player.tv", false), "ARD – Tagesschau");
    assert_eq!(text("number.target", false), "21.5 °C");
    assert_eq!(text("select.mode", true), "Eco");
    assert_eq!(text("script.goodnight", true), "Skript ausführen");
    assert_eq!(text("automation.lights", false), "Disabled");
    assert_eq!(text("button.doorbell", false), "Press");
}

#[test]
fn tiles_light_up_for_active_states() {
    let core = core_with(vec![
        state("media_player.tv", "playing", json!({})),
        state("media_player.radio", "paused", json!({})),
        state("lock.front", "unlocked", json!({})),
        state("alarm_control_panel.house", "armed_home", json!({})),
        state("sensor.temp", "20", json!({})),
    ]);
    assert!(tile_active(&tile(&core, "media_player.tv")));
    assert!(!tile_active(&tile(&core, "media_player.radio")));
    assert!(tile_active(&tile(&core, "lock.front")));
    assert!(tile_active(&tile(&core, "alarm_control_panel.house")));
    assert!(!tile_active(&tile(&core, "sensor.temp")));
}

#[test]
fn a_matching_state_before_ack_does_not_mask_a_later_rejection() {
    let mut core = core_with(vec![state("light.desk", "off", json!({}))]);
    core.primary_tap("light.desk").expect("light action");
    core.apply_state(state("light.desk", "on", json!({})))
        .expect("matching state is valid");
    assert_eq!(
        tile(&core, "light.desk").action_feedback,
        Some(ActionFeedback::AwaitingAcknowledgement {
            action: ActionKind::Toggle(njorun_app_core::TargetState::On),
        })
    );
    core.acknowledge(
        "light.desk",
        ActionAcknowledgement::Rejected {
            reason: "service rejected".to_owned(),
        },
    )
    .expect("rejection must still match the in-flight request");
    assert_eq!(
        tile(&core, "light.desk").action_feedback,
        Some(ActionFeedback::Rejected {
            reason: "service rejected".to_owned(),
        })
    );
}

#[test]
fn a_matching_state_before_acceptance_confirms_after_the_ack() {
    let mut core = core_with(vec![state("light.desk", "off", json!({}))]);
    core.primary_tap("light.desk").expect("light action");
    core.apply_state(state("light.desk", "on", json!({})))
        .expect("matching state is valid");
    core.acknowledge("light.desk", ActionAcknowledgement::Accepted)
        .expect("acceptance must still match the in-flight request");
    assert_eq!(
        tile(&core, "light.desk").action_feedback,
        Some(ActionFeedback::Confirmed {
            state: njorun_app_core::TargetState::On,
        })
    );
}

#[test]
fn history_queries_work_for_unavailable_sensors_only() {
    let history_action = DetailAction::Entity {
        key: "history".to_owned(),
        argument: Value::Null,
    };
    let sensor_core = core_with(vec![state("sensor.temperature", "unavailable", json!({}))]);
    assert!(tile(&sensor_core, "sensor.temperature").command_enabled);
    let mut sensor_core = sensor_core;
    assert!(
        sensor_core
            .request_action("sensor.temperature", &history_action)
            .is_ok()
    );

    let mut light_core = core_with(vec![state("light.desk", "unavailable", json!({}))]);
    assert!(!tile(&light_core, "light.desk").command_enabled);
    assert_eq!(
        light_core.request_action("light.desk", &history_action),
        Err(ActionError::Unavailable)
    );
}
