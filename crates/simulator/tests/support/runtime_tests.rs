use super::*;
use njorun_app_core::RoomId;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc,
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct PrivateDirectory(PathBuf);
impl PrivateDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "njorun-runtime-{}-{nonce}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("test directory");
        Self(path)
    }
}
impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn snapshot() -> Snapshot {
    Snapshot {
        areas: vec![],
        devices: vec![],
        entities: vec![],
        states: BTreeMap::new(),
        state_subscription: 1,
        state_order: vec![],
        tombstones: BTreeMap::new(),
    }
}
fn config(revision: u64, settings: &Settings) -> Value {
    json!({"type":"configuration","revision":revision,"operation_id":"test",
        "expires_at":9_999_999_999_u64,"settings":settings})
}

#[test]
fn configuration_commit_persists_before_notifying_ui() {
    let directory = PrivateDirectory::new();
    let path = directory.0.join("settings.json");
    let mut applied = AppliedSettings::default();
    let mut core = AppCore::new();
    let settings = Settings {
        brightness: 25,
        ..Settings::default()
    };
    let (tx, rx) = mpsc::sync_channel(4);
    apply_configuration(
        &config(1, &settings),
        &mut applied,
        &mut core,
        &snapshot(),
        &path,
        &tx,
    )
    .expect("apply");
    let saved: AppliedSettings = storage::read(&path).expect("durable settings");
    assert_eq!(saved, applied);
    assert_eq!(saved.settings.brightness, 25);
    assert!(matches!(
        rx.try_recv().expect("dashboard event"),
        RuntimeEvent::Dashboard(_)
    ));
    let RuntimeEvent::Settings(event) = rx.try_recv().expect("UI settings event") else {
        panic!("settings must follow the dashboard before allowing its ACK")
    };
    assert_eq!(*event, saved);
}

#[test]
fn invalid_operation_identity_cannot_mutate_configuration() {
    let directory = PrivateDirectory::new();
    let path = directory.0.join("settings.json");
    let original = AppliedSettings::default();
    storage::write(&path, &original).expect("initial settings");
    for id in [
        Value::Null,
        json!(12),
        json!(""),
        json!("x".repeat(129)),
        json!("bad\nidentifier"),
    ] {
        let mut applied = original.clone();
        let mut core = AppCore::new();
        let dashboard = core.dashboard().clone();
        let mut envelope = config(1, &Settings::default());
        envelope["operation_id"] = id;
        let (tx, rx) = mpsc::sync_channel(4);
        assert_eq!(
            apply_configuration(&envelope, &mut applied, &mut core, &snapshot(), &path, &tx),
            Err(ClientError::Protocol)
        );
        assert_eq!(applied, original);
        assert_eq!(core.dashboard(), &dashboard);
        assert_eq!(
            storage::read::<AppliedSettings>(&path).expect("unchanged file"),
            original
        );
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn rejected_snapshot_cannot_poison_persisted_configuration() {
    let directory = PrivateDirectory::new();
    let path = directory.0.join("settings.json");
    let original = AppliedSettings {
        revision: 1,
        settings: Settings::default(),
    };
    storage::write(&path, &original).expect("initial settings");
    let mut applied = original.clone();
    let mut core = AppCore::new();
    let dashboard = core.dashboard().clone();
    let mut invalid_snapshot = snapshot();
    invalid_snapshot.areas = vec![
        json!({"area_id":"room","name":"One"}),
        json!({"area_id":"room","name":"Two"}),
    ];
    let changed = Settings {
        brightness: 25,
        ..Settings::default()
    };
    let (tx, rx) = mpsc::sync_channel(4);
    assert!(
        apply_configuration(
            &config(2, &changed),
            &mut applied,
            &mut core,
            &invalid_snapshot,
            &path,
            &tx
        )
        .is_err()
    );
    assert_eq!(applied, original);
    assert_eq!(
        storage::read::<AppliedSettings>(&path).expect("previous settings"),
        original
    );
    assert_eq!(core.dashboard(), &dashboard);
    assert!(rx.try_recv().is_err());
}

#[test]
fn expired_settings_and_storage_failure_emit_no_success() {
    let directory = PrivateDirectory::new();
    let mut applied = AppliedSettings::default();
    let original = applied.clone();
    let mut core = AppCore::new();
    let (tx, rx) = mpsc::sync_channel(4);
    let mut expired = config(1, &Settings::default());
    expired["expires_at"] = 0.into();
    assert_eq!(
        apply_configuration(
            &expired,
            &mut applied,
            &mut core,
            &snapshot(),
            &directory.0,
            &tx
        ),
        Err(ClientError::Protocol)
    );
    // A directory cannot be atomically replaced by the settings file.
    assert_eq!(
        apply_configuration(
            &config(1, &Settings::default()),
            &mut applied,
            &mut core,
            &snapshot(),
            &directory.0,
            &tx
        ),
        Err(ClientError::Storage)
    );
    assert_eq!(applied, original);
    assert!(rx.try_recv().is_err());
}

#[test]
fn last_daylight_survives_restart_and_unknown_sun_state() {
    let directory = PrivateDirectory::new();
    let mut files = StateFiles::new(&directory.0);
    let (tx, rx) = mpsc::sync_channel(4);
    let mut snapshot = snapshot();
    snapshot
        .states
        .insert("sun.sun".into(), json!({"state":"below_horizon"}));
    files.update_daylight(&snapshot, &tx).expect("night");
    assert!(matches!(
        rx.try_recv().expect("night event"),
        RuntimeEvent::Daylight(false)
    ));
    let mut restarted = StateFiles::new(&directory.0);
    assert_eq!(restarted.last_daylight, Some(false));
    snapshot
        .states
        .insert("sun.sun".into(), json!({"state":"unavailable"}));
    restarted
        .update_daylight(&snapshot, &tx)
        .expect("unknown state");
    assert_eq!(restarted.last_daylight, Some(false));
    assert!(rx.try_recv().is_err());
    snapshot
        .states
        .insert("sun.sun".into(), json!({"state":"above_horizon"}));
    restarted.update_daylight(&snapshot, &tx).expect("sunrise");
    assert!(matches!(
        rx.try_recv().expect("day event"),
        RuntimeEvent::Daylight(true)
    ));
    assert_eq!(StateFiles::new(&directory.0).last_daylight, Some(true));
}

#[test]
fn uncertain_commit_reconciles_memory_but_requires_durable_retry_before_ack() {
    let directory = PrivateDirectory::new();
    let path = directory.0.join("settings.json");
    let mut applied = AppliedSettings::default();
    let mut core = AppCore::new();
    let desired = config(
        1,
        &Settings {
            brightness: 25,
            ..Settings::default()
        },
    );
    let (tx, rx) = mpsc::sync_channel(4);
    let result = apply_configuration_with_writer(
        &desired,
        &mut applied,
        &mut core,
        &snapshot(),
        &path,
        &tx,
        |path, candidate| {
            storage::write(path, candidate)?;
            Err(ClientError::StorageCommitUncertain)
        },
    );
    assert_eq!(result, Err(ClientError::StorageCommitUncertain));
    assert_eq!(applied.revision, 1);
    assert_eq!(applied.settings.brightness, 25);
    assert_eq!(
        storage::read::<AppliedSettings>(&path).expect("replacement"),
        applied
    );
    assert!(rx.try_recv().is_err());
    apply_configuration(&desired, &mut applied, &mut core, &snapshot(), &path, &tx)
        .expect("same revision can retry durability");
    assert!(matches!(
        rx.try_recv().expect("dashboard"),
        RuntimeEvent::Dashboard(_)
    ));
    assert!(matches!(
        rx.try_recv().expect("ack eligible"),
        RuntimeEvent::Settings(_)
    ));
}

#[test]
fn invalid_persisted_settings_never_reach_ui_or_network_on_restart() {
    let directory = PrivateDirectory::new();
    let options = RuntimeOptions {
        credentials: directory.0.join("credentials.json"),
        state_directory: directory.0.join("state"),
    };
    let credentials = Credentials {
        ha_url: "https://unused.invalid".into(),
        access_token: "test".into(),
        refresh_token: None,
        client_id: None,
        allow_http: false,
        tablet_id: "test".into(),
        binding_id: "test".into(),
        tablet_private_key: "unused".into(),
        ha_public_key: "unused".into(),
        capabilities: None,
    };
    storage::write(&options.credentials, &credentials).expect("private credentials");
    let path = options.state_directory.join("settings.json");
    let invalid = AppliedSettings {
        revision: 1,
        settings: Settings {
            brightness: 101,
            ..Settings::default()
        },
    };
    storage::write(&path, &invalid).expect("corrupt fixture");
    let (_command_tx, command_rx) = mpsc::sync_channel(1);
    let (event_tx, event_rx) = mpsc::sync_channel(4);
    assert_eq!(
        run(&options, &command_rx, &event_tx),
        Err(ClientError::Storage)
    );
    assert!(event_rx.try_recv().is_err());
    assert_eq!(
        storage::read::<AppliedSettings>(&path).expect("preserved for recovery"),
        invalid
    );
}

fn populated_snapshot() -> Snapshot {
    let mut data = snapshot();
    data.areas = vec![
        json!({"area_id":"living","name":"Living"}),
        json!({"area_id":"kitchen","name":"Kitchen"}),
    ];
    for (id, area) in [
        ("light.a", Some("living")),
        ("light.b", Some("living")),
        ("light.c", Some("living")),
        ("light.d", Some("kitchen")),
        ("light.e", None),
        ("light.f", None),
    ] {
        data.entities.push(json!({"entity_id":id,"area_id":area}));
        data.states.insert(
            id.into(),
            json!({"entity_id":id,"state":"on","attributes":{},
            "last_updated":"2026-11-05T00:00:00+00:00"}),
        );
        data.state_order.push(id.into());
    }
    data
}

#[test]
fn runtime_applies_panel_exclusions_and_manual_orders_without_changing_ha_snapshot() {
    let data = populated_snapshot();
    let settings = Settings {
        excluded_entities: vec!["light.a".into()],
        room_order: vec!["kitchen".into(), "living".into()],
        entity_order: BTreeMap::from([
            ("living".into(), vec!["light.c".into(), "light.b".into()]),
            (
                "unassigned".into(),
                vec!["light.f".into(), "light.e".into()],
            ),
        ]),
        ..Settings::default()
    };
    let mut customized = AppCore::new();
    rebuild(&mut customized, &data, &settings).expect("customized panel");
    let rooms = &customized.dashboard().rooms;
    assert_eq!(rooms[0].id, RoomId::Area("kitchen".into()));
    assert_eq!(
        rooms[1]
            .entities
            .iter()
            .map(|tile| tile.entity_id.as_str())
            .collect::<Vec<_>>(),
        vec!["light.c", "light.b"]
    );
    assert_eq!(rooms[2].id, RoomId::Unassigned);
    assert_eq!(
        rooms[2]
            .entities
            .iter()
            .map(|tile| tile.entity_id.as_str())
            .collect::<Vec<_>>(),
        vec!["light.f", "light.e"]
    );
    assert!(customized.dashboard().entity("light.a").is_none());
    let mut other_panel = AppCore::new();
    rebuild(&mut other_panel, &data, &Settings::default()).expect("other panel");
    assert_eq!(
        other_panel.dashboard().rooms[0].id,
        RoomId::Area("living".into())
    );
    assert!(other_panel.dashboard().entity("light.a").is_some());
    assert!(data.entities[0]["hidden_by"].is_null());
}

#[test]
fn action_deadline_reports_unconfirmed_once_without_expiring_other_actions() {
    let mut core = AppCore::new();
    rebuild(&mut core, &populated_snapshot(), &Settings::default()).expect("dashboard");
    core.set_connection(ConnectionStatus::Ready);
    core.primary_tap("light.b").expect("first action");
    core.acknowledge("light.b", ActionAcknowledgement::Accepted)
        .expect("HA accepted first action");
    core.primary_tap("light.c").expect("other action");
    let now = Instant::now();
    let mut deadlines = BTreeMap::from([
        ("light.b".into(), now),
        ("light.c".into(), now + Duration::from_secs(30)),
        ("light.removed".into(), now),
    ]);
    let (tx, rx) = mpsc::sync_channel(4);
    expire_actions(&mut deadlines, &mut core, &tx).expect("expire elapsed deadlines");
    assert_eq!(
        deadlines.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["light.c"]
    );
    let RuntimeEvent::Dashboard(dashboard) = rx.try_recv().expect("visible timeout") else {
        panic!("expected dashboard feedback");
    };
    assert!(matches!(
        dashboard
            .entity("light.b")
            .expect("first light")
            .action_feedback,
        Some(njorun_app_core::ActionFeedback::Unconfirmed { .. })
    ));
    assert!(matches!(
        dashboard
            .entity("light.c")
            .expect("other light")
            .action_feedback,
        Some(njorun_app_core::ActionFeedback::AwaitingAcknowledgement { .. })
    ));
    expire_actions(&mut deadlines, &mut core, &tx).expect("do not repeat timeout");
    assert!(rx.try_recv().is_err());
}

#[test]
fn expanded_snapshot_preserves_favorites_and_registry_exclusions_at_capacity() {
    let mut snapshot = snapshot();
    for index in 0..514 {
        let entity_id = format!("sensor.sample_{index:04}");
        snapshot.states.insert(
            entity_id.clone(),
            json!({"entity_id":entity_id,"state":"21","attributes":{"unit_of_measurement":"°C"}}),
        );
    }
    snapshot
        .entities
        .push(json!({"entity_id":"sensor.sample_0000","hidden_by":"user"}));
    snapshot.state_order = snapshot.states.keys().cloned().collect();
    let settings = Settings {
        favorites: vec!["sensor.sample_0513".into()],
        excluded_entities: vec!["sensor.sample_0001".into()],
        ..Settings::default()
    };
    let mut core = AppCore::new();
    assert_eq!(rebuild(&mut core, &snapshot, &settings), Ok(0));
    assert!(core.dashboard().entity("sensor.sample_0000").is_none());
    assert!(core.dashboard().entity("sensor.sample_0001").is_none());
    assert!(core.dashboard().entity("sensor.sample_0513").is_some());
    snapshot.states.insert(
        "sensor.extra".into(),
        json!({"entity_id":"sensor.extra","state":"2","attributes":{}}),
    );
    snapshot.state_order.push("sensor.extra".into());
    assert_eq!(rebuild(&mut core, &snapshot, &settings), Ok(1));
    assert!(core.dashboard().entity("sensor.sample_0513").is_some());
}

#[test]
fn asynchronous_query_response_is_delivered_once_and_old_ids_are_ignored() {
    let mut snapshot = snapshot();
    snapshot.states.insert(
        "sensor.temperature".into(),
        json!({"entity_id":"sensor.temperature","state":"21","attributes":{}}),
    );
    snapshot.state_order = snapshot.states.keys().cloned().collect();
    let mut core = AppCore::new();
    rebuild(&mut core, &snapshot, &Settings::default()).unwrap();
    core.set_connection(ConnectionStatus::Ready);
    core.request_action(
        "sensor.temperature",
        &DetailAction::Entity {
            key: "history".into(),
            argument: Value::Null,
        },
    )
    .unwrap();
    let mut pending =
        PendingCalls::from([(7, ("sensor.temperature".into(), true, Instant::now()))]);
    let mut deadlines = BTreeMap::new();
    let (tx, rx) = mpsc::sync_channel(8);
    let result = json!({"id":7,"type":"result","success":true,"result":{"sensor.temperature":[{"s":"21","lu":1_794_650_000}]}});
    receive_action_result(&result, &mut pending, &mut deadlines, &mut core, &tx).unwrap();
    assert!(
        matches!(rx.try_recv().unwrap(), RuntimeEvent::ActionResult { entity_id, value } if entity_id == "sensor.temperature" && value["sensor.temperature"][0]["s"] == "21")
    );
    assert!(matches!(rx.try_recv().unwrap(), RuntimeEvent::Dashboard(_)));
    receive_action_result(&result, &mut pending, &mut deadlines, &mut core, &tx).unwrap();
    assert!(rx.try_recv().is_err());
    assert!(pending.is_empty());
}

#[test]
fn old_confirmation_deadline_cannot_expire_a_new_unacknowledged_request() {
    let mut snapshot = snapshot();
    snapshot.states.insert(
        "sensor.temperature".into(),
        json!({"entity_id":"sensor.temperature","state":"21","attributes":{}}),
    );
    snapshot.state_order.push("sensor.temperature".into());
    let mut core = AppCore::new();
    rebuild(&mut core, &snapshot, &Settings::default()).unwrap();
    core.set_connection(ConnectionStatus::Ready);
    let action = DetailAction::Entity {
        key: "history".into(),
        argument: Value::Null,
    };
    core.request_action("sensor.temperature", &action).unwrap();
    core.acknowledge("sensor.temperature", ActionAcknowledgement::Accepted)
        .unwrap();
    core.request_action("sensor.temperature", &action).unwrap();
    let mut deadlines = BTreeMap::from([(
        "sensor.temperature".into(),
        Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("one second before now"),
    )]);
    let (tx, rx) = mpsc::sync_channel(4);
    expire_actions(&mut deadlines, &mut core, &tx).unwrap();
    assert!(matches!(
        core.dashboard()
            .entity("sensor.temperature")
            .unwrap()
            .action_feedback,
        Some(njorun_app_core::ActionFeedback::AwaitingAcknowledgement { .. })
    ));
    assert!(rx.try_recv().is_err());
    core.acknowledge("sensor.temperature", ActionAcknowledgement::Accepted)
        .unwrap();
}
