//! Shared Slint control mapping; also compiled by the P4 frontend.

#![allow(clippy::redundant_pub_crate)] // The P4 frontend calls this shared module from its sibling main module.

use super::{ControlItem, InputItem, MainWindow, ResultItem};
use njorun_app_core::controls::{self, ControlKind, ControlSpec, InputKind};
use njorun_app_core::{DetailAction, EntityTile};
use serde_json::{Map, Value};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

fn value_text(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => String::new(),
        _ => value.to_string(),
    }
}

fn strings(values: Vec<String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        values.into_iter().map(Into::into).collect::<Vec<_>>(),
    ))
}

#[allow(clippy::cast_possible_truncation)]
fn control(spec: ControlSpec, available: bool) -> ControlItem {
    ControlItem {
        key: spec.key.into(),
        label: spec.label.into(),
        kind: match spec.kind {
            ControlKind::Button => "button",
            ControlKind::Slider => "slider",
            ControlKind::Select => "select",
            ControlKind::Toggle => "toggle",
            ControlKind::Form => "form",
        }
        .into(),
        value: value_text(&spec.value).into(),
        options: strings(spec.options),
        minimum: spec.minimum as f32,
        maximum: spec.maximum as f32,
        step: spec.step as f32,
        unit: spec.unit.into(),
        enabled: available && spec.enabled,
        dangerous: spec.dangerous,
        icon_key: spec.icon.into(),
        group: spec.group.into(),
        inputs: ModelRc::new(VecModel::from(
            spec.inputs
                .into_iter()
                .map(|field| InputItem {
                    key: field.key.into(),
                    label: field.label.into(),
                    kind: match field.kind {
                        InputKind::Text => "text",
                        InputKind::Password => "password",
                        InputKind::Number => "number",
                        InputKind::Select => "select",
                        InputKind::Toggle => "toggle",
                        InputKind::Entity => "entity",
                    }
                    .into(),
                    value: value_text(&field.value).into(),
                    options: strings(field.options),
                    required: field.required,
                })
                .collect::<Vec<_>>(),
        )),
    }
}

pub(crate) fn render(ui: &MainWindow, tile: Option<&EntityTile>, german: bool) {
    ui.set_use_entity_controls(tile.is_some());
    ui.set_media_can_browse(tile.is_some_and(|tile| {
        tile.domain == njorun_app_core::EntityDomain::MediaPlayer
            && controls::feature(tile, 131_072)
    }));
    let enabled = tile.is_some_and(|tile| {
        tile.command_enabled
            && !tile.is_stale
            && (tile.is_available()
                || matches!(
                    tile.domain,
                    njorun_app_core::EntityDomain::Sensor
                        | njorun_app_core::EntityDomain::BinarySensor
                ))
            && !matches!(
                tile.action_feedback,
                Some(
                    njorun_app_core::ActionFeedback::AwaitingAcknowledgement { .. }
                        | njorun_app_core::ActionFeedback::AcceptedAwaitingState { .. }
                        | njorun_app_core::ActionFeedback::AcceptedAwaitingConfirmation { .. }
                )
            )
    });
    ui.set_controls_available(enabled);
    ui.set_media_can_play(tile.is_some_and(|tile| {
        tile.domain == njorun_app_core::EntityDomain::MediaPlayer && controls::feature(tile, 512)
    }));
    if !ui.get_editing_controls() {
        let specs = tile
            .map(|tile| controls::for_entity(tile, german))
            .unwrap_or_default();
        let (mut transport, other): (Vec<_>, Vec<_>) = specs
            .into_iter()
            .partition(|spec| spec.group == "transport");
        transport.sort_by_key(|spec| match spec.key {
            "media_previous" => 0,
            "media_play" | "media_pause" => 1,
            "media_next" => 2,
            "media_stop" => 3,
            _ => 4,
        });
        ui.set_transport_controls(ModelRc::new(VecModel::from(
            transport
                .into_iter()
                .map(|spec| control(spec, enabled))
                .collect::<Vec<_>>(),
        )));
        let (seek, other): (Vec<_>, Vec<_>) =
            other.into_iter().partition(|spec| spec.key == "media_seek");
        ui.set_seek_controls(ModelRc::new(VecModel::from(
            seek.into_iter()
                .map(|spec| control(spec, enabled))
                .collect::<Vec<_>>(),
        )));
        ui.set_entity_controls(ModelRc::new(VecModel::from(
            other
                .into_iter()
                .map(|spec| control(spec, enabled))
                .collect::<Vec<_>>(),
        )));
    }
    render_media(ui, tile);
}

pub(crate) fn render_media(ui: &MainWindow, tile: Option<&EntityTile>) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let media = tile
        .filter(|tile| tile.domain == njorun_app_core::EntityDomain::MediaPlayer)
        .map(|tile| njorun_app_core::media::view(tile, now));
    #[allow(clippy::cast_possible_truncation)]
    let position = media
        .as_ref()
        .and_then(|view| view.elapsed_seconds)
        .unwrap_or(0.0) as f32;
    ui.set_media_position(position);
    ui.set_media_title(
        media
            .as_ref()
            .and_then(|view| view.title.as_deref())
            .unwrap_or("")
            .into(),
    );
    ui.set_media_artist(
        media
            .as_ref()
            .and_then(|view| view.artist.as_deref())
            .unwrap_or("")
            .into(),
    );
    ui.set_media_album(
        media
            .as_ref()
            .and_then(|view| view.album.as_deref())
            .unwrap_or("")
            .into(),
    );
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let format_time = |seconds: f64| {
        let seconds = seconds.max(0.0).floor() as u64;
        format!("{}:{:02}", seconds / 60, seconds % 60)
    };
    ui.set_media_timing(
        media
            .as_ref()
            .and_then(|view| {
                view.elapsed_seconds.map(|elapsed| {
                    view.duration_seconds.map_or_else(
                        || format_time(elapsed),
                        |duration| format!("{} / {}", format_time(elapsed), format_time(duration)),
                    )
                })
            })
            .unwrap_or_default()
            .into(),
    );
}

pub(crate) fn action(tile: &EntityTile, key: &str, argument: &str) -> Option<DetailAction> {
    if let Some(key) = key.strip_prefix("entity:") {
        let spec = controls::for_entity(tile, false)
            .into_iter()
            .find(|spec| spec.key == key)?;
        let argument = if spec.kind == ControlKind::Select {
            Value::String(argument.into())
        } else if argument.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(argument).unwrap_or_else(|_| Value::String(argument.into()))
        };
        Some(DetailAction::Entity {
            key: key.into(),
            argument,
        })
    } else {
        DetailAction::from_ui(key, argument, tile.domain)
    }
}

pub(crate) fn form_argument(fields: &ModelRc<InputItem>) -> Result<Value, &'static str> {
    let mut values = Map::new();
    for field in fields.iter() {
        if field.value.trim().is_empty() {
            if field.required {
                return Err("Complete the required fields");
            }
            continue;
        }
        let value = match field.kind.as_str() {
            "number" => {
                let number = field
                    .value
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite())
                    .ok_or("Enter a valid number")?;
                Value::from(number)
            }
            "toggle" => Value::Bool(field.value == "true"),
            _ => Value::String(field.value.to_string()),
        };
        values.insert(field.key.to_string(), value);
    }
    Ok(Value::Object(values))
}

pub(crate) fn virtual_key(ui: &MainWindow, value: &str) {
    use slint::platform::{Key, WindowEvent};
    let text: SharedString = match value {
        "backspace" => Key::Backspace.into(),
        "left" => Key::LeftArrow.into(),
        "right" => Key::RightArrow.into(),
        "submit" => Key::Return.into(),
        _ => value.into(),
    };
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
}

/// Render bounded, human-readable responses from HA queries on either frontend.
pub(crate) fn response(ui: &MainWindow, value: &Value) {
    let value = value.get("response").unwrap_or(value);
    let mut rows = Vec::new();
    response_rows(value, "", &mut rows, 0);
    for row in &mut rows {
        if row.action == "media_play_item" && !ui.get_media_can_play() {
            row.action = SharedString::default();
        }
        if row.secondary_action == "media_play_item" && !ui.get_media_can_play() {
            row.secondary_action = SharedString::default();
        }
    }
    ui.set_entity_error("".into());
    ui.set_entity_result_message(
        if rows.is_empty() {
            if ui.get_german() {
                "Keine Ergebnisse"
            } else {
                "No results"
            }
        } else if rows.len() >= 200 {
            if ui.get_german() {
                "Die ersten 200 Ergebnisse"
            } else {
                "First 200 results"
            }
        } else {
            ""
        }
        .into(),
    );
    ui.set_entity_results(ModelRc::new(VecModel::from(rows)));
    ui.set_showing_results(true);
}

#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)] // Bounded recursive projection of HA response variants and timestamps.
fn response_rows(value: &Value, label: &str, rows: &mut Vec<ResultItem>, depth: usize) {
    if rows.len() >= 200 || depth > 8 {
        return;
    }
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        push_text_rows(text, label, rows);
        return;
    }
    if let Some(children) = value.get("children").and_then(Value::as_array) {
        if let Some(title) = value.get("title").and_then(Value::as_str) {
            rows.push(ResultItem {
                title: title.into(),
                ..Default::default()
            });
        }
        for child in children {
            response_rows(child, "", rows, depth + 1);
        }
        return;
    }
    if let Some(state) = value.get("s").and_then(Value::as_str) {
        let timestamp = value
            .get("lu")
            .or_else(|| value.get("lc"))
            .and_then(Value::as_f64)
            .map(|value| njorun_app_core::presentation::unix_timestamp(value as i64))
            .unwrap_or_default();
        rows.push(ResultItem {
            title: state.into(),
            subtitle: timestamp.into(),
            ..Default::default()
        });
        return;
    }
    if let Some(id) = value.get("media_content_id").and_then(Value::as_str) {
        let media_type = value
            .get("media_content_type")
            .and_then(Value::as_str)
            .unwrap_or("");
        let title = value.get("title").and_then(Value::as_str).unwrap_or(id);
        let argument =
            serde_json::json!({"media_content_id":id,"media_content_type":media_type}).to_string();
        let expand = value.get("can_expand").and_then(Value::as_bool) == Some(true);
        let play = value.get("can_play").and_then(Value::as_bool) == Some(true);
        rows.push(ResultItem {
            title: title.into(),
            subtitle: media_type.replace('_', " ").into(),
            action: if expand {
                "media_browse"
            } else if play {
                "media_play_item"
            } else {
                ""
            }
            .into(),
            argument: argument.clone().into(),
            secondary_action: if expand && play {
                "media_play_item"
            } else {
                ""
            }
            .into(),
            secondary_argument: argument.into(),
        });
        return;
    }
    match value {
        Value::Array(items) => {
            if items.first().is_some_and(|item| item.get("s").is_some()) {
                for item in items.iter().rev().take(200) {
                    response_rows(item, label, rows, depth + 1);
                }
            } else {
                for item in items.iter().take(200) {
                    response_rows(item, label, rows, depth + 1);
                }
            }
        }
        Value::Object(object) => {
            // Calendar, to-do, forecasts and conversation responses have a concise title and scalar fields.
            let title = ["summary", "title", "name", "datetime", "text"]
                .iter()
                .find_map(|key| object.get(*key).and_then(Value::as_str));
            if let Some(title) = title {
                let subtitle = object
                    .iter()
                    .filter(|(key, v)| {
                        !matches!(key.as_str(), "summary" | "title" | "name" | "text")
                            && (!v.is_object()
                                || v.get("dateTime").is_some()
                                || v.get("date").is_some())
                            && !v.is_array()
                            && !v.is_null()
                    })
                    .map(|(key, v)| {
                        let value = v.get("dateTime").or_else(|| v.get("date")).unwrap_or(v);
                        format!("{}: {}", key.replace('_', " "), value_text(value))
                    })
                    .collect::<Vec<_>>()
                    .join(" · ");
                rows.push(ResultItem {
                    title: title.chars().take(500).collect::<String>().into(),
                    subtitle: subtitle.chars().take(1000).collect::<String>().into(),
                    ..Default::default()
                });
            } else {
                for (key, value) in object {
                    response_rows(value, &key.replace('_', " "), rows, depth + 1);
                }
            }
        }
        Value::String(text) => push_text_rows(text, label, rows),
        Value::Null => {}
        scalar => rows.push(ResultItem {
            title: label.into(),
            subtitle: value_text(scalar)
                .chars()
                .take(1000)
                .collect::<String>()
                .into(),
            ..Default::default()
        }),
    }
}

fn push_text_rows(text: &str, label: &str, rows: &mut Vec<ResultItem>) {
    let mut characters = text.chars().peekable();
    let mut first = true;
    while characters.peek().is_some() && rows.len() < 200 {
        rows.push(ResultItem {
            title: if first {
                label.into()
            } else {
                SharedString::default()
            },
            subtitle: characters.by_ref().take(800).collect::<String>().into(),
            ..Default::default()
        });
        first = false;
    }
}
