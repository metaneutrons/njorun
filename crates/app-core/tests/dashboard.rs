use std::collections::BTreeMap;

use njorun_app_core::{
    ActionAcknowledgement, ActionError, ActionFeedback, ActionKind, AppCore, AreaRegistryEntry,
    ConnectionStatus, CoreError, DashboardOrder, DeviceRegistryEntry, EntityRegistryEntry,
    HaEntityState, HomeAssistantService, MAX_ACTION_REASON_CHARS, MAX_AREA_COUNT,
    MAX_ATTRIBUTE_COUNT, MAX_ATTRIBUTE_KEY_CHARS, MAX_DEVICE_COUNT, MAX_ENTITY_COUNT,
    MAX_ENTITY_REGISTRY_COUNT, MAX_IDENTIFIER_CHARS, MAX_LAST_UPDATED_CHARS, MAX_NAME_CHARS,
    MAX_STATE_CHARS, MAX_STATE_COUNT, PrimaryTap, RoomId, TargetState, build_dashboard,
};
use serde_json::{Value, json};

fn area(area_id: &str, name: &str) -> AreaRegistryEntry {
    AreaRegistryEntry {
        area_id: area_id.to_owned(),
        name: name.to_owned(),
        floor_id: None,
    }
}

fn device(id: &str, area_id: Option<&str>, via_device_id: Option<&str>) -> DeviceRegistryEntry {
    DeviceRegistryEntry {
        id: id.to_owned(),
        area_id: area_id.map(str::to_owned),
        via_device_id: via_device_id.map(str::to_owned),
    }
}

fn entity(entity_id: &str, area_id: Option<&str>, device_id: Option<&str>) -> EntityRegistryEntry {
    EntityRegistryEntry {
        entity_id: entity_id.to_owned(),
        area_id: area_id.map(str::to_owned),
        device_id: device_id.map(str::to_owned),
        disabled_by: None,
        hidden_by: None,
        entity_category: None,
        name: None,
        icon: None,
    }
}

fn state(entity_id: &str, value: &str, attributes: BTreeMap<String, Value>) -> HaEntityState {
    HaEntityState {
        entity_id: entity_id.to_owned(),
        state: value.to_owned(),
        attributes,
        last_updated: Some("2026-11-05T12:00:00+00:00".to_owned()),
    }
}

fn states(rows: &[(&str, &str)]) -> Vec<HaEntityState> {
    rows.iter()
        .map(|(entity_id, value)| state(entity_id, value, BTreeMap::new()))
        .collect()
}

#[test]
fn empty_snapshot_has_no_unassigned_room() {
    let dashboard = build_dashboard(
        &[],
        &[],
        &[],
        &[],
        &DashboardOrder::default(),
        ConnectionStatus::Offline,
    )
    .expect("empty HA snapshot should be valid");

    assert!(dashboard.is_empty());
    assert!(dashboard.is_stale);
}

#[test]
fn registry_filtering_and_area_resolution_keep_unassigned_last_only_when_populated() {
    let areas = [area("kitchen", "Kitchen"), area("office", "Office")];
    let devices = [
        device("office-parent", Some("office"), None),
        device("office-child", None, Some("office-parent")),
        device("cycle-a", None, Some("cycle-b")),
        device("cycle-b", None, Some("cycle-a")),
    ];
    let mut hidden = entity("light.hidden", None, None);
    hidden.hidden_by = Some("user".to_owned());
    let mut disabled = entity("switch.disabled", None, None);
    disabled.disabled_by = Some("integration".to_owned());
    let mut diagnostic = entity("sensor.internal", None, None);
    diagnostic.entity_category = Some("diagnostic".to_owned());
    let entities = [
        entity("light.kitchen", Some("kitchen"), Some("office-parent")),
        entity("switch.office", None, Some("office-child")),
        entity("sensor.cycle", None, Some("cycle-a")),
        entity("sensor.free", None, None),
        hidden,
        disabled,
        diagnostic,
        entity("custom.unsupported", None, None),
    ];
    let state_rows = states(&[
        ("light.kitchen", "off"),
        ("switch.office", "on"),
        ("sensor.cycle", "20"),
        ("sensor.free", "21"),
    ]);

    let dashboard = build_dashboard(
        &areas,
        &devices,
        &entities,
        &state_rows,
        &DashboardOrder::default(),
        ConnectionStatus::Ready,
    )
    .expect("valid registry snapshot should build");

    assert_eq!(
        dashboard
            .rooms
            .iter()
            .map(|room| &room.id)
            .collect::<Vec<_>>(),
        [
            &RoomId::Area("kitchen".to_owned()),
            &RoomId::Area("office".to_owned()),
            &RoomId::Unassigned,
        ]
    );
    assert_eq!(
        dashboard.rooms[0].entities[0].entity_id, "light.kitchen",
        "entity area must override device area"
    );
    assert_eq!(dashboard.rooms[1].entities[0].entity_id, "switch.office");
    assert_eq!(
        dashboard.rooms[2]
            .entities
            .iter()
            .map(|tile| tile.entity_id.as_str())
            .collect::<Vec<_>>(),
        ["sensor.cycle", "sensor.free"],
        "device-parent cycles must terminate and resolve to Unassigned"
    );
    assert!(
        dashboard
            .rooms
            .iter()
            .flat_map(|room| &room.entities)
            .all(|tile| !tile.entity_id.contains("hidden")
                && !tile.entity_id.contains("disabled")
                && !tile.entity_id.contains("internal")
                && !tile.entity_id.contains("unsupported"))
    );
}

#[test]
fn explicit_room_and_tile_order_appends_new_items_in_home_assistant_order() {
    let areas = [
        area("kitchen", "Kitchen"),
        area("office", "Office"),
        area("hall", "Hall"),
    ];
    let entities = [
        entity("light.first", Some("kitchen"), None),
        entity("light.second", Some("kitchen"), None),
        entity("light.third", Some("kitchen"), None),
    ];
    let state_rows = states(&[
        ("light.first", "off"),
        ("light.second", "off"),
        ("light.third", "off"),
    ]);
    let order = DashboardOrder {
        area_ids: Some(vec!["office".to_owned(), "missing".to_owned()]),
        entity_ids_by_room: BTreeMap::from([(
            RoomId::Area("kitchen".to_owned()),
            vec!["light.third".to_owned(), "deleted".to_owned()],
        )]),
    };

    let dashboard = build_dashboard(
        &areas,
        &[],
        &entities,
        &state_rows,
        &order,
        ConnectionStatus::Ready,
    )
    .expect("order with removed entries should be reconciled");

    assert_eq!(
        dashboard
            .rooms
            .iter()
            .map(|room| &room.id)
            .collect::<Vec<_>>(),
        [
            &RoomId::Area("office".to_owned()),
            &RoomId::Area("kitchen".to_owned()),
            &RoomId::Area("hall".to_owned()),
        ]
    );
    assert_eq!(
        dashboard.rooms[1]
            .entities
            .iter()
            .map(|tile| tile.entity_id.as_str())
            .collect::<Vec<_>>(),
        ["light.third", "light.first", "light.second"]
    );
}

#[test]
fn ha_state_attributes_and_timestamp_survive_into_the_public_tile_model() {
    let mut attributes = BTreeMap::new();
    attributes.insert("friendly_name".to_owned(), json!("Counter lamp"));
    attributes.insert("brightness".to_owned(), json!(155));
    attributes.insert("unit_of_measurement".to_owned(), json!("%"));
    attributes.insert("icon".to_owned(), json!("mdi:lightbulb"));
    let state = state("light.counter", "on", attributes);
    let dashboard = build_dashboard(
        &[area("room", "Room")],
        &[],
        &[entity("light.counter", Some("room"), None)],
        std::slice::from_ref(&state),
        &DashboardOrder::default(),
        ConnectionStatus::Ready,
    )
    .expect("well-formed HA state should build");
    let tile = &dashboard.rooms[0].entities[0];

    assert_eq!(tile.name, "Counter lamp");
    assert_eq!(tile.icon.as_deref(), Some("mdi:lightbulb"));
    assert_eq!(tile.state.as_ref(), Some(&state));
    assert_eq!(
        tile.state
            .as_ref()
            .and_then(|state| state.attributes.get("brightness")),
        Some(&json!(155))
    );
    assert_eq!(
        tile.state
            .as_ref()
            .and_then(|state| state.last_updated.as_deref()),
        Some("2026-11-05T12:00:00+00:00")
    );
}

#[test]
fn offline_and_synchronizing_states_are_stale_and_never_queue_actions() {
    let mut core = AppCore::new();
    core.refresh_from_ha(
        &[],
        &[],
        &[
            entity("light.desk", None, None),
            entity("sensor.temperature", None, None),
        ],
        &states(&[("light.desk", "off"), ("sensor.temperature", "21.4")]),
        &DashboardOrder::default(),
    )
    .expect("cached data should load offline");

    assert!(core.dashboard().is_stale);
    assert!(core.dashboard().entity("light.desk").unwrap().is_stale);
    assert!(
        !core
            .dashboard()
            .entity("light.desk")
            .unwrap()
            .command_enabled
    );
    assert_eq!(core.primary_tap("light.desk"), Err(ActionError::Offline));
    assert_eq!(
        core.primary_tap("sensor.temperature"),
        Ok(PrimaryTap::Inspect {
            entity_id: "sensor.temperature".to_owned(),
            unavailable: false,
        }),
        "read-only details remain available offline"
    );

    core.set_connection(ConnectionStatus::Synchronizing);
    assert!(core.dashboard().is_stale);
    assert_eq!(
        core.primary_tap("light.desk"),
        Err(ActionError::Synchronizing)
    );
    core.set_connection(ConnectionStatus::Ready);
    assert!(!core.dashboard().is_stale);
    assert!(
        core.dashboard()
            .entity("light.desk")
            .unwrap()
            .command_enabled
    );
    assert!(matches!(
        core.primary_tap("light.desk"),
        Ok(PrimaryTap::CallService { .. })
    ));
}

#[test]
fn acknowledgement_is_not_confirmation_and_matching_ha_state_is() {
    let mut core = AppCore::new();
    core.set_connection(ConnectionStatus::Ready);
    core.refresh_from_ha(
        &[],
        &[],
        &[entity("light.desk", None, None)],
        &states(&[("light.desk", "off")]),
        &DashboardOrder::default(),
    )
    .expect("snapshot should load");

    assert_eq!(
        core.primary_tap("light.desk"),
        Ok(PrimaryTap::CallService {
            entity_id: "light.desk".to_owned(),
            service: HomeAssistantService::LightToggle,
        })
    );
    assert_eq!(
        core.primary_tap("light.desk"),
        Err(ActionError::AlreadyPending)
    );
    core.acknowledge("light.desk", ActionAcknowledgement::Accepted)
        .expect("HA acceptance should be recorded");
    assert_eq!(
        core.dashboard()
            .entity("light.desk")
            .unwrap()
            .action_feedback,
        Some(ActionFeedback::AcceptedAwaitingState {
            target: TargetState::On,
        })
    );
    assert_eq!(
        core.dashboard().entity("light.desk").unwrap().state_value(),
        Some("off"),
        "accepted service response must not mutate authoritative state"
    );
    core.apply_state(state("light.desk", "off", BTreeMap::new()))
        .expect("nonmatching authoritative state is valid");
    assert!(matches!(
        core.dashboard()
            .entity("light.desk")
            .unwrap()
            .action_feedback,
        Some(ActionFeedback::AcceptedAwaitingState {
            target: TargetState::On,
        })
    ));
    core.apply_state(state("light.desk", "on", BTreeMap::new()))
        .expect("matching authoritative state is valid");
    assert_eq!(
        core.dashboard()
            .entity("light.desk")
            .unwrap()
            .action_feedback,
        Some(ActionFeedback::Confirmed {
            state: TargetState::On,
        })
    );
    assert_eq!(
        core.dashboard().entity("light.desk").unwrap().state_value(),
        Some("on")
    );
}

#[test]
fn disconnect_marks_inflight_unconfirmed_and_reconnect_does_not_replay() {
    let mut core = AppCore::new();
    core.set_connection(ConnectionStatus::Ready);
    core.refresh_from_ha(
        &[],
        &[],
        &[entity("switch.fan", None, None)],
        &states(&[("switch.fan", "off")]),
        &DashboardOrder::default(),
    )
    .expect("snapshot should load");
    assert!(matches!(
        core.primary_tap("switch.fan"),
        Ok(PrimaryTap::CallService {
            service: HomeAssistantService::SwitchToggle,
            ..
        })
    ));

    core.set_connection(ConnectionStatus::Offline);
    assert_eq!(
        core.dashboard()
            .entity("switch.fan")
            .unwrap()
            .action_feedback,
        Some(ActionFeedback::Unconfirmed {
            action: njorun_app_core::ActionKind::Toggle(TargetState::On),
        })
    );
    assert_eq!(
        core.primary_tap("switch.fan"),
        Err(ActionError::Offline),
        "offline taps do not produce or queue service calls"
    );

    core.set_connection(ConnectionStatus::Synchronizing);
    core.refresh_from_ha(
        &[],
        &[],
        &[entity("switch.fan", None, None)],
        &states(&[("switch.fan", "off")]),
        &DashboardOrder::default(),
    )
    .expect("fresh snapshot should replace cached state");
    assert!(core.dashboard().is_stale);
    assert_eq!(
        core.dashboard()
            .entity("switch.fan")
            .unwrap()
            .action_feedback,
        None,
        "fresh state clears the transient unconfirmed marker without claiming success"
    );

    core.set_connection(ConnectionStatus::Ready);
    assert_eq!(
        core.dashboard().entity("switch.fan").unwrap().state_value(),
        Some("off"),
        "the old action was not replayed during reconnect"
    );
    assert!(matches!(
        core.primary_tap("switch.fan"),
        Ok(PrimaryTap::CallService {
            service: HomeAssistantService::SwitchToggle,
            ..
        })
    ));
}

#[test]
fn scene_acknowledgement_reports_receipt_without_claiming_state_confirmation() {
    let mut core = AppCore::new();
    core.set_connection(ConnectionStatus::Ready);
    core.refresh_from_ha(
        &[],
        &[],
        &[entity("scene.evening", None, None)],
        &states(&[("scene.evening", "2026-11-05T11:00:00+00:00")]),
        &DashboardOrder::default(),
    )
    .expect("snapshot should load");

    assert_eq!(
        core.primary_tap("scene.evening"),
        Ok(PrimaryTap::CallService {
            entity_id: "scene.evening".to_owned(),
            service: HomeAssistantService::SceneTurnOn,
        })
    );
    core.acknowledge("scene.evening", ActionAcknowledgement::Accepted)
        .expect("scene request should be accepted");
    assert_eq!(
        core.dashboard()
            .entity("scene.evening")
            .unwrap()
            .action_feedback,
        Some(ActionFeedback::SceneRequested)
    );
    assert_eq!(
        core.dashboard()
            .entity("scene.evening")
            .unwrap()
            .state_value(),
        Some("2026-11-05T11:00:00+00:00"),
        "acknowledgement does not rewrite the HA scene state"
    );
}

#[test]
fn unavailable_tiles_open_details_and_unsupported_entities_are_not_in_the_dashboard() {
    let mut core = AppCore::new();
    core.set_connection(ConnectionStatus::Ready);
    core.refresh_from_ha(
        &[],
        &[],
        &[
            entity("light.missing", None, None),
            entity("custom.garage", None, None),
        ],
        &[state("light.missing", "unavailable", BTreeMap::new())],
        &DashboardOrder::default(),
    )
    .expect("snapshot should load");

    assert_eq!(
        core.primary_tap("light.missing"),
        Ok(PrimaryTap::Inspect {
            entity_id: "light.missing".to_owned(),
            unavailable: true,
        })
    );
    assert!(core.dashboard().entity("custom.garage").is_none());
    assert_eq!(
        core.primary_tap("custom.garage"),
        Err(ActionError::UnknownEntity)
    );
}

#[test]
fn invalid_refresh_preserves_dashboard_and_inflight_action() {
    let mut core = AppCore::new();
    let registry = [entity("light.desk", None, None)];
    core.refresh_from_ha(
        &[],
        &[],
        &registry,
        &states(&[("light.desk", "off")]),
        &DashboardOrder::default(),
    )
    .expect("snapshot");
    core.set_connection(ConnectionStatus::Ready);
    core.primary_tap("light.desk").expect("tap");
    let previous = core.dashboard().clone();
    assert!(
        core.refresh_from_ha(
            &[area("duplicate", "First"), area("duplicate", "Second")],
            &[],
            &registry,
            &states(&[("light.desk", "on")]),
            &DashboardOrder::default()
        )
        .is_err()
    );
    assert_eq!(core.dashboard(), &previous);
    assert_eq!(
        core.primary_tap("light.desk"),
        Err(ActionError::AlreadyPending)
    );
    core.acknowledge(
        "light.desk",
        ActionAcknowledgement::Rejected {
            reason: "not allowed".into(),
        },
    )
    .expect("reject");
    assert_eq!(
        core.dashboard()
            .entity("light.desk")
            .expect("tile")
            .action_feedback,
        Some(ActionFeedback::Rejected {
            reason: "not allowed".into()
        })
    );
    core.clear_action_feedback("light.desk").expect("dismiss");
    assert!(
        core.dashboard()
            .entity("light.desk")
            .expect("tile")
            .action_feedback
            .is_none()
    );
    core.primary_tap("light.desk")
        .expect("explicit retry allowed");
    core.mark_unconfirmed("light.desk").expect("timeout");
    assert!(matches!(
        core.dashboard()
            .entity("light.desk")
            .expect("tile")
            .action_feedback,
        Some(ActionFeedback::Unconfirmed { .. })
    ));
    core.refresh_from_ha(
        &[],
        &[],
        &registry,
        &states(&[("light.desk", "on")]),
        &DashboardOrder::default(),
    )
    .expect("fresh snapshot");
    assert!(
        core.dashboard()
            .entity("light.desk")
            .expect("tile")
            .action_feedback
            .is_none()
    );
}

#[test]
fn default_core_and_connection_status_report_offline_state() {
    let core = AppCore::default();

    assert_eq!(core.dashboard().connection, ConnectionStatus::Offline);
    assert!(core.dashboard().is_stale);
    assert!(core.dashboard().is_empty());
    assert!(ConnectionStatus::Offline.is_offline());
    assert!(!ConnectionStatus::Synchronizing.is_offline());
    assert!(!ConnectionStatus::Ready.is_offline());
}

#[test]
fn action_validation_preserves_pending_work_and_off_toggles_confirm_correctly() {
    let mut core = AppCore::new();
    core.set_connection(ConnectionStatus::Ready);
    core.refresh_from_ha(
        &[],
        &[],
        &[
            entity("light.already_on", None, None),
            entity("light.transitioning", None, None),
        ],
        &states(&[
            ("light.already_on", "on"),
            ("light.transitioning", "transitioning"),
        ]),
        &DashboardOrder::default(),
    )
    .expect("valid snapshot should load");

    assert_eq!(
        core.primary_tap("light.missing"),
        Err(ActionError::UnknownEntity)
    );
    assert_eq!(
        core.primary_tap("light.transitioning"),
        Err(ActionError::Unavailable),
        "nonstandard but available state must not produce a speculative toggle"
    );
    assert_eq!(
        core.primary_tap("light.already_on"),
        Ok(PrimaryTap::CallService {
            entity_id: "light.already_on".to_owned(),
            service: HomeAssistantService::LightToggle,
        })
    );
    assert_eq!(
        core.dashboard()
            .entity("light.already_on")
            .unwrap()
            .action_feedback,
        Some(ActionFeedback::AwaitingAcknowledgement {
            action: ActionKind::Toggle(TargetState::Off),
        })
    );

    let long_reason = "x".repeat(MAX_ACTION_REASON_CHARS + 1);
    assert_eq!(
        core.acknowledge(
            "light.already_on",
            ActionAcknowledgement::Rejected {
                reason: long_reason,
            }
        ),
        Err(ActionError::InvalidReason)
    );
    assert_eq!(
        core.primary_tap("light.already_on"),
        Err(ActionError::AlreadyPending),
        "invalid rejection data must not discard the in-flight request"
    );

    core.acknowledge("light.already_on", ActionAcknowledgement::Accepted)
        .expect("acceptance should be retained until matching state arrives");
    core.apply_state(state("light.already_on", "on", BTreeMap::new()))
        .expect("nonmatching state should be accepted");
    assert_eq!(
        core.dashboard()
            .entity("light.already_on")
            .unwrap()
            .action_feedback,
        Some(ActionFeedback::AcceptedAwaitingState {
            target: TargetState::Off,
        })
    );
    core.apply_state(state("light.already_on", "off", BTreeMap::new()))
        .expect("matching state should confirm the toggle");
    assert_eq!(
        core.dashboard()
            .entity("light.already_on")
            .unwrap()
            .action_feedback,
        Some(ActionFeedback::Confirmed {
            state: TargetState::Off,
        })
    );
    assert_eq!(
        core.acknowledge("light.already_on", ActionAcknowledgement::Accepted),
        Err(ActionError::NoPendingAction)
    );
}

#[test]
fn invalid_state_updates_leave_the_visible_dashboard_unchanged() {
    let mut core = AppCore::new();
    core.set_connection(ConnectionStatus::Ready);
    core.refresh_from_ha(
        &[],
        &[],
        &[entity("light.desk", None, None)],
        &states(&[("light.desk", "off")]),
        &DashboardOrder::default(),
    )
    .expect("valid snapshot should load");
    let previous = core.dashboard().clone();

    let mut too_many_attributes = BTreeMap::new();
    for index in 0..=MAX_ATTRIBUTE_COUNT {
        too_many_attributes.insert(format!("key-{index}"), json!(index));
    }
    let mut long_attribute_key = BTreeMap::new();
    long_attribute_key.insert("k".repeat(MAX_ATTRIBUTE_KEY_CHARS + 1), json!(true));
    let mut long_icon = BTreeMap::new();
    long_icon.insert("icon".to_owned(), json!("i".repeat(MAX_NAME_CHARS + 1)));
    let mut long_friendly_name = BTreeMap::new();
    long_friendly_name.insert(
        "friendly_name".to_owned(),
        json!("n".repeat(MAX_NAME_CHARS + 1)),
    );

    let invalid_updates = [
        (
            state(
                "light.desk",
                &"s".repeat(MAX_STATE_CHARS + 1),
                BTreeMap::new(),
            ),
            CoreError::StateTooLong,
        ),
        (
            HaEntityState {
                last_updated: Some("t".repeat(MAX_LAST_UPDATED_CHARS + 1)),
                ..state("light.desk", "on", BTreeMap::new())
            },
            CoreError::StateTooLong,
        ),
        (
            state("light.desk", "on", too_many_attributes),
            CoreError::TooManyAttributes,
        ),
        (
            state("light.desk", "on", long_attribute_key),
            CoreError::AttributeKeyTooLong,
        ),
        (state("light.desk", "on", long_icon), CoreError::NameTooLong),
        (
            state("light.desk", "on", long_friendly_name),
            CoreError::NameTooLong,
        ),
        (
            state(
                &format!("light.{}", "x".repeat(MAX_IDENTIFIER_CHARS)),
                "on",
                BTreeMap::new(),
            ),
            CoreError::IdentifierTooLong,
        ),
    ];

    for (update, expected_error) in invalid_updates {
        assert_eq!(core.apply_state(update), Err(expected_error));
        assert_eq!(
            core.dashboard(),
            &previous,
            "a rejected state update must not partially alter the tile"
        );
    }
}

#[test]
fn snapshot_limits_and_registry_identity_errors_are_enforced() {
    let too_many_areas = (0..=MAX_AREA_COUNT)
        .map(|index| area(&format!("area-{index}"), "Area"))
        .collect::<Vec<_>>();
    assert_eq!(
        build_dashboard(
            &too_many_areas,
            &[],
            &[],
            &[],
            &DashboardOrder::default(),
            ConnectionStatus::Offline,
        ),
        Err(CoreError::TooManyAreas)
    );

    let too_many_devices = (0..=MAX_DEVICE_COUNT)
        .map(|index| device(&format!("device-{index}"), None, None))
        .collect::<Vec<_>>();
    assert_eq!(
        build_dashboard(
            &[],
            &too_many_devices,
            &[],
            &[],
            &DashboardOrder::default(),
            ConnectionStatus::Offline,
        ),
        Err(CoreError::TooManyDevices)
    );

    let too_many_entities = (0..=MAX_ENTITY_REGISTRY_COUNT)
        .map(|index| entity(&format!("sensor.value_{index}"), None, None))
        .collect::<Vec<_>>();
    assert_eq!(
        build_dashboard(
            &[],
            &[],
            &too_many_entities,
            &[],
            &DashboardOrder::default(),
            ConnectionStatus::Offline,
        ),
        Err(CoreError::TooManyEntityRegistryEntries)
    );

    let too_many_states = (0..=MAX_STATE_COUNT)
        .map(|index| state(&format!("sensor.value_{index}"), "1", BTreeMap::new()))
        .collect::<Vec<_>>();
    assert_eq!(
        build_dashboard(
            &[],
            &[],
            &[],
            &too_many_states,
            &DashboardOrder::default(),
            ConnectionStatus::Offline,
        ),
        Err(CoreError::TooManyStates)
    );

    let too_many_tiles = (0..=MAX_ENTITY_COUNT)
        .map(|index| entity(&format!("sensor.value_{index}"), None, None))
        .collect::<Vec<_>>();
    let matching_states = (0..=MAX_ENTITY_COUNT)
        .map(|index| state(&format!("sensor.value_{index}"), "1", BTreeMap::new()))
        .collect::<Vec<_>>();
    assert_eq!(
        build_dashboard(
            &[],
            &[],
            &too_many_tiles,
            &matching_states,
            &DashboardOrder::default(),
            ConnectionStatus::Offline,
        ),
        Err(CoreError::TooManyEligibleEntities)
    );
}

#[test]
fn duplicate_registry_identities_and_malformed_entity_ids_are_rejected() {
    assert_eq!(
        build_dashboard(
            &[],
            &[device("same", None, None), device("same", None, None)],
            &[],
            &[],
            &DashboardOrder::default(),
            ConnectionStatus::Offline,
        ),
        Err(CoreError::DuplicateDeviceId)
    );
    assert_eq!(
        build_dashboard(
            &[],
            &[],
            &[
                entity("sensor.same", None, None),
                entity("sensor.same", None, None)
            ],
            &[],
            &DashboardOrder::default(),
            ConnectionStatus::Offline,
        ),
        Err(CoreError::DuplicateEntityId)
    );
    assert_eq!(
        build_dashboard(
            &[],
            &[],
            &[],
            &[
                state("sensor.same", "1", BTreeMap::new()),
                state("sensor.same", "2", BTreeMap::new()),
            ],
            &DashboardOrder::default(),
            ConnectionStatus::Offline,
        ),
        Err(CoreError::DuplicateEntityId)
    );
    assert_eq!(
        build_dashboard(
            &[],
            &[],
            &[entity("light.lamp.extra", None, None)],
            &[],
            &DashboardOrder::default(),
            ConnectionStatus::Offline,
        ),
        Err(CoreError::InvalidEntityId)
    );
}

#[test]
fn explicit_order_deduplicates_removed_ids_and_keeps_unassigned_last() {
    let order = DashboardOrder {
        area_ids: Some(vec![
            "room".to_owned(),
            "room".to_owned(),
            "deleted-room".to_owned(),
        ]),
        entity_ids_by_room: BTreeMap::from([
            (
                RoomId::Area("room".to_owned()),
                vec![
                    "light.second".to_owned(),
                    "light.second".to_owned(),
                    "deleted-entity".to_owned(),
                ],
            ),
            (RoomId::Unassigned, vec!["sensor.free".to_owned()]),
        ]),
    };
    let dashboard = build_dashboard(
        &[area("room", "Room")],
        &[],
        &[
            entity("light.first", Some("room"), None),
            entity("light.second", Some("room"), None),
            entity("sensor.free", None, None),
        ],
        &states(&[
            ("light.first", "off"),
            ("light.second", "off"),
            ("sensor.free", "21"),
        ]),
        &order,
        ConnectionStatus::Ready,
    )
    .expect("stale and repeated order entries should be reconciled");

    assert_eq!(dashboard.rooms.len(), 2);
    assert_eq!(dashboard.rooms[0].id, RoomId::Area("room".to_owned()));
    assert_eq!(
        dashboard.rooms[0]
            .entities
            .iter()
            .map(|tile| tile.entity_id.as_str())
            .collect::<Vec<_>>(),
        ["light.second", "light.first"]
    );
    assert_eq!(dashboard.rooms[1].id, RoomId::Unassigned);
    assert_eq!(dashboard.rooms[1].entities[0].entity_id, "sensor.free");
}
