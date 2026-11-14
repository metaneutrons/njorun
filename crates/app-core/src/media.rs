//! Capability-aware controls and actions for Home Assistant media players.
//!
//! The feature bits mirror `MediaPlayerEntityFeature` in Home Assistant Core. Attribute
//! lists supply values for a capability; they do not decide whether the capability exists.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::controls::{
    ControlKind, ControlSpec, InputKind, InputSpec, attribute, feature, options, text,
};
use crate::{ActionError, ActionKind, EntityDomain, EntityTile, PlaybackTarget, ServiceCall};

// Values from homeassistant.components.media_player.const.MediaPlayerEntityFeature.
const PAUSE: u64 = 1;
const SEEK: u64 = 2;
const VOLUME_SET: u64 = 4;
const VOLUME_MUTE: u64 = 8;
const PREVIOUS_TRACK: u64 = 16;
const NEXT_TRACK: u64 = 32;
const TURN_ON: u64 = 128;
const TURN_OFF: u64 = 256;
const PLAY_MEDIA: u64 = 512;
const SELECT_SOURCE: u64 = 2048;
const STOP: u64 = 4096;
const CLEAR_PLAYLIST: u64 = 8192;
const PLAY: u64 = 16384;
const SHUFFLE_SET: u64 = 32768;
const SELECT_SOUND_MODE: u64 = 65536;
const BROWSE_MEDIA: u64 = 131_072;
const REPEAT_SET: u64 = 262_144;
const GROUPING: u64 = 524_288;
const MEDIA_ANNOUNCE: u64 = 1_048_576;
const MEDIA_ENQUEUE: u64 = 2_097_152;
const SEARCH_MEDIA: u64 = 4_194_304;

const REPEAT_MODES: [&str; 3] = ["off", "one", "all"];
const ENQUEUE_MODES: [&str; 4] = ["add", "next", "play", "replace"];
const MEDIA_CLASSES: &[&str] = &[
    "album",
    "app",
    "artist",
    "channel",
    "composer",
    "contributing_artist",
    "directory",
    "episode",
    "game",
    "genre",
    "image",
    "movie",
    "music",
    "playlist",
    "podcast",
    "season",
    "track",
    "tv_show",
    "url",
    "video",
];
const MAX_TEXT_CHARS: usize = 255;
const MAX_ENTITY_ID_CHARS: usize = 255;
const MAX_GROUP_MEMBERS: usize = 32;
const MAX_MEDIA_SECONDS: f64 = 2_678_400.0; // 31 days, for streams with no reported duration.

/// Media metadata for a tile. `position_seconds` is the reported position;
/// `elapsed_seconds` advances it while the player is playing.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaView {
    pub position_seconds: Option<f64>,
    pub elapsed_seconds: Option<f64>,
    pub duration_seconds: Option<f64>,
    pub progress: Option<f64>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub source: Option<String>,
    pub artwork_url: Option<String>,
}

/// Builds the presentation controls advertised by the player's feature bits.
#[must_use]
// Keep the feature-to-control table together for capability auditing.
#[allow(clippy::too_many_lines)]
pub fn controls(tile: &EntityTile, german: bool) -> Vec<ControlSpec> {
    if tile.domain != EntityDomain::MediaPlayer || !tile.is_available() {
        return Vec::new();
    }

    let state = tile.state.as_ref().map_or("", |state| state.state.as_str());
    let mut controls = Vec::new();

    if state == "off" && feature(tile, TURN_ON) {
        controls.push(button(
            "media_power_on",
            text(german, "Turn on", "Einschalten"),
            "power",
            "transport",
        ));
    }

    if state != "off" {
        match state {
            "playing" | "buffering" if feature(tile, PAUSE) => controls.push(button(
                "media_pause",
                text(german, "Pause", "Pause"),
                "pause",
                "transport",
            )),
            "paused" | "idle" | "on" | "standby" if feature(tile, PLAY) => controls.push(button(
                "media_play",
                text(german, "Play", "Wiedergabe"),
                "play",
                "transport",
            )),
            _ => {}
        }

        if feature(tile, STOP) && matches!(state, "playing" | "paused" | "buffering") {
            controls.push(button(
                "media_stop",
                text(german, "Stop", "Stopp"),
                "stop",
                "transport",
            ));
        }
        if feature(tile, PREVIOUS_TRACK) {
            controls.push(button(
                "media_previous",
                text(german, "Previous", "Zurück"),
                "previous",
                "transport",
            ));
        }
        if feature(tile, NEXT_TRACK) {
            controls.push(button(
                "media_next",
                text(german, "Next", "Weiter"),
                "next",
                "transport",
            ));
        }
        if feature(tile, TURN_OFF) {
            controls.push(button(
                "media_power_off",
                text(german, "Turn off", "Ausschalten"),
                "power",
                "transport",
            ));
        }
    }

    if feature(tile, VOLUME_SET) {
        let mut control =
            ControlSpec::button("media_volume", &text(german, "Volume", "Lautstärke"));
        control.kind = ControlKind::Slider;
        control.value = json!(
            attribute(tile, "volume_level")
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite())
                .map_or(0.0, |value| (value.clamp(0.0, 1.0) * 100.0).round())
        );
        control.minimum = 0.0;
        control.maximum = 100.0;
        control.step = 1.0;
        "%".clone_into(&mut control.unit);
        control.icon = "volume";
        control.group = "volume";
        controls.push(control);
    }
    if feature(tile, VOLUME_MUTE) {
        let mut control = button(
            "media_mute",
            text(german, "Mute", "Stumm"),
            "mute",
            "volume",
        );
        control.kind = ControlKind::Toggle;
        control.value = json!(
            attribute(tile, "is_volume_muted")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        );
        controls.push(control);
    }
    if feature(tile, SEEK) {
        let media = view(tile, 0.0);
        let max = media.duration_seconds.unwrap_or(3_600.0).max(1.0);
        let mut control = ControlSpec::button("media_seek", &text(german, "Position", "Position"));
        control.kind = ControlKind::Slider;
        control.value = json!(media.elapsed_seconds.unwrap_or(0.0));
        control.minimum = 0.0;
        control.maximum = max;
        control.step = 1.0;
        "s".clone_into(&mut control.unit);
        control.icon = "seek";
        control.group = "playback";
        control.enabled = state != "off";
        controls.push(control);
    }
    if feature(tile, SELECT_SOURCE) {
        let available = options(tile, "source_list");
        let current = attribute(tile, "source")
            .and_then(Value::as_str)
            .unwrap_or_default();
        controls.push(select_control(
            "media_source",
            text(german, "Source", "Quelle"),
            available,
            current,
            "playback",
        ));
    }
    if feature(tile, SELECT_SOUND_MODE) {
        let available = options(tile, "sound_mode_list");
        let current = attribute(tile, "sound_mode")
            .and_then(Value::as_str)
            .unwrap_or_default();
        controls.push(select_control(
            "media_sound_mode",
            text(german, "Sound mode", "Klangmodus"),
            available,
            current,
            "playback",
        ));
    }
    if feature(tile, SHUFFLE_SET) {
        let mut control = button(
            "media_shuffle",
            text(german, "Shuffle", "Zufallswiedergabe"),
            "shuffle",
            "playback",
        );
        control.kind = ControlKind::Toggle;
        control.value = json!(
            attribute(tile, "shuffle")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        );
        controls.push(control);
    }
    if feature(tile, REPEAT_SET) {
        let current = attribute(tile, "repeat")
            .and_then(Value::as_str)
            .filter(|value| REPEAT_MODES.contains(value))
            .unwrap_or("off");
        controls.push(select_control(
            "media_repeat",
            text(german, "Repeat", "Wiederholen"),
            REPEAT_MODES
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            current,
            "playback",
        ));
    }

    if feature(tile, BROWSE_MEDIA) {
        controls.push(button(
            "media_browse",
            text(german, "Browse media", "Medien durchsuchen"),
            "browse",
            "library",
        ));
    }
    if feature(tile, SEARCH_MEDIA) {
        controls.push(form(
            "media_search",
            text(german, "Search media", "Medien suchen"),
            "search",
            "library",
            vec![
                input(
                    "search_query",
                    text(german, "Search term", "Suchbegriff"),
                    InputKind::Text,
                    Value::String(String::new()),
                    Vec::new(),
                    true,
                ),
                input(
                    "media_content_type",
                    text(german, "Content type", "Inhaltstyp"),
                    InputKind::Text,
                    Value::String(String::new()),
                    Vec::new(),
                    false,
                ),
                input(
                    "media_content_id",
                    text(german, "Content ID", "Inhalts-ID"),
                    InputKind::Text,
                    Value::String(String::new()),
                    Vec::new(),
                    false,
                ),
            ],
        ));
    }
    if feature(tile, PLAY_MEDIA) {
        let mut inputs = vec![
            input(
                "media_content_type",
                text(german, "Content type", "Inhaltstyp"),
                InputKind::Text,
                Value::String(String::new()),
                Vec::new(),
                true,
            ),
            input(
                "media_content_id",
                text(german, "Content ID", "Inhalts-ID"),
                InputKind::Text,
                Value::String(String::new()),
                Vec::new(),
                true,
            ),
        ];
        if feature(tile, MEDIA_ENQUEUE) {
            inputs.push(input(
                "enqueue",
                text(german, "Queue", "Warteschlange"),
                InputKind::Select,
                Value::Null,
                ENQUEUE_MODES
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
                false,
            ));
        }
        if feature(tile, MEDIA_ANNOUNCE) {
            inputs.push(input(
                "announce",
                text(german, "Announcement", "Durchsage"),
                InputKind::Toggle,
                Value::Null,
                Vec::new(),
                false,
            ));
        }
        controls.push(form(
            "media_play_item",
            text(german, "Play media", "Medium abspielen"),
            "play",
            "library",
            inputs,
        ));
    }
    if feature(tile, CLEAR_PLAYLIST) {
        controls.push(button(
            "media_clear_queue",
            text(german, "Clear queue", "Warteschlange leeren"),
            "clear",
            "playback",
        ));
    }
    if feature(tile, GROUPING) {
        controls.push(form(
            "media_join",
            text(german, "Join players", "Player verbinden"),
            "group",
            "group",
            vec![input(
                "group_members",
                text(german, "Players", "Player"),
                InputKind::Entity,
                Value::String(String::new()),
                Vec::new(),
                true,
            )],
        ));
        controls.push(button(
            "media_unjoin",
            text(german, "Leave group", "Gruppe verlassen"),
            "ungroup",
            "group",
        ));
    }

    controls
}

/// Validates an action against the entity's current media-player capabilities.
///
/// Arguments are constrained to Home Assistant's media-player service fields. Arbitrary
/// service names or data cannot be supplied by the caller.
///
/// # Errors
/// Returns an error when the tile is unavailable, the capability is missing, or the argument
/// does not match the service's validated fields.
// Keep the action-to-service mapping together for capability auditing.
#[allow(clippy::too_many_lines)]
pub fn plan(
    tile: &EntityTile,
    key: &str,
    argument: &Value,
) -> Result<(ActionKind, ServiceCall), ActionError> {
    if tile.domain != EntityDomain::MediaPlayer {
        return Err(ActionError::Unsupported);
    }
    ensure_available(tile)?;

    match key {
        "media_play" => {
            no_arguments(argument)?;
            require_feature(tile, PLAY)?;
            if tile
                .state
                .as_ref()
                .is_some_and(|state| state.state == "off")
            {
                return Err(ActionError::Unsupported);
            }
            Ok((
                ActionKind::Playback(PlaybackTarget::Playing),
                service("media_play", json!({})),
            ))
        }
        "media_pause" => {
            no_arguments(argument)?;
            require_feature(tile, PAUSE)?;
            if !matches!(state(tile), "playing" | "buffering") {
                return Err(ActionError::Unsupported);
            }
            Ok((
                ActionKind::Playback(PlaybackTarget::Paused),
                service("media_pause", json!({})),
            ))
        }
        "media_stop" => {
            no_arguments(argument)?;
            require_feature(tile, STOP)?;
            if !matches!(state(tile), "playing" | "paused" | "buffering") {
                return Err(ActionError::Unsupported);
            }
            Ok((ActionKind::RunScene, service("media_stop", json!({}))))
        }
        "media_next" | "media_previous" => {
            no_arguments(argument)?;
            if state(tile) == "off" {
                return Err(ActionError::Unsupported);
            }
            let (bit, name) = if key == "media_next" {
                (NEXT_TRACK, "media_next_track")
            } else {
                (PREVIOUS_TRACK, "media_previous_track")
            };
            require_feature(tile, bit)?;
            Ok((ActionKind::RunScene, service(name, json!({}))))
        }
        "media_power_on" => {
            no_arguments(argument)?;
            require_feature(tile, TURN_ON)?;
            if state(tile) != "off" {
                return Err(ActionError::Unsupported);
            }
            Ok((ActionKind::RunScene, service("turn_on", json!({}))))
        }
        "media_power_off" => {
            no_arguments(argument)?;
            require_feature(tile, TURN_OFF)?;
            if state(tile) == "off" {
                return Err(ActionError::Unsupported);
            }
            Ok((
                ActionKind::Observe {
                    state: Some("off".to_owned()),
                    attributes: BTreeMap::new(),
                },
                service("turn_off", json!({})),
            ))
        }
        "media_volume" => {
            require_feature(tile, VOLUME_SET)?;
            let value = finite_number(argument)?;
            if !(0.0..=100.0).contains(&value) {
                return Err(ActionError::InvalidValue);
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let percent = value.round() as u8;
            Ok((
                ActionKind::SetVolume(percent),
                service("volume_set", json!({"volume_level": value / 100.0})),
            ))
        }
        "media_mute" => {
            require_feature(tile, VOLUME_MUTE)?;
            let muted = argument.as_bool().ok_or(ActionError::InvalidValue)?;
            Ok((
                observed("is_volume_muted", json!(muted)),
                service("volume_mute", json!({"is_volume_muted": muted})),
            ))
        }
        "media_seek" => {
            require_feature(tile, SEEK)?;
            if state(tile) == "off" {
                return Err(ActionError::Unsupported);
            }
            let value = finite_number(argument)?;
            let max = view(tile, 0.0)
                .duration_seconds
                .unwrap_or(MAX_MEDIA_SECONDS)
                .min(MAX_MEDIA_SECONDS);
            if !(0.0..=max).contains(&value) {
                return Err(ActionError::InvalidValue);
            }
            Ok((
                ActionKind::RunScene,
                service("media_seek", json!({"seek_position": value})),
            ))
        }
        "media_source" => {
            require_feature(tile, SELECT_SOURCE)?;
            let source = selected_option(tile, argument, "source_list")?;
            Ok((
                observed("source", json!(source)),
                service("select_source", json!({"source": source})),
            ))
        }
        "media_sound_mode" => {
            require_feature(tile, SELECT_SOUND_MODE)?;
            let mode = selected_option(tile, argument, "sound_mode_list")?;
            Ok((
                observed("sound_mode", json!(mode)),
                service("select_sound_mode", json!({"sound_mode": mode})),
            ))
        }
        "media_shuffle" => {
            require_feature(tile, SHUFFLE_SET)?;
            let shuffle = argument.as_bool().ok_or(ActionError::InvalidValue)?;
            Ok((
                observed("shuffle", json!(shuffle)),
                service("shuffle_set", json!({"shuffle": shuffle})),
            ))
        }
        "media_repeat" => {
            require_feature(tile, REPEAT_SET)?;
            let repeat = argument.as_str().ok_or(ActionError::InvalidValue)?;
            if !REPEAT_MODES.contains(&repeat) {
                return Err(ActionError::InvalidValue);
            }
            Ok((
                observed("repeat", json!(repeat)),
                service("repeat_set", json!({"repeat": repeat})),
            ))
        }
        "media_browse" => {
            require_feature(tile, BROWSE_MEDIA)?;
            let data = content_pair(argument, false)?;
            Ok((ActionKind::RunScene, response_service("browse_media", data)))
        }
        "media_search" => {
            require_feature(tile, SEARCH_MEDIA)?;
            let data = search_data(argument)?;
            Ok((ActionKind::RunScene, response_service("search_media", data)))
        }
        "media_play_item" => {
            require_feature(tile, PLAY_MEDIA)?;
            let data = play_media_data(tile, argument)?;
            let enqueue = data.get("enqueue").and_then(Value::as_str);
            let action = if matches!(enqueue, Some("add" | "next")) {
                ActionKind::RunScene
            } else {
                ActionKind::Playback(PlaybackTarget::Playing)
            };
            Ok((action, service("play_media", Value::Object(data))))
        }
        "media_clear_queue" => {
            no_arguments(argument)?;
            require_feature(tile, CLEAR_PLAYLIST)?;
            Ok((ActionKind::RunScene, service("clear_playlist", json!({}))))
        }
        "media_join" => {
            require_feature(tile, GROUPING)?;
            let members = group_members(tile, argument)?;
            Ok((
                ActionKind::RunScene,
                service("join", json!({"group_members": members})),
            ))
        }
        "media_unjoin" => {
            no_arguments(argument)?;
            require_feature(tile, GROUPING)?;
            Ok((ActionKind::RunScene, service("unjoin", json!({}))))
        }
        _ => Err(ActionError::Unsupported),
    }
}

/// Builds metadata and a wall-clock-adjusted playback position from HA attributes.
#[must_use]
pub fn view(tile: &EntityTile, now_unix: f64) -> MediaView {
    let duration = finite_attribute(tile, "media_duration")
        .filter(|value| *value >= 0.0)
        .map(|value| value.min(MAX_MEDIA_SECONDS));
    let raw_position = finite_attribute(tile, "media_position")
        .filter(|value| *value >= 0.0)
        .map(|value| value.min(MAX_MEDIA_SECONDS));
    let reported = raw_position.map(|position| clamp_position(position, duration));

    let elapsed = if tile
        .state
        .as_ref()
        .is_some_and(|state| state.state == "playing")
        && now_unix.is_finite()
        && let (Some(position), Some(updated_at)) = (
            reported,
            attribute(tile, "media_position_updated_at").and_then(Value::as_str),
        )
        && let Some(updated_unix) = parse_rfc3339(updated_at)
    {
        let delta = (now_unix - updated_unix).max(0.0);
        Some(clamp_position(position + delta, duration))
    } else {
        reported
    };

    let media_title = attribute_string(tile, "media_title");
    let media_artist = attribute_string(tile, "media_artist");
    let media_album = attribute_string(tile, "media_album_name");
    let media_source = attribute_string(tile, "source");
    // HA exposes the current media artwork as entity_picture. `media_image_url` is accepted
    // for integrations that publish the same HA-owned URL under that attribute.
    let artwork_url = attribute_string(tile, "entity_picture")
        .or_else(|| attribute_string(tile, "media_image_url"));
    let progress = elapsed.zip(duration).and_then(|(position, length)| {
        (length > 0.0).then_some((position / length).clamp(0.0, 1.0))
    });

    MediaView {
        position_seconds: reported,
        elapsed_seconds: elapsed,
        duration_seconds: duration,
        progress,
        title: media_title,
        artist: media_artist,
        album: media_album,
        source: media_source,
        artwork_url,
    }
}

fn button(
    key: &'static str,
    label: impl AsRef<str>,
    icon: &'static str,
    group: &'static str,
) -> ControlSpec {
    let mut control = ControlSpec::button(key, label.as_ref());
    control.icon = icon;
    control.group = group;
    control
}

fn select_control(
    key: &'static str,
    label: impl AsRef<str>,
    available: Vec<String>,
    current: &str,
    group: &'static str,
) -> ControlSpec {
    let mut control = ControlSpec::button(key, label.as_ref());
    control.kind = ControlKind::Select;
    control.value = Value::String(current.to_owned());
    control.options = available;
    // A missing options attribute does not erase an advertised capability. Keep the control
    // visible, but disabled until HA supplies valid choices that plan() can revalidate.
    control.enabled = !control.options.is_empty();
    control.group = group;
    control
}

fn form(
    key: &'static str,
    label: String,
    icon: &'static str,
    group: &'static str,
    inputs: Vec<InputSpec>,
) -> ControlSpec {
    let mut control = button(key, label, icon, group);
    control.kind = ControlKind::Form;
    control.inputs = inputs;
    control
}

const fn input(
    key: &'static str,
    label: String,
    kind: InputKind,
    value: Value,
    choices: Vec<String>,
    required: bool,
) -> InputSpec {
    InputSpec {
        key,
        label,
        kind,
        value,
        options: choices,
        minimum: 0.0,
        maximum: 100.0,
        step: 1.0,
        required,
    }
}

const fn service(service: &'static str, data: Value) -> ServiceCall {
    ServiceCall::new("media_player", service, data)
}

const fn response_service(service_name: &'static str, data: Map<String, Value>) -> ServiceCall {
    service(service_name, Value::Object(data))
}

fn observed(key: &str, value: Value) -> ActionKind {
    ActionKind::Observe {
        state: None,
        attributes: BTreeMap::from([(key.to_owned(), value)]),
    }
}

fn ensure_available(tile: &EntityTile) -> Result<(), ActionError> {
    if tile
        .state
        .as_ref()
        .is_none_or(|state| matches!(state.state.as_str(), "unavailable" | "unknown"))
    {
        Err(ActionError::Unavailable)
    } else {
        Ok(())
    }
}

fn state(tile: &EntityTile) -> &str {
    tile.state.as_ref().map_or("", |state| state.state.as_str())
}

fn require_feature(tile: &EntityTile, bit: u64) -> Result<(), ActionError> {
    if feature(tile, bit) {
        Ok(())
    } else {
        Err(ActionError::Unsupported)
    }
}

fn no_arguments(argument: &Value) -> Result<(), ActionError> {
    match argument {
        Value::Null => Ok(()),
        Value::Object(map) if map.is_empty() => Ok(()),
        _ => Err(ActionError::InvalidValue),
    }
}

fn object(argument: &Value) -> Result<&Map<String, Value>, ActionError> {
    argument.as_object().ok_or(ActionError::InvalidValue)
}

fn only_fields(map: &Map<String, Value>, allowed: &[&str]) -> Result<(), ActionError> {
    if map.keys().all(|key| allowed.contains(&key.as_str())) {
        Ok(())
    } else {
        Err(ActionError::InvalidValue)
    }
}

fn finite_number(value: &Value) -> Result<f64, ActionError> {
    value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or(ActionError::InvalidValue)
}

fn bounded_text(value: &Value, allow_empty: bool) -> Result<&str, ActionError> {
    let value = value.as_str().ok_or(ActionError::InvalidValue)?;
    if value.chars().count() > MAX_TEXT_CHARS
        || (!allow_empty && value.trim().is_empty())
        || value.contains('\0')
    {
        return Err(ActionError::InvalidValue);
    }
    Ok(value)
}

fn selected_option<'a>(
    tile: &EntityTile,
    argument: &'a Value,
    option_attribute: &str,
) -> Result<&'a str, ActionError> {
    let value = bounded_text(argument, false)?;
    if options(tile, option_attribute)
        .iter()
        .any(|option| option == value)
    {
        Ok(value)
    } else {
        Err(ActionError::InvalidValue)
    }
}

fn content_pair(argument: &Value, require_query: bool) -> Result<Map<String, Value>, ActionError> {
    let empty = Map::new();
    let map = if argument.is_null() && !require_query {
        &empty
    } else {
        object(argument)?
    };
    let allowed = if require_query {
        &[
            "search_query",
            "media_content_type",
            "media_content_id",
            "media_filter_classes",
        ][..]
    } else {
        &["media_content_type", "media_content_id"][..]
    };
    only_fields(map, allowed)?;
    let content_type = map
        .get("media_content_type")
        .filter(|value| !value.as_str().is_some_and(str::is_empty));
    let content_id = map
        .get("media_content_id")
        .filter(|value| !value.as_str().is_some_and(str::is_empty));
    if content_type.is_some() != content_id.is_some() {
        return Err(ActionError::InvalidValue);
    }
    let mut data = Map::new();
    if let (Some(content_type), Some(content_id)) = (content_type, content_id) {
        data.insert(
            "media_content_type".to_owned(),
            Value::String(bounded_text(content_type, false)?.to_owned()),
        );
        data.insert(
            "media_content_id".to_owned(),
            Value::String(bounded_text(content_id, false)?.to_owned()),
        );
    }
    Ok(data)
}

fn search_data(argument: &Value) -> Result<Map<String, Value>, ActionError> {
    let map = object(argument)?;
    only_fields(
        map,
        &[
            "search_query",
            "media_content_type",
            "media_content_id",
            "media_filter_classes",
        ],
    )?;
    let query = map.get("search_query").ok_or(ActionError::InvalidValue)?;
    let mut data = content_pair(argument, true)?;
    let query = bounded_text(query, false)?;
    data.insert("search_query".to_owned(), Value::String(query.to_owned()));
    if let Some(classes) = map.get("media_filter_classes") {
        let values = classes.as_array().ok_or(ActionError::InvalidValue)?;
        if values.is_empty() || values.len() > MEDIA_CLASSES.len() {
            return Err(ActionError::InvalidValue);
        }
        let mut parsed = Vec::with_capacity(values.len());
        for value in values {
            let class = value.as_str().ok_or(ActionError::InvalidValue)?;
            if !MEDIA_CLASSES.contains(&class)
                || class.is_empty()
                || parsed.iter().any(|x| x == class)
            {
                return Err(ActionError::InvalidValue);
            }
            parsed.push(class.to_owned());
        }
        data.insert("media_filter_classes".to_owned(), json!(parsed));
    }
    Ok(data)
}

fn play_media_data(tile: &EntityTile, argument: &Value) -> Result<Map<String, Value>, ActionError> {
    let map = object(argument)?;
    only_fields(
        map,
        &[
            "media_content_type",
            "media_content_id",
            "enqueue",
            "announce",
        ],
    )?;
    if map.contains_key("enqueue") && map.contains_key("announce") {
        return Err(ActionError::InvalidValue);
    }
    let content_type = map
        .get("media_content_type")
        .ok_or(ActionError::InvalidValue)?;
    let content_id = map
        .get("media_content_id")
        .ok_or(ActionError::InvalidValue)?;
    let mut data = Map::new();
    data.insert(
        "media_content_type".to_owned(),
        Value::String(bounded_text(content_type, false)?.to_owned()),
    );
    data.insert(
        "media_content_id".to_owned(),
        Value::String(bounded_text(content_id, false)?.to_owned()),
    );

    if let Some(enqueue) = map.get("enqueue") {
        require_feature(tile, MEDIA_ENQUEUE)?;
        let valid = enqueue.as_bool().is_some()
            || enqueue
                .as_str()
                .is_some_and(|value| ENQUEUE_MODES.contains(&value));
        if !valid {
            return Err(ActionError::InvalidValue);
        }
        data.insert("enqueue".to_owned(), enqueue.clone());
    }
    if let Some(announce) = map.get("announce") {
        require_feature(tile, MEDIA_ANNOUNCE)?;
        let Some(announce) = announce.as_bool() else {
            return Err(ActionError::InvalidValue);
        };
        data.insert("announce".to_owned(), json!(announce));
    }
    Ok(data)
}

fn group_members(tile: &EntityTile, argument: &Value) -> Result<Vec<String>, ActionError> {
    let map = object(argument)?;
    only_fields(map, &["group_members"])?;
    let members = map.get("group_members").ok_or(ActionError::InvalidValue)?;
    let parsed_from_string;
    let values: Vec<&str> = match members {
        Value::String(value) => {
            if value.chars().count() > MAX_GROUP_MEMBERS * (MAX_ENTITY_ID_CHARS + 1) {
                return Err(ActionError::InvalidValue);
            }
            parsed_from_string = value.split(',').map(str::trim).collect::<Vec<_>>();
            parsed_from_string
        }
        Value::Array(values) => values
            .iter()
            .map(|value| value.as_str().ok_or(ActionError::InvalidValue))
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(ActionError::InvalidValue),
    };
    if values.is_empty() || values.len() > MAX_GROUP_MEMBERS {
        return Err(ActionError::InvalidValue);
    }
    let mut parsed = Vec::with_capacity(values.len());
    for id in values {
        if !valid_media_entity_id(id)
            || id == tile.entity_id
            || parsed.iter().any(|existing| existing == id)
        {
            return Err(ActionError::InvalidValue);
        }
        parsed.push(id.to_owned());
    }
    Ok(parsed)
}

fn valid_media_entity_id(value: &str) -> bool {
    if value.len() > MAX_ENTITY_ID_CHARS {
        return false;
    }
    let Some(object_id) = value.strip_prefix("media_player.") else {
        return false;
    };
    !object_id.is_empty()
        && object_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn finite_attribute(tile: &EntityTile, key: &str) -> Option<f64> {
    attribute(tile, key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

fn attribute_string(tile: &EntityTile, key: &str) -> Option<String> {
    attribute(tile, key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn clamp_position(value: f64, duration: Option<f64>) -> f64 {
    let max = duration.unwrap_or(MAX_MEDIA_SECONDS).min(MAX_MEDIA_SECONDS);
    value.clamp(0.0, max)
}

/// Parses an RFC 3339 timestamp without adding a date/time dependency.
fn parse_rfc3339(value: &str) -> Option<f64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }
    let year = i64::from(digits(bytes, 0, 4)?);
    let month = i64::from(digits(bytes, 5, 2)?);
    let day = i64::from(digits(bytes, 8, 2)?);
    let hour = i64::from(digits(bytes, 11, 2)?);
    let minute = i64::from(digits(bytes, 14, 2)?);
    let second = i64::from(digits(bytes, 17, 2)?);
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day < 1 || day > days_in_month {
        return None;
    }

    let mut cursor = 19;
    let mut fraction: f64 = 0.0;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let start = cursor;
        let mut scale = 0.1;
        while let Some(byte) = bytes.get(cursor).filter(|byte| byte.is_ascii_digit()) {
            fraction = fraction.mul_add(f64::from(*byte - b'0'), scale);
            scale *= 0.1;
            cursor += 1;
        }
        if cursor == start {
            return None;
        }
    }

    let offset_seconds = match bytes.get(cursor) {
        Some(b'Z') if cursor + 1 == bytes.len() => 0_i64,
        Some(sign @ (b'+' | b'-')) if cursor + 6 == bytes.len() && bytes[cursor + 3] == b':' => {
            let offset_hour = i64::from(digits(bytes, cursor + 1, 2)?);
            let offset_minute = i64::from(digits(bytes, cursor + 4, 2)?);
            if offset_hour > 23 || offset_minute > 59 {
                return None;
            }
            let offset = offset_hour * 3_600 + offset_minute * 60;
            if *sign == b'+' { offset } else { -offset }
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day);
    // Four decimal year digits bound this value to an exactly representable f64 integer.
    #[allow(clippy::cast_precision_loss)]
    let whole_seconds =
        (days * 86_400 + hour * 3_600 + minute * 60 + second - offset_seconds) as f64;
    Some(whole_seconds + fraction)
}

fn digits(bytes: &[u8], start: usize, len: usize) -> Option<u32> {
    let slice = bytes.get(start..start + len)?;
    if !slice.iter().all(u8::is_ascii_digit) {
        return None;
    }
    slice.iter().try_fold(0_u32, |value, byte| {
        value.checked_mul(10)?.checked_add(u32::from(*byte - b'0'))
    })
}

const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    // `From<bool>` is not const on the workspace's stable Rust toolchain.
    #[allow(clippy::bool_to_int_with_if)]
    let adjusted_year = year - if month <= 2 { 1 } else { 0 };
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}
