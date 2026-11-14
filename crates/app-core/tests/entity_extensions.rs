use njorun_app_core::{DetailAction, EntityDomain, EntityTile, HaEntityState, controls};
use serde_json::json;

fn tile(domain: EntityDomain, bits: u64) -> EntityTile {
    let id = format!("{}.test", domain.service_domain());
    EntityTile {
        entity_id: id.clone(),
        name: "Test".into(),
        icon: None,
        domain,
        state: Some(HaEntityState {
            entity_id: id,
            state: "idle".into(),
            attributes: serde_json::from_value(json!({"supported_features":bits})).unwrap(),
            last_updated: None,
        }),
        command_enabled: true,
        is_stale: false,
        action_feedback: None,
    }
}

#[test]
fn provider_forms_obey_features_and_target_the_selected_agent() {
    assert_eq!(
        controls::for_entity(&tile(EntityDomain::AiTask, 0), false).len(),
        0
    );
    let ai = tile(EntityDomain::AiTask, 1);
    let (_, call) = controls::plan(
        &ai,
        "generate_data",
        &json!({"task_name":"daily","instructions":"Summarize"}),
    )
    .unwrap();
    let payload = call.payload(&ai.entity_id);
    assert_eq!(payload["service_data"]["entity_id"], ai.entity_id);
    assert_eq!(payload["return_response"], true);
    assert!(payload.get("target").is_none());
    assert!(
        controls::plan(
            &ai,
            "generate_image",
            &json!({"task_name":"daily","instructions":"Draw"})
        )
        .is_err()
    );
    let conversation = tile(EntityDomain::Conversation, 0);
    let (_, call) = controls::plan(&conversation, "process", &json!({"text":"hello"})).unwrap();
    let payload = call.payload(&conversation.entity_id);
    assert_eq!(payload["service_data"]["agent_id"], conversation.entity_id);
    assert!(payload.get("target").is_none());
    assert!(
        controls::plan(
            &conversation,
            "process",
            &json!({"text":"hello","agent_id":"conversation.other"})
        )
        .is_err()
    );
}

#[test]
fn satellite_questions_return_answers_and_evaluations_validate_criteria() {
    let satellite = tile(EntityDomain::AssistSatellite, 2);
    assert!(controls::plan(&satellite, "announce", &json!({"message":"test"})).is_err());
    let (_, call) =
        controls::plan(&satellite, "ask_question", &json!({"question":"Ready?"})).unwrap();
    assert_eq!(
        call.payload(&satellite.entity_id)["service_data"]["entity_id"],
        satellite.entity_id
    );
    assert!(call.response_required());
    let ai = tile(EntityDomain::AiTask, 8);
    let mut input = json!({"task_name":"choice","instructions":"Pick one","question_type":"choice","criteria":"a"});
    assert!(controls::plan(&ai, "evaluate", &input).is_err());
    input["criteria"] = "a;b".into();
    let (_, call) = controls::plan(&ai, "evaluate", &input).unwrap();
    assert_eq!(
        call.data["questions"]["answer"]["criteria"],
        json!({"a":"a","b":"b"})
    );
}

#[test]
fn history_is_a_bounded_read_query_not_a_fabricated_service() {
    let sensor = tile(EntityDomain::Sensor, 0);
    let (_, call) = controls::plan(&sensor, "history", &serde_json::Value::Null).unwrap();
    let payload = call.payload(&sensor.entity_id);
    assert_eq!(payload["type"], "history/history_during_period");
    assert_eq!(payload["entity_ids"], json!([sensor.entity_id]));
    assert_eq!(payload["no_attributes"], true);
    assert!(payload.get("target").is_none());
    assert!(call.response_required());
    assert!(DetailAction::from_ui("entity:history", "null", EntityDomain::Sensor).is_some());
}
