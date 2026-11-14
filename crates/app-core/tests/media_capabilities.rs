use std::collections::BTreeMap;

use njorun_app_core::controls::{ControlKind, InputKind};
use njorun_app_core::media::{self, MediaView};
use njorun_app_core::{ActionError, ActionKind, EntityDomain, EntityTile, HaEntityState};
use serde_json::{Value, json};

const PAUSE: u64 = 1;
const SEEK: u64 = 2;
const VOLUME_SET: u64 = 4;
const VOLUME_MUTE: u64 = 8;
const PREVIOUS_TRACK: u64 = 16;
const NEXT_TRACK: u64 = 32;
const TURN_ON: u64 = 128;
const TURN_OFF: u64 = 256;
const PLAY_MEDIA: u64 = 512;
const SELECT_SOURCE: u64 = 2_048;
const STOP: u64 = 4_096;
const CLEAR_PLAYLIST: u64 = 8_192;
const PLAY: u64 = 16_384;
const SHUFFLE_SET: u64 = 32_768;
const SELECT_SOUND_MODE: u64 = 65_536;
const BROWSE_MEDIA: u64 = 131_072;
const REPEAT_SET: u64 = 262_144;
const GROUPING: u64 = 524_288;
const MEDIA_ANNOUNCE: u64 = 1_048_576;
const MEDIA_ENQUEUE: u64 = 2_097_152;
const SEARCH_MEDIA: u64 = 4_194_304;

fn tile(state: &str, features: u64, attributes: Value) -> EntityTile {
    let Value::Object(attributes) = attributes else {
        panic!("fixture attributes are an object");
    };
    EntityTile {
        entity_id: "media_player.den".to_owned(),
        name: "Den player".to_owned(),
        icon: None,
        domain: EntityDomain::MediaPlayer,
        state: Some(HaEntityState {
            entity_id: "media_player.den".to_owned(),
            state: state.to_owned(),
            attributes: BTreeMap::from([("supported_features".to_owned(), json!(features))])
                .into_iter()
                .chain(attributes)
                .collect(),
            last_updated: None,
        }),
        is_stale: false,
        command_enabled: true,
        action_feedback: None,
    }
}

fn controls_for(tile: &EntityTile) -> Vec<&'static str> {
    media::controls(tile, false)
        .iter()
        .map(|control| control.key)
        .collect()
}

fn call(tile: &EntityTile, key: &str, argument: &Value) -> (ActionKind, String, String, Value) {
    let (kind, call) = media::plan(tile, key, argument).expect("action is supported");
    (
        kind,
        call.domain.to_owned(),
        call.service.to_owned(),
        call.data,
    )
}

#[test]
fn feature_bits_offer_each_supported_media_control() {
    let all = PAUSE
        | SEEK
        | VOLUME_SET
        | VOLUME_MUTE
        | PREVIOUS_TRACK
        | NEXT_TRACK
        | TURN_ON
        | TURN_OFF
        | PLAY_MEDIA
        | SELECT_SOURCE
        | STOP
        | CLEAR_PLAYLIST
        | PLAY
        | SHUFFLE_SET
        | SELECT_SOUND_MODE
        | BROWSE_MEDIA
        | REPEAT_SET
        | GROUPING
        | MEDIA_ANNOUNCE
        | MEDIA_ENQUEUE
        | SEARCH_MEDIA;
    let player = tile(
        "playing",
        all,
        json!({
            "source_list": ["HDMI 1"],
            "sound_mode_list": ["movie"],
            "volume_level": 0.35,
            "is_volume_muted": false,
            "shuffle": true,
            "repeat": "all",
            "media_duration": 600
        }),
    );
    let keys = controls_for(&player);
    for key in [
        "media_pause",
        "media_stop",
        "media_previous",
        "media_next",
        "media_power_off",
        "media_volume",
        "media_mute",
        "media_seek",
        "media_source",
        "media_sound_mode",
        "media_shuffle",
        "media_repeat",
        "media_browse",
        "media_search",
        "media_play_item",
        "media_clear_queue",
        "media_join",
        "media_unjoin",
    ] {
        assert!(keys.contains(&key), "missing advertised control {key}");
    }
    assert!(!keys.contains(&"media_play"));

    let volume = media::controls(&player, false)
        .into_iter()
        .find(|control| control.key == "media_volume")
        .unwrap();
    assert_eq!(volume.kind, ControlKind::Slider);
    assert_eq!(volume.value, json!(35.0));
    assert_eq!(volume.group, "volume");
}

#[test]
fn absent_feature_bits_hide_controls_and_reject_actions_even_if_attributes_exist() {
    let player = tile(
        "playing",
        0,
        json!({
            "source_list": ["HDMI 1"],
            "sound_mode_list": ["movie"],
            "volume_level": 0.9,
            "shuffle": true,
            "repeat": "all"
        }),
    );
    assert_eq!(controls_for(&player).len(), 0);
    for (key, argument) in [
        ("media_play", Value::Null),
        ("media_pause", Value::Null),
        ("media_volume", json!(90)),
        ("media_mute", json!(true)),
        ("media_source", json!("HDMI 1")),
        ("media_sound_mode", json!("movie")),
        ("media_shuffle", json!(false)),
        ("media_repeat", json!("all")),
        ("media_browse", json!({})),
        ("media_search", json!({"search_query": "news"})),
        (
            "media_play_item",
            json!({"media_content_type": "music", "media_content_id": "1"}),
        ),
        ("media_clear_queue", Value::Null),
        (
            "media_join",
            json!({"group_members": "media_player.kitchen"}),
        ),
    ] {
        assert_eq!(
            media::plan(&player, key, &argument),
            Err(ActionError::Unsupported),
            "unexpectedly accepted {key} without its feature bit"
        );
    }
}

#[test]
fn source_and_sound_capability_bits_are_not_inferred_from_missing_option_attributes() {
    let player = tile("idle", SELECT_SOURCE | SELECT_SOUND_MODE, json!({}));
    let controls = media::controls(&player, false);
    assert_eq!(controls.len(), 2);
    assert!(
        controls
            .iter()
            .all(|control| control.kind == ControlKind::Select)
    );
    assert!(controls.iter().all(|control| !control.enabled));
    assert_eq!(
        media::plan(&player, "media_source", &json!("HDMI 1")),
        Err(ActionError::InvalidValue)
    );
    assert_eq!(
        media::plan(&player, "media_sound_mode", &json!("movie")),
        Err(ActionError::InvalidValue)
    );
}

#[test]
fn transport_controls_follow_playback_and_power_state() {
    let features = PAUSE | PLAY | STOP | NEXT_TRACK | TURN_ON | TURN_OFF;
    let playing = tile("playing", features, json!({}));
    let keys = controls_for(&playing);
    assert!(keys.contains(&"media_pause"));
    assert!(keys.contains(&"media_stop"));
    assert!(!keys.contains(&"media_play"));

    let paused = tile("paused", features, json!({}));
    let keys = controls_for(&paused);
    assert!(keys.contains(&"media_play"));
    assert!(!keys.contains(&"media_pause"));

    let standby = tile("standby", features, json!({}));
    assert!(controls_for(&standby).contains(&"media_play"));
    assert_eq!(call(&standby, "media_play", &Value::Null).2, "media_play");

    let buffering = tile("buffering", features, json!({}));
    let keys = controls_for(&buffering);
    assert!(keys.contains(&"media_pause"));
    assert!(keys.contains(&"media_stop"));
    assert!(!keys.contains(&"media_play"));

    let off = tile("off", features, json!({}));
    assert_eq!(controls_for(&off), vec!["media_power_on"]);
    assert_eq!(call(&off, "media_power_on", &Value::Null).2, "turn_on");
    assert_eq!(
        media::plan(&off, "media_power_off", &Value::Null),
        Err(ActionError::Unsupported)
    );

    let off_without_turn_on = tile("off", features & !TURN_ON, json!({}));
    assert_eq!(controls_for(&off_without_turn_on).len(), 0);
    assert_eq!(
        media::plan(&off_without_turn_on, "media_power_on", &Value::Null),
        Err(ActionError::Unsupported)
    );
}

#[test]
fn unavailable_players_have_no_controls_and_cannot_be_actioned() {
    let player = tile("unavailable", TURN_ON | PLAY | VOLUME_SET, json!({}));
    assert_eq!(controls_for(&player).len(), 0);
    assert_eq!(
        media::plan(&player, "media_play", &Value::Null),
        Err(ActionError::Unavailable)
    );
    let unknown = tile("unknown", TURN_ON, json!({}));
    assert_eq!(
        media::plan(&unknown, "media_power_on", &Value::Null),
        Err(ActionError::Unavailable)
    );
}

#[test]
fn slider_toggles_and_selects_revalidate_values() {
    let player = tile(
        "playing",
        VOLUME_SET
            | VOLUME_MUTE
            | SEEK
            | SELECT_SOURCE
            | SELECT_SOUND_MODE
            | SHUFFLE_SET
            | REPEAT_SET,
        json!({
            "source_list": ["HDMI 1", "TV"],
            "sound_mode_list": ["movie", "music"],
            "media_duration": 90
        }),
    );
    let (kind, domain, service, data) = call(&player, "media_volume", &json!(42.5));
    assert_eq!(kind, ActionKind::SetVolume(43));
    assert_eq!(
        (domain.as_str(), service.as_str()),
        ("media_player", "volume_set")
    );
    assert_eq!(data, json!({"volume_level": 0.425}));

    let (kind, _, service, data) = call(&player, "media_mute", &json!(true));
    assert!(matches!(kind, ActionKind::Observe { .. }));
    assert_eq!(service, "volume_mute");
    assert_eq!(data, json!({"is_volume_muted": true}));
    assert_eq!(
        call(&player, "media_shuffle", &json!(false)).3,
        json!({"shuffle": false})
    );
    assert_eq!(
        call(&player, "media_repeat", &json!("one")).3,
        json!({"repeat": "one"})
    );
    assert_eq!(
        call(&player, "media_source", &json!("TV")).3,
        json!({"source": "TV"})
    );
    assert_eq!(
        call(&player, "media_sound_mode", &json!("movie")).3,
        json!({"sound_mode": "movie"})
    );
    assert_eq!(
        call(&player, "media_seek", &json!(65)).3,
        json!({"seek_position": 65.0})
    );

    for (key, value) in [
        ("media_volume", json!(-1)),
        ("media_volume", json!(101)),
        ("media_mute", json!("true")),
        ("media_shuffle", json!(0)),
        ("media_repeat", json!("invalid")),
        ("media_source", json!("missing")),
        ("media_sound_mode", json!("missing")),
        ("media_seek", json!(91)),
    ] {
        assert_eq!(
            media::plan(&player, key, &value),
            Err(ActionError::InvalidValue),
            "accepted malformed {key} value {value}"
        );
    }
}

#[test]
fn media_browse_and_search_validate_and_build_response_service_arguments() {
    let player = tile("idle", BROWSE_MEDIA | SEARCH_MEDIA, json!({}));
    let controls = media::controls(&player, false);
    let browse_control = controls
        .iter()
        .find(|control| control.key == "media_browse")
        .unwrap();
    assert_eq!(browse_control.kind, ControlKind::Button);
    assert_eq!(browse_control.inputs.len(), 0);
    assert_eq!(
        media::plan(&player, "media_browse", &Value::Null)
            .unwrap()
            .1
            .data,
        json!({})
    );
    let browse = call(
        &player,
        "media_browse",
        &json!({"media_content_type": "library", "media_content_id": "root"}),
    );
    assert_eq!(
        (browse.1.as_str(), browse.2.as_str()),
        ("media_player", "browse_media")
    );
    assert_eq!(
        browse.3,
        json!({"media_content_type": "library", "media_content_id": "root"})
    );

    // Browse result folders can re-enter the same response service with HA IDs directly.
    let nested = call(
        &player,
        "media_browse",
        &json!({"media_content_type": "music", "media_content_id": "library/albums"}),
    );
    assert_eq!(nested.2, "browse_media");
    assert_eq!(
        nested.3,
        json!({"media_content_type": "music", "media_content_id": "library/albums"})
    );
    assert_eq!(call(&player, "media_browse", &json!({})).3, json!({}));

    let search = call(
        &player,
        "media_search",
        &json!({"search_query": "Blue Train", "media_filter_classes": ["album", "track"]}),
    );
    assert_eq!(search.2, "search_media");
    assert_eq!(
        search.3,
        json!({"search_query": "Blue Train", "media_filter_classes": ["album", "track"]})
    );
    for argument in [
        json!({"media_content_type": "music"}),
        json!({"search_query": ""}),
        json!({"search_query": "news", "arbitrary_data": true}),
        json!({"search_query": "news", "media_filter_classes": ["no_such_class"]}),
        json!({"search_query": "news", "media_content_id": "id"}),
    ] {
        let key = if argument.get("search_query").is_some() {
            "media_search"
        } else {
            "media_browse"
        };
        assert_eq!(
            media::plan(&player, key, &argument),
            Err(ActionError::InvalidValue)
        );
    }
}

#[test]
#[allow(clippy::too_many_lines)] // Keep the play_media field and capability matrix together.
fn play_media_forms_enforce_capabilities_and_home_assistant_field_shapes() {
    let player = tile(
        "idle",
        PLAY_MEDIA | MEDIA_ENQUEUE | MEDIA_ANNOUNCE | CLEAR_PLAYLIST,
        json!({}),
    );
    let play_control = media::controls(&player, true)
        .into_iter()
        .find(|control| control.key == "media_play_item")
        .unwrap();
    assert_eq!(play_control.kind, ControlKind::Form);
    assert!(
        play_control
            .inputs
            .iter()
            .any(|input| input.key == "enqueue")
    );
    assert!(
        play_control
            .inputs
            .iter()
            .any(|input| input.key == "announce")
    );
    let enqueue = play_control
        .inputs
        .iter()
        .find(|input| input.key == "enqueue")
        .unwrap();
    assert_eq!(enqueue.kind, InputKind::Select);
    assert!(enqueue.value.is_null());
    assert!(
        play_control
            .inputs
            .iter()
            .find(|input| input.key == "announce")
            .unwrap()
            .value
            .is_null()
    );

    let data = call(
        &player,
        "media_play_item",
        &json!({
            "media_content_type": "music",
            "media_content_id": "track/1",
            "enqueue": "replace"
        }),
    );
    assert_eq!(data.2, "play_media");
    assert_eq!(
        data.3,
        json!({
            "media_content_type": "music",
            "media_content_id": "track/1",
            "enqueue": "replace"
        })
    );
    let announcement = call(
        &player,
        "media_play_item",
        &json!({
            "media_content_type": "music",
            "media_content_id": "track/1",
            "announce": true
        }),
    );
    assert_eq!(
        announcement.3,
        json!({
            "media_content_type": "music",
            "media_content_id": "track/1",
            "announce": true
        })
    );
    assert_eq!(
        call(&player, "media_clear_queue", &Value::Null).2,
        "clear_playlist"
    );

    for argument in [
        json!({"media_content_type": "music"}),
        json!({"media_content_type": "music", "media_content_id": "x", "bad": 1}),
        json!({"media_content_type": "music", "media_content_id": "x", "announce": "yes"}),
        json!({"media_content_type": "music", "media_content_id": "x", "enqueue": "later"}),
        json!({"media_content_type": "music", "media_content_id": "x", "enqueue": "add", "announce": true}),
        json!({"media_content_type": "music", "media_content_id": "x", "enqueue": "add", "announce": false}),
    ] {
        assert_eq!(
            media::plan(&player, "media_play_item", &argument),
            Err(ActionError::InvalidValue)
        );
    }

    let no_enqueue = tile("idle", PLAY_MEDIA, json!({}));
    assert_eq!(
        media::plan(
            &no_enqueue,
            "media_play_item",
            &json!({"media_content_type": "music", "media_content_id": "x", "enqueue": "replace"})
        ),
        Err(ActionError::Unsupported)
    );
    let no_announce = tile("idle", PLAY_MEDIA | MEDIA_ENQUEUE, json!({}));
    assert_eq!(
        media::plan(
            &no_announce,
            "media_play_item",
            &json!({"media_content_type": "music", "media_content_id": "x", "announce": true})
        ),
        Err(ActionError::Unsupported)
    );
}

#[test]
fn play_media_payload_matches_live_home_assistant_service_data_shape() {
    let player = tile("idle", PLAY_MEDIA | MEDIA_ENQUEUE, json!({}));
    let (action, call) = media::plan(
        &player,
        "media_play_item",
        &json!({
            "media_content_type": "music",
            "media_content_id": "media-source://media_source/local/track.mp3",
            "enqueue": "next"
        }),
    )
    .unwrap();

    assert_eq!(action, ActionKind::RunScene);
    assert_eq!(call.domain, "media_player");
    assert_eq!(call.service, "play_media");
    assert_eq!(
        call.data,
        json!({
            "media_content_type": "music",
            "media_content_id": "media-source://media_source/local/track.mp3",
            "enqueue": "next"
        })
    );
    assert!(call.data.get("media").is_none());
}

#[test]
fn grouping_accepts_bounded_entity_ids_and_rejects_malformed_targets() {
    let player = tile("idle", GROUPING, json!({}));
    let control = media::controls(&player, false)
        .into_iter()
        .find(|control| control.key == "media_join")
        .unwrap();
    assert_eq!(control.kind, ControlKind::Form);
    assert_eq!(control.inputs[0].kind, InputKind::Entity);
    let joined = call(
        &player,
        "media_join",
        &json!({"group_members": "media_player.kitchen, media_player.office"}),
    );
    assert_eq!(joined.2, "join");
    assert_eq!(
        joined.3,
        json!({"group_members": ["media_player.kitchen", "media_player.office"]})
    );
    assert_eq!(call(&player, "media_unjoin", &Value::Null).2, "unjoin");

    for argument in [
        json!({"group_members": ""}),
        json!({"group_members": "media_player.den"}),
        json!({"group_members": "light.kitchen"}),
        json!({"group_members": "media_player.bad-id"}),
        json!({"group_members": ["media_player.office", "media_player.office"]}),
        json!({"group_members": "media_player.kitchen", "extra": true}),
    ] {
        assert_eq!(
            media::plan(&player, "media_join", &argument),
            Err(ActionError::InvalidValue)
        );
    }
}

#[test]
fn media_view_uses_ha_attributes_and_advances_position_from_rfc3339_timestamp() {
    let player = tile(
        "playing",
        0,
        json!({
            "media_position": 20,
            "media_position_updated_at": "1970-01-01T00:01:40Z",
            "media_duration": 35,
            "media_title": "Blue Train",
            "media_artist": "John Coltrane",
            "media_album_name": "Blue Train",
            "source": "Library",
            "entity_picture": "/api/media_player_proxy/media_player.den?token=abc"
        }),
    );
    let media: MediaView = media::view(&player, 120.0);
    assert_eq!(media.position_seconds, Some(20.0));
    assert_eq!(media.elapsed_seconds, Some(35.0));
    assert_eq!(media.duration_seconds, Some(35.0));
    assert_eq!(media.progress, Some(1.0));
    assert_eq!(media.title.as_deref(), Some("Blue Train"));
    assert_eq!(media.artist.as_deref(), Some("John Coltrane"));
    assert_eq!(media.album.as_deref(), Some("Blue Train"));
    assert_eq!(media.source.as_deref(), Some("Library"));
    assert_eq!(
        media.artwork_url.as_deref(),
        Some("/api/media_player_proxy/media_player.den?token=abc")
    );

    let utc_offset = tile(
        "playing",
        0,
        json!({
            "media_position": 9,
            "media_position_updated_at": "1970-01-01T01:00:00+01:00"
        }),
    );
    assert_eq!(media::view(&utc_offset, 15.0).elapsed_seconds, Some(24.0));

    let paused = tile(
        "paused",
        0,
        json!({
            "media_position": 9,
            "media_position_updated_at": "1970-01-01T00:00:10Z"
        }),
    );
    assert_eq!(media::view(&paused, 100.0).elapsed_seconds, Some(9.0));

    let no_url = tile("idle", 0, json!({"media_title": "Track"}));
    assert_eq!(media::view(&no_url, 0.0).artwork_url, None);
}

#[test]
fn wrong_domain_and_unknown_action_are_rejected() {
    let mut light = tile("on", 0, json!({}));
    light.domain = EntityDomain::Light;
    assert_eq!(
        media::plan(&light, "media_play", &Value::Null),
        Err(ActionError::Unsupported)
    );
    let player = tile("idle", PLAY, json!({}));
    assert_eq!(
        media::plan(&player, "raw_service", &json!({"service": "turn_off"})),
        Err(ActionError::Unsupported)
    );
    assert_eq!(
        media::plan(&player, "media_play", &json!({"extra": true})),
        Err(ActionError::InvalidValue)
    );
}
