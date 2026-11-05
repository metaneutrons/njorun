use njorun_ha_client::{ClientError, snapshot::Snapshot};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn state(value: &str, second: u8) -> Value {
    json!({"entity_id":"light.one","state":value,"attributes":{},
           "last_updated":format!("2026-11-05T00:00:{second:02}+00:00")})
}
fn snapshot() -> Snapshot {
    Snapshot {
        areas: vec![],
        devices: vec![],
        entities: vec![],
        states: BTreeMap::from([("light.one".into(), state("on", 10))]),
        state_subscription: 3,
        tombstones: BTreeMap::new(),
        state_order: vec!["light.one".into()],
    }
}
fn event(state: &Value) -> Value {
    json!({"type":"event","id":3,"event":{"event_type":"state_changed",
        "time_fired":"2026-11-05T00:00:11+00:00",
        "data":{"entity_id":"light.one","new_state":state}}})
}

#[test]
fn buffered_old_events_cannot_overwrite_newer_snapshot() {
    let mut cache = snapshot();
    assert!(
        !cache
            .apply_event(&event(&state("off", 9)))
            .expect("old event")
    );
    assert!(
        !cache
            .apply_event(&event(&state("off", 10)))
            .expect("duplicate version")
    );
    assert_eq!(cache.states["light.one"]["state"], "on");
    assert!(
        cache
            .apply_event(&event(&state("off", 11)))
            .expect("new event")
    );
    assert_eq!(cache.ordered_states()[0]["state"], "off");
}

#[test]
fn stale_deletion_does_not_remove_newer_snapshot_state() {
    let mut cache = snapshot();
    let mut deletion = event(&Value::Null);
    deletion["event"]["time_fired"] = "2026-11-05T00:00:09+00:00".into();
    assert!(!cache.apply_event(&deletion).expect("stale deletion"));
    assert!(
        cache
            .apply_event(&event(&Value::Null))
            .expect("current deletion")
    );
    assert_eq!(cache.ordered_states(), Vec::<Value>::new());
}

#[test]
fn malformed_identity_and_timestamp_are_rejected_without_mutation() {
    let mut cache = snapshot();
    let mut invalid = state("off", 11);
    invalid["last_updated"] = "not-a-time".into();
    assert_eq!(
        cache.apply_event(&event(&invalid)),
        Err(ClientError::Protocol)
    );
    let mut invalid = state("off", 11);
    invalid["entity_id"] = "light.other".into();
    assert_eq!(
        cache.apply_event(&event(&invalid)),
        Err(ClientError::Protocol)
    );
    assert_eq!(cache.states["light.one"]["state"], "on");
    let mut unrelated = event(&state("off", 11));
    unrelated["id"] = 5.into();
    assert!(!cache.apply_event(&unrelated).expect("other subscription"));
}

#[test]
fn deletion_survives_cache_reload_and_rejects_delayed_resurrection() {
    let mut cache = snapshot();
    assert!(cache.apply_event(&event(&Value::Null)).expect("delete"));
    let serialized = serde_json::to_vec(&cache).expect("persist");
    let mut cache: Snapshot = serde_json::from_slice(&serialized).expect("reload");
    cache.state_subscription = 3;
    for second in [9, 10, 11] {
        assert!(
            !cache
                .apply_event(&event(&state("on", second)))
                .expect("delayed event")
        );
        assert!(cache.states.is_empty());
    }
    assert!(
        cache
            .apply_event(&event(&state("off", 12)))
            .expect("recreated")
    );
    assert_eq!(cache.states["light.one"]["state"], "off");
    assert!(cache.tombstones.is_empty());
}

#[test]
fn deleting_an_absent_entity_marks_the_cache_dirty() {
    let mut cache = snapshot();
    cache.states.clear();
    cache.state_order.clear();
    let deletion = event(&Value::Null);
    assert!(cache.apply_event(&deletion).expect("new deletion marker"));
    assert!(!cache.apply_event(&deletion).expect("duplicate deletion"));
    let encoded = serde_json::to_vec(&cache).expect("persist dirty cache");
    let mut restored: Snapshot = serde_json::from_slice(&encoded).expect("reload");
    assert!(
        !restored
            .apply_event(&event(&state("on", 10)))
            .expect("stale event")
    );
    assert!(restored.states.is_empty());
}
