//! Provider operations that use response-producing HA services, rather than toggles.

use crate::controls::{ControlKind, ControlSpec, InputKind, InputSpec, feature, text};
use crate::{ActionError, ActionKind, EntityDomain as D, EntityTile, ServiceCall};
use serde_json::{Map, Value, json};

#[must_use]
pub const fn handles(domain: D) -> bool {
    matches!(
        domain,
        D::AiTask
            | D::AssistSatellite
            | D::Conversation
            | D::Stt
            | D::WakeWord
            | D::Infrared
            | D::RadioFrequency
    )
}

fn field(key: &'static str, en: &str, de: &str, required: bool, german: bool) -> InputSpec {
    InputSpec::text(key, &text(german, en, de), required)
}

fn form(
    key: &'static str,
    en: &str,
    de: &str,
    inputs: Vec<InputSpec>,
    german: bool,
) -> ControlSpec {
    let mut control = ControlSpec::button(key, &text(german, en, de));
    control.kind = ControlKind::Form;
    control.inputs = inputs;
    control
}

#[must_use]
#[allow(clippy::too_many_lines)] // Each provider keeps its capability-gated form together.
pub fn controls(tile: &EntityTile, german: bool) -> Vec<ControlSpec> {
    if tile.is_stale || !tile.command_enabled || !tile.is_available() {
        return Vec::new();
    }
    let mut controls = Vec::new();
    match tile.domain {
        D::Conversation => controls.push(form(
            "process",
            "Send to assistant",
            "Assistent fragen",
            vec![
                field("text", "Message", "Nachricht", true, german),
                field(
                    "language",
                    "Language (optional)",
                    "Sprache (optional)",
                    false,
                    german,
                ),
                field(
                    "conversation_id",
                    "Continue conversation (optional ID)",
                    "Gespräch fortsetzen (optionale ID)",
                    false,
                    german,
                ),
            ],
            german,
        )),
        D::AssistSatellite => {
            for (bit, key, en, de, message_key) in [
                (1, "announce", "Announcement", "Durchsage", "message"),
                (
                    2,
                    "start_conversation",
                    "Start conversation",
                    "Gespräch starten",
                    "start_message",
                ),
                (
                    2,
                    "ask_question",
                    "Ask a question",
                    "Frage stellen",
                    "question",
                ),
            ] {
                if !feature(tile, bit) {
                    continue;
                }
                let mut preannounce = field(
                    "preannounce",
                    "Announcement sound",
                    "Hinweiston",
                    false,
                    german,
                );
                preannounce.kind = InputKind::Toggle;
                preannounce.value = Value::Bool(true);
                let mut inputs = vec![
                    field(
                        message_key,
                        "Message (optional)",
                        "Nachricht (optional)",
                        false,
                        german,
                    ),
                    preannounce,
                ];
                let media_key = match key {
                    "announce" => "media_id",
                    "start_conversation" => "start_media_id",
                    _ => "question_media_id",
                };
                inputs.push(field(
                    media_key,
                    "Media ID (optional)",
                    "Medien-ID (optional)",
                    false,
                    german,
                ));
                inputs.push(field(
                    "preannounce_media_id",
                    "Preannouncement media ID (optional)",
                    "Medien-ID des Hinweistons (optional)",
                    false,
                    german,
                ));
                if key == "start_conversation" {
                    inputs.push(field(
                        "extra_system_prompt",
                        "Additional instructions",
                        "Zusätzliche Anweisungen",
                        false,
                        german,
                    ));
                } else if key == "ask_question" {
                    inputs.push(field(
                        "answers_json",
                        "Answers as JSON (optional)",
                        "Antworten als JSON (optional)",
                        false,
                        german,
                    ));
                }
                controls.push(form(key, en, de, inputs, german));
            }
        }
        D::AiTask => {
            for (bit, key, en, de) in [
                (1, "generate_data", "Generate answer", "Antwort erzeugen"),
                (4, "generate_image", "Generate image", "Bild erzeugen"),
                (8, "evaluate", "Evaluate a question", "Frage bewerten"),
            ] {
                if !feature(tile, bit) {
                    continue;
                }
                let mut inputs = vec![
                    field("task_name", "Task name", "Aufgabenname", true, german),
                    field("instructions", "Instructions", "Anweisungen", true, german),
                ];
                if key == "generate_data" {
                    inputs.push(field(
                        "structure_json",
                        "Output structure as JSON (optional)",
                        "Ausgabestruktur als JSON (optional)",
                        false,
                        german,
                    ));
                }
                if key == "evaluate" {
                    let mut kind =
                        field("question_type", "Answer type", "Antworttyp", true, german);
                    kind.kind = InputKind::Select;
                    kind.options = vec!["noul".into(), "choice".into(), "score".into()];
                    kind.value = "noul".into();
                    inputs.push(kind);
                    inputs.push(field(
                        "criteria",
                        "Alternatives or ordered levels, separated by ;",
                        "Alternativen oder geordnete Stufen, getrennt durch ;",
                        false,
                        german,
                    ));
                    inputs.push(field(
                        "state_json",
                        "State as JSON (optional)",
                        "Zustand als JSON (optional)",
                        false,
                        german,
                    ));
                }
                if feature(tile, 2) {
                    inputs.push(field(
                        "attachment_id",
                        "Attachment media IDs, separated by ;",
                        "Medien-IDs der Anhänge, getrennt durch ;",
                        false,
                        german,
                    ));
                    inputs.push(field(
                        "attachment_type",
                        "Attachment media types, separated by ;",
                        "Medientypen der Anhänge, getrennt durch ;",
                        false,
                        german,
                    ));
                }
                controls.push(form(key, en, de, inputs, german));
            }
        }
        _ => {}
    }
    controls
}

fn json_object_field(
    data: &mut Map<String, Value>,
    source: &str,
    target: &str,
) -> Result<(), ActionError> {
    let Some(value) = data.remove(source) else {
        return Ok(());
    };
    let text = value.as_str().ok_or(ActionError::InvalidValue)?;
    if text.is_empty() {
        return Ok(());
    }
    let parsed: Value = serde_json::from_str(text).map_err(|_| ActionError::InvalidValue)?;
    if !parsed.is_object() {
        return Err(ActionError::InvalidValue);
    }
    data.insert(target.to_owned(), parsed);
    Ok(())
}

fn attachment_list(ids: &str, kinds: &str) -> Result<Value, ActionError> {
    let ids = ids.split(';').map(str::trim).collect::<Vec<_>>();
    let kinds = kinds.split(';').map(str::trim).collect::<Vec<_>>();
    if ids.len() != kinds.len()
        || ids.is_empty()
        || ids.len() > 8
        || ids
            .iter()
            .chain(&kinds)
            .any(|value| value.is_empty() || value.len() > 1024)
    {
        return Err(ActionError::InvalidValue);
    }
    Ok(Value::Array(
        ids.into_iter()
            .zip(kinds)
            .map(|(id, kind)| json!({"media_content_id":id,"media_content_type":kind}))
            .collect(),
    ))
}

fn answers_field(data: &mut Map<String, Value>) -> Result<(), ActionError> {
    let Some(value) = data.remove("answers_json") else {
        return Ok(());
    };
    let text = value.as_str().ok_or(ActionError::InvalidValue)?;
    if text.is_empty() {
        return Ok(());
    }
    let answers: Value = serde_json::from_str(text).map_err(|_| ActionError::InvalidValue)?;
    let values = answers.as_array().ok_or(ActionError::InvalidValue)?;
    if values.is_empty() || values.len() > 32 {
        return Err(ActionError::InvalidValue);
    }
    for answer in values {
        let answer = answer.as_object().ok_or(ActionError::InvalidValue)?;
        if answer.len() != 2
            || answer
                .get("id")
                .and_then(Value::as_str)
                .is_none_or(|id| id.is_empty() || id.len() > 255)
        {
            return Err(ActionError::InvalidValue);
        }
        let sentences = answer
            .get("sentences")
            .and_then(Value::as_array)
            .ok_or(ActionError::InvalidValue)?;
        if sentences.is_empty()
            || sentences.len() > 32
            || sentences.iter().any(|sentence| {
                sentence
                    .as_str()
                    .is_none_or(|sentence| sentence.is_empty() || sentence.len() > 255)
            })
        {
            return Err(ActionError::InvalidValue);
        }
    }
    data.insert("answers".into(), answers);
    Ok(())
}

fn remove_empty_strings(data: &mut Map<String, Value>, keys: &[&str]) {
    for key in keys {
        if data
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(str::is_empty)
        {
            data.remove(*key);
        }
    }
}

/// Validate the selected provider operation and its typed fields.
///
/// # Errors
/// Rejects unadvertised operations, invalid fields and unsupported input values.
#[allow(clippy::too_many_lines)] // Validate typed input before constructing provider-specific payloads.
pub fn plan(
    tile: &EntityTile,
    key: &str,
    argument: &Value,
) -> Result<(ActionKind, ServiceCall), ActionError> {
    if tile.is_stale || !tile.command_enabled || !tile.is_available() {
        return Err(ActionError::Unavailable);
    }
    let control = controls(tile, false)
        .into_iter()
        .find(|control| control.key == key)
        .ok_or(ActionError::Unsupported)?;
    let input = argument.as_object().ok_or(ActionError::InvalidValue)?;
    if input
        .keys()
        .any(|key| !control.inputs.iter().any(|field| field.key == key))
    {
        return Err(ActionError::InvalidValue);
    }
    let mut data = Map::new();
    for field in &control.inputs {
        let Some(value) = input.get(field.key) else {
            if field.required {
                return Err(ActionError::InvalidValue);
            }
            continue;
        };
        if field.kind == InputKind::Toggle {
            if !value.is_boolean() {
                return Err(ActionError::InvalidValue);
            }
        } else {
            let text = value.as_str().ok_or(ActionError::InvalidValue)?;
            if text.len() > 8192
                || text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
                || (field.required && text.trim().is_empty())
            {
                return Err(ActionError::InvalidValue);
            }
            if field.kind == InputKind::Select && !field.options.iter().any(|option| option == text)
            {
                return Err(ActionError::InvalidValue);
            }
        }
        data.insert(field.key.into(), value.clone());
    }
    let service = control.key;
    match tile.domain {
        D::Conversation => {
            data.insert("agent_id".into(), tile.entity_id.clone().into());
        }
        D::AiTask => {
            data.insert("entity_id".into(), tile.entity_id.clone().into());
            match (data.remove("attachment_id"), data.remove("attachment_type")) {
                (Some(id), Some(kind)) => {
                    let id = id.as_str().ok_or(ActionError::InvalidValue)?;
                    let kind = kind.as_str().ok_or(ActionError::InvalidValue)?;
                    if !id.is_empty() || !kind.is_empty() {
                        data.insert("attachments".into(), attachment_list(id, kind)?);
                    }
                }
                (None, None) => {}
                _ => return Err(ActionError::InvalidValue),
            }
            json_object_field(&mut data, "structure_json", "structure")?;
            json_object_field(&mut data, "state_json", "state")?;
            if service == "evaluate" {
                let kind = data
                    .remove("question_type")
                    .unwrap_or_else(|| json!("noul"));
                let instructions = data
                    .remove("instructions")
                    .ok_or(ActionError::InvalidValue)?;
                let criteria = data.remove("criteria");
                let mut question = json!({"type":kind,"instructions":instructions});
                if kind != "noul" {
                    let choices: Vec<_> = criteria
                        .as_ref()
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .split(';')
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .collect();
                    if !(2..=32).contains(&choices.len()) {
                        return Err(ActionError::InvalidValue);
                    }
                    question["criteria"] = if kind == "choice" {
                        Value::Object(
                            choices
                                .into_iter()
                                .map(|item| (item.to_owned(), Value::String(item.to_owned())))
                                .collect(),
                        )
                    } else {
                        json!(choices)
                    };
                }
                data.insert("questions".into(), json!({"answer":question}));
            }
        }
        D::AssistSatellite if service == "ask_question" => {
            answers_field(&mut data)?;
            remove_empty_strings(
                &mut data,
                &["question", "question_media_id", "preannounce_media_id"],
            );
            data.insert("entity_id".into(), tile.entity_id.clone().into());
        }
        D::AssistSatellite => {
            let message_key = if service == "announce" {
                "message"
            } else {
                "start_message"
            };
            let media_key = if service == "announce" {
                "media_id"
            } else {
                "start_media_id"
            };
            remove_empty_strings(&mut data, &[message_key, media_key, "preannounce_media_id"]);
            let message_present = data
                .get(message_key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty());
            let media_present = data
                .get(media_key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty());
            if !message_present && !media_present {
                return Err(ActionError::InvalidValue);
            }
            if !message_present {
                data.remove(message_key);
            }
            if !media_present {
                data.remove(media_key);
            }
        }
        _ => return Err(ActionError::Unsupported),
    }
    Ok((
        ActionKind::RunScene,
        ServiceCall::new(tile.domain.service_domain(), service, Value::Object(data)),
    ))
}

/// These are infrastructure providers, configured and consumed by HA integrations.
#[must_use]
pub fn explanation(domain: D, german: bool) -> String {
    match domain {
        D::Stt | D::WakeWord => text(
            german,
            "This voice provider is used by the Assist pipeline configured in Home Assistant. Start a conversation on a configured Assist satellite.",
            "Dieser Sprachdienst wird von der in Home Assistant konfigurierten Assist-Pipeline verwendet. Gespräche werden an einem konfigurierten Assist-Satelliten gestartet.",
        ),
        D::Infrared | D::RadioFrequency => text(
            german,
            "This transmitter or receiver is used by Home Assistant device integrations. Configure device commands in HA and expose them as remote controls or scripts on the panel.",
            "Dieser Sender oder Empfänger wird von Geräteintegrationen in Home Assistant verwendet. Gerätebefehle in HA konfigurieren und als Fernbedienung oder Skript auf dem Panel bereitstellen.",
        ),
        _ => String::new(),
    }
}
