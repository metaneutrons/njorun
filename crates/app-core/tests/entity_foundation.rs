//! Correctness checks for discovery, precision, and domain parsing.
#![allow(clippy::needless_pass_by_value, clippy::float_cmp)]

use njorun_app_core::{
    ActionAcknowledgement, ActionError, ActionFeedback, ActionKind, AppCore, AreaRegistryEntry,
    ConnectionStatus, DashboardOrder, DetailAction, EntityDomain, EntityRegistryEntry,
    HaEntityState, PrimaryTap, RoomId,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn state(entity_id: &str, state: &str, attributes: Value) -> HaEntityState {
    HaEntityState {
        entity_id: entity_id.to_owned(),
        state: state.to_owned(),
        attributes: attributes
            .as_object()
            .map(|attributes| {
                attributes
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
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

fn area() -> AreaRegistryEntry {
    AreaRegistryEntry {
        area_id: "living".to_owned(),
        name: "Living room".to_owned(),
        floor_id: None,
    }
}

#[test]
fn unknown_press_entities_stay_enabled_after_both_snapshot_paths() {
    let states = vec![
        state("scene.movie", "unknown", json!({})),
        state(
            "script.sleep",
            "unknown",
            json!({"njorun_script_fields": {}}),
        ),
        state("button.doorbell", "unknown", json!({})),
        state("input_button.reset", "unknown", json!({})),
    ];
    let entities: Vec<_> = states
        .iter()
        .map(|state| registry(&state.entity_id))
        .collect();
    let orders = [
        DashboardOrder::default(),
        DashboardOrder {
            area_ids: Some(vec!["living".to_owned()]),
            entity_ids_by_room: BTreeMap::from([(
                RoomId::Area("living".to_owned()),
                vec![
                    "input_button.reset".to_owned(),
                    "button.doorbell".to_owned(),
                    "script.sleep".to_owned(),
                    "scene.movie".to_owned(),
                ],
            )]),
        },
    ];

    for order in orders {
        let mut core = AppCore::default();
        core.set_connection(ConnectionStatus::Ready);
        core.refresh_from_ha(&[area()], &[], &entities, &states, &order)
            .unwrap();
        for entity_id in [
            "scene.movie",
            "script.sleep",
            "button.doorbell",
            "input_button.reset",
        ] {
            assert!(core.dashboard().entity(entity_id).unwrap().command_enabled);
            assert!(matches!(
                core.primary_tap(entity_id),
                Ok(PrimaryTap::CallService { .. })
            ));
            core.acknowledge(entity_id, ActionAcknowledgement::Accepted)
                .unwrap();
        }

        core.refresh_from_ha(&[area()], &[], &entities, &states, &order)
            .unwrap();
        for entity_id in [
            "scene.movie",
            "script.sleep",
            "button.doorbell",
            "input_button.reset",
        ] {
            let tile = core.dashboard().entity(entity_id).unwrap();
            assert!(tile.command_enabled, "{entity_id}");
            assert!(tile.is_available(), "{entity_id}");
            assert!(matches!(
                core.primary_tap(entity_id),
                Ok(PrimaryTap::CallService { .. })
            ));
        }
    }
}

#[test]
fn recognized_states_without_registry_rows_are_unassigned_and_deduplicated() {
    let mut hidden = registry("button.hidden");
    hidden.hidden_by = Some("user".to_owned());
    let mut disabled = registry("script.disabled");
    disabled.disabled_by = Some("integration".to_owned());
    let mut diagnostic = registry("scene.diagnostic");
    diagnostic.entity_category = Some("diagnostic".to_owned());
    let entities = vec![hidden, disabled, diagnostic];
    let states = vec![
        state("button.hidden", "unknown", json!({})),
        state("script.disabled", "off", json!({})),
        state("scene.diagnostic", "unknown", json!({})),
        state("remote.unregistered", "off", json!({})),
        state("script.orphan", "unknown", json!({})),
        state("not_a_domain.entity", "on", json!({})),
    ];
    let mut core = AppCore::default();
    core.set_connection(ConnectionStatus::Ready);
    core.refresh_from_ha(
        &[area()],
        &[],
        &entities,
        &states,
        &DashboardOrder::default(),
    )
    .unwrap();

    for excluded in ["button.hidden", "script.disabled", "scene.diagnostic"] {
        assert!(core.dashboard().entity(excluded).is_none(), "{excluded}");
    }
    let remote = core.dashboard().entity("remote.unregistered").unwrap();
    assert_eq!(remote.domain, EntityDomain::Remote);
    assert_eq!(remote.state.as_ref().unwrap().state, "off");
    assert_eq!(
        core.dashboard().entity("script.orphan").unwrap().domain,
        EntityDomain::Script
    );
    assert!(core.dashboard().entity("not_a_domain.entity").is_none());
    let unassigned = core
        .dashboard()
        .rooms
        .iter()
        .find(|room| room.id == RoomId::Unassigned)
        .unwrap();
    assert_eq!(unassigned.entities.len(), 2);
}

#[test]
fn number_planning_enforces_limits_steps_and_decimal_precision() {
    let mut core = AppCore::default();
    core.set_connection(ConnectionStatus::Ready);
    let number = registry("number.precise");
    core.refresh_from_ha(
        &[area()],
        &[],
        &[number],
        &[state(
            "number.precise",
            "1.000",
            json!({"min": 1.0, "max": 2.0, "step": 0.001}),
        )],
        &DashboardOrder::default(),
    )
    .unwrap();

    assert_eq!(
        core.request_action("number.precise", &DetailAction::SetNumber(0.999)),
        Err(ActionError::InvalidValue)
    );
    assert_eq!(
        core.request_action("number.precise", &DetailAction::SetNumber(1.0005)),
        Err(ActionError::InvalidValue),
        "a half step is not a valid target"
    );
    let tap = core
        .request_action("number.precise", &DetailAction::SetNumber(1.001))
        .unwrap();
    let (_, call) = tap.into_call().unwrap();
    assert_eq!(call.data, json!({"value": 1.001}));
    assert_eq!(
        core.dashboard()
            .entity("number.precise")
            .unwrap()
            .action_feedback,
        Some(ActionFeedback::AwaitingAcknowledgement {
            action: ActionKind::SetNumberExact("1.001".to_owned())
        })
    );
    core.acknowledge("number.precise", ActionAcknowledgement::Accepted)
        .unwrap();
    assert!(matches!(
        core.dashboard()
            .entity("number.precise")
            .unwrap()
            .action_feedback,
        Some(ActionFeedback::AcceptedAwaitingConfirmation { .. })
    ));
    core.apply_state(state(
        "number.precise",
        "1.000",
        json!({"min": 1.0, "max": 2.0, "step": 0.001}),
    ))
    .unwrap();
    assert!(
        core.dashboard()
            .entity("number.precise")
            .unwrap()
            .action_feedback
            .is_some()
    );
    core.apply_state(state(
        "number.precise",
        "1.001",
        json!({"min": 1.0, "max": 2.0, "step": 0.001}),
    ))
    .unwrap();
    assert_eq!(
        core.dashboard()
            .entity("number.precise")
            .unwrap()
            .action_feedback,
        None
    );
}

#[test]
fn number_confirmation_does_not_round_1_04_to_1_00() {
    let mut core = AppCore::default();
    core.set_connection(ConnectionStatus::Ready);
    core.refresh_from_ha(
        &[area()],
        &[],
        &[registry("number.precise")],
        &[state(
            "number.precise",
            "1.00",
            json!({"min": 0.0, "max": 2.0, "step": 0.01}),
        )],
        &DashboardOrder::default(),
    )
    .unwrap();
    core.request_action("number.precise", &DetailAction::SetNumber(1.04))
        .unwrap();
    core.acknowledge("number.precise", ActionAcknowledgement::Accepted)
        .unwrap();
    core.apply_state(state(
        "number.precise",
        "1.00",
        json!({"min": 0.0, "max": 2.0, "step": 0.01}),
    ))
    .unwrap();
    assert!(
        core.dashboard()
            .entity("number.precise")
            .unwrap()
            .action_feedback
            .is_some()
    );

    core.apply_state(state(
        "number.precise",
        "1.04",
        json!({"min": 0.0, "max": 2.0, "step": 0.01}),
    ))
    .unwrap();
    assert_eq!(
        core.dashboard()
            .entity("number.precise")
            .unwrap()
            .action_feedback,
        None
    );
}

#[test]
fn selects_must_match_a_current_option() {
    let mut core = AppCore::default();
    core.set_connection(ConnectionStatus::Ready);
    core.refresh_from_ha(
        &[area()],
        &[],
        &[registry("select.mode")],
        &[state(
            "select.mode",
            "home",
            json!({"options": ["home", "away"]}),
        )],
        &DashboardOrder::default(),
    )
    .unwrap();

    assert_eq!(
        core.request_action(
            "select.mode",
            &DetailAction::SelectOption("vacation".to_owned())
        ),
        Err(ActionError::InvalidValue)
    );
    assert!(
        core.request_action(
            "select.mode",
            &DetailAction::SelectOption("away".to_owned())
        )
        .is_ok()
    );
}

#[test]
fn entity_ui_actions_parse_json_or_keep_plain_text() {
    assert_eq!(
        DetailAction::from_ui(
            "entity:set_temperature",
            "{\"temperature\":21.5}",
            EntityDomain::Climate
        ),
        Some(DetailAction::Entity {
            key: "set_temperature".to_owned(),
            argument: json!({"temperature": 21.5}),
        })
    );
    assert_eq!(
        DetailAction::from_ui("entity:select_option", "Away", EntityDomain::Select),
        Some(DetailAction::Entity {
            key: "select_option".to_owned(),
            argument: Value::String("Away".to_owned()),
        })
    );
    assert_eq!(
        DetailAction::from_ui("entity:", "{}", EntityDomain::Climate),
        None
    );
}

#[test]
fn every_audited_domain_round_trips_through_entity_id_parsing() {
    assert!(EntityDomain::ALL.len() >= 60);
    for domain in EntityDomain::ALL {
        let entity_id = format!("{}.test_entity", domain.service_domain());
        assert_eq!(EntityDomain::from_entity_id(&entity_id), Some(*domain));
    }
    assert_eq!(EntityDomain::from_entity_id("unknown.test_entity"), None);
    assert_eq!(EntityDomain::from_entity_id("sensor."), None);
    assert_eq!(EntityDomain::from_entity_id("sensor.bad.extra"), None);
}
