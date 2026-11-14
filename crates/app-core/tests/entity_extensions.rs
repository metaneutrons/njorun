use njorun_app_core::{
    ActionError, DetailAction, EntityDomain, EntityTile, HaEntityState, controls,
};
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

    let ai_with_schema = tile(EntityDomain::AiTask, 1 | 2);
    let (_, call) = controls::plan(
        &ai_with_schema,
        "generate_data",
        &json!({
            "task_name":"structured",
            "instructions":"Extract",
            "structure_json":"{\"name\":{\"required\":true}}",
            "attachment_id":"media-source://a;media-source://b",
            "attachment_type":"image/jpeg;image/png"
        }),
    )
    .unwrap();
    assert_eq!(call.data["structure"]["name"]["required"], true);
    assert_eq!(call.data["attachments"].as_array().unwrap().len(), 2);
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
    let (_, call) = controls::plan(
        &satellite,
        "ask_question",
        &json!({
            "question_media_id":"media-source://media_source/local/question.mp3",
            "answers_json":"[{\"id\":\"yes\",\"sentences\":[\"yes\",\"sure\"]}]",
            "preannounce":false
        }),
    )
    .unwrap();
    assert_eq!(call.data["answers"][0]["id"], "yes");
    let ai = tile(EntityDomain::AiTask, 8);
    let mut input = json!({"task_name":"choice","instructions":"Pick one","question_type":"choice","criteria":"a"});
    assert!(controls::plan(&ai, "evaluate", &input).is_err());
    input["criteria"] = "a;b".into();
    input["state_json"] = "{\"delivered\":true}".into();
    let (_, call) = controls::plan(&ai, "evaluate", &input).unwrap();
    assert_eq!(
        call.data["questions"]["answer"]["criteria"],
        json!({"a":"a","b":"b"})
    );
    assert_eq!(call.data["state"], json!({"delivered":true}));

    let announcer = tile(EntityDomain::AssistSatellite, 1);
    let (_, call) = controls::plan(
        &announcer,
        "announce",
        &json!({"media_id":"media-source://media_source/local/chime.mp3","preannounce":true}),
    )
    .unwrap();
    assert!(call.data.get("message").is_none());

    let mut stale = tile(EntityDomain::Conversation, 0);
    stale.is_stale = true;
    assert_eq!(controls::for_entity(&stale, false), Vec::new());
    assert_eq!(
        controls::plan(&stale, "process", &json!({"text":"hello"})),
        Err(ActionError::Unavailable)
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
