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
                    field(message_key, "Message", "Nachricht", true, german),
                    preannounce,
                ];
                if key == "start_conversation" {
                    inputs.push(field(
                        "extra_system_prompt",
                        "Additional instructions",
                        "Zusätzliche Anweisungen",
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
                    inputs.push(field("state", "Context", "Kontext", false, german));
                }
                if feature(tile, 2) {
                    inputs.push(field(
                        "attachment_id",
                        "Attachment media ID",
                        "Medien-ID des Anhangs",
                        false,
                        german,
                    ));
                    inputs.push(field(
                        "attachment_type",
                        "Attachment media type",
                        "Medientyp des Anhangs",
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
                    data.insert(
                        "attachments".into(),
                        json!([{"media_content_id":id,"media_content_type":kind}]),
                    );
                }
                (None, None) => {}
                _ => return Err(ActionError::InvalidValue),
            }
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
            data.insert("entity_id".into(), tile.entity_id.clone().into());
        }
        D::AssistSatellite => {}
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
