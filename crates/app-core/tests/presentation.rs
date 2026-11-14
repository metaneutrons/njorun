//! Tile texts, icons and details-sheet controls for every entity domain, in both languages.
#![allow(clippy::needless_pass_by_value, clippy::too_many_lines)] // Fixtures and case tables.

use njorun_app_core::presentation::{
    Control, IconKind, action_label, dark_mode, detail_attributes, detail_controls, detail_value,
    detail_view, entity_icon, feedback_value, light_supports_brightness, pending_label,
    room_summary, spaced_code, state_value, tile_active, turning_label, unavailable_label,
};
use njorun_app_core::{
    ActionFeedback, ActionKind, AlarmMode, EntityDomain, EntityTile, HaEntityState, LockTarget,
    PlaybackTarget, Room, RoomId, TargetState,
};
use serde_json::{Value, json};

fn tile(domain: EntityDomain, state: &str, attributes: Value) -> EntityTile {
    EntityTile {
        entity_id: "test.entity".to_owned(),
        name: "Test".to_owned(),
        icon: None,
        domain,
        state: Some(HaEntityState {
            entity_id: "test.entity".to_owned(),
            state: state.to_owned(),
            attributes: attributes
                .as_object()
                .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
            last_updated: None,
        }),
        is_stale: false,
        command_enabled: true,
        action_feedback: None,
    }
}

fn texts(domain: EntityDomain, state: &str, attributes: &Value) -> (String, String) {
    let tile = tile(domain, state, attributes.clone());
    (state_value(&tile, false), state_value(&tile, true))
}

#[test]
fn room_summaries_prioritize_live_values_without_guessing_unknown_lights() {
    let mut room = Room {
        id: RoomId::Unassigned,
        name: "Hall".to_owned(),
        entities: Vec::new(),
    };
    assert_eq!(room_summary(&room, false), "No controls yet");
    assert_eq!(room_summary(&room, true), "Noch keine Steuerelemente");
    room.entities
        .push(tile(EntityDomain::Light, "on", json!({})));
    assert_eq!(room_summary(&room, false), "1 light on");
    assert_eq!(room_summary(&room, true), "1 Licht an");
    room.entities
        .push(tile(EntityDomain::Switch, "off", json!({})));
    assert_eq!(room_summary(&room, false), "1 light on");
    room.entities.push(tile(
        EntityDomain::Sensor,
        "21.4",
        json!({"device_class": "temperature", "unit_of_measurement": "°C"}),
    ));
    assert_eq!(room_summary(&room, false), "1 light on · 21.4 °C");
    assert_eq!(room_summary(&room, true), "1 Licht an · 21,4 °C");

    let mut lights_off = Room {
        id: RoomId::Area("room".to_owned()),
        name: "Room".to_owned(),
        entities: vec![tile(EntityDomain::Light, "off", json!({}))],
    };
    assert_eq!(room_summary(&lights_off, false), "Lights off");
    assert_eq!(room_summary(&lights_off, true), "Lichter aus");
    lights_off.entities[0].is_stale = true;
    assert_eq!(room_summary(&lights_off, false), "1 control");

    let temperature_only = Room {
        id: RoomId::Area("sensor".to_owned()),
        name: "Sensor".to_owned(),
        entities: vec![tile(
            EntityDomain::Sensor,
            "21.4",
            json!({"device_class": "temperature", "unit_of_measurement": "°C"}),
        )],
    };
    assert_eq!(room_summary(&temperature_only, false), "21.4 °C");
    assert_eq!(room_summary(&temperature_only, true), "21,4 °C");
}

#[test]
fn state_texts_cover_every_domain() {
    use EntityDomain as D;
    let none = json!({});
    let cases: Vec<(D, &str, Value, &str, &str)> = vec![
        (D::Switch, "on", none.clone(), "On", "Ein"),
        (D::InputBoolean, "off", none.clone(), "Off", "Aus"),
        (D::Switch, "unknown", none.clone(), "Unknown", "Unbekannt"),
        (
            D::BinarySensor,
            "on",
            json!({"device_class": "door"}),
            "Open",
            "Offen",
        ),
        (
            D::BinarySensor,
            "off",
            json!({"device_class": "window"}),
            "Closed",
            "Geschlossen",
        ),
        (
            D::BinarySensor,
            "on",
            json!({"device_class": "motion"}),
            "Motion detected",
            "Bewegung erkannt",
        ),
        (
            D::BinarySensor,
            "off",
            json!({"device_class": "motion"}),
            "No motion",
            "Keine Bewegung",
        ),
        (
            D::BinarySensor,
            "on",
            json!({"device_class": "moisture"}),
            "Wet",
            "Feucht",
        ),
        (
            D::BinarySensor,
            "off",
            json!({"device_class": "moisture"}),
            "Dry",
            "Trocken",
        ),
        (
            D::Light,
            "on",
            json!({"brightness": 128}),
            "On · 50%",
            "Ein · 50%",
        ),
        (D::Light, "on", none.clone(), "On", "Ein"),
        (
            D::Fan,
            "on",
            json!({"percentage": 33.4}),
            "On · 33%",
            "Ein · 33%",
        ),
        (D::Fan, "off", json!({"percentage": 0}), "Off", "Aus"),
        (
            D::Scene,
            "unknown",
            none.clone(),
            "Run scene",
            "Szene ausführen",
        ),
        (
            D::Script,
            "off",
            none.clone(),
            "Run script",
            "Skript ausführen",
        ),
        (D::Button, "unknown", none.clone(), "Press", "Drücken"),
        (
            D::InputButton,
            "2026-11-08T10:00:00+00:00",
            none.clone(),
            "Press",
            "Drücken",
        ),
        (D::Automation, "on", none.clone(), "Enabled", "Aktiv"),
        (D::Automation, "off", none.clone(), "Disabled", "Inaktiv"),
        (D::Lock, "locked", none.clone(), "Locked", "Verriegelt"),
        (D::Lock, "unlocked", none.clone(), "Unlocked", "Entriegelt"),
        (
            D::Lock,
            "locking",
            none.clone(),
            "Locking…",
            "Wird verriegelt …",
        ),
        (
            D::Lock,
            "unlocking",
            none.clone(),
            "Unlocking…",
            "Wird entriegelt …",
        ),
        (D::Lock, "jammed", none.clone(), "Jammed", "Blockiert"),
        (D::Lock, "open", none.clone(), "Open", "Offen"),
        (
            D::Lock,
            "opening",
            none.clone(),
            "Opening…",
            "Wird geöffnet …",
        ),
        (D::Lock, "mystery", none.clone(), "mystery", "mystery"),
        (
            D::AlarmControlPanel,
            "disarmed",
            none.clone(),
            "Disarmed",
            "Deaktiviert",
        ),
        (
            D::AlarmControlPanel,
            "armed_home",
            none.clone(),
            "Armed home",
            "Zuhause scharf",
        ),
        (
            D::AlarmControlPanel,
            "armed_away",
            none.clone(),
            "Armed away",
            "Abwesend scharf",
        ),
        (
            D::AlarmControlPanel,
            "armed_night",
            none.clone(),
            "Armed night",
            "Nacht scharf",
        ),
        (
            D::AlarmControlPanel,
            "armed_vacation",
            none.clone(),
            "Armed vacation",
            "Urlaub scharf",
        ),
        (
            D::AlarmControlPanel,
            "armed_custom_bypass",
            none.clone(),
            "Armed custom",
            "Benutzerdefiniert scharf",
        ),
        (
            D::AlarmControlPanel,
            "arming",
            none.clone(),
            "Arming…",
            "Wird scharf geschaltet …",
        ),
        (
            D::AlarmControlPanel,
            "disarming",
            none.clone(),
            "Disarming…",
            "Wird deaktiviert …",
        ),
        (
            D::AlarmControlPanel,
            "pending",
            none.clone(),
            "Pending",
            "Wartet",
        ),
        (
            D::AlarmControlPanel,
            "triggered",
            none.clone(),
            "Triggered!",
            "Ausgelöst!",
        ),
        (D::AlarmControlPanel, "odd", none.clone(), "odd", "odd"),
        (
            D::MediaPlayer,
            "playing",
            json!({"media_title": "Song", "media_artist": "Band"}),
            "Band – Song",
            "Band – Song",
        ),
        (
            D::MediaPlayer,
            "playing",
            json!({"media_title": "Song"}),
            "Song",
            "Song",
        ),
        (
            D::MediaPlayer,
            "playing",
            none.clone(),
            "Playing",
            "Wiedergabe",
        ),
        (
            D::MediaPlayer,
            "paused",
            json!({"media_title": "Song"}),
            "Paused · Song",
            "Pausiert · Song",
        ),
        (D::MediaPlayer, "paused", none.clone(), "Paused", "Pausiert"),
        (D::MediaPlayer, "idle", none.clone(), "Ready", "Bereit"),
        (D::MediaPlayer, "on", none.clone(), "Ready", "Bereit"),
        (
            D::MediaPlayer,
            "standby",
            none.clone(),
            "Standby",
            "Bereitschaft",
        ),
        (
            D::MediaPlayer,
            "buffering",
            none.clone(),
            "Buffering…",
            "Puffert …",
        ),
        (D::MediaPlayer, "off", none.clone(), "Off", "Aus"),
        (
            D::MediaPlayer,
            "streaming",
            none.clone(),
            "streaming",
            "streaming",
        ),
        (D::Camera, "idle", none.clone(), "Camera", "Kamera"),
        (
            D::Sensor,
            "21.5",
            json!({"unit_of_measurement": "°C"}),
            "21.5 °C",
            "21,5 °C",
        ),
        (D::Sensor, "3", json!({"unit_of_measurement": ""}), "3", "3"),
        (
            D::Number,
            "40",
            json!({"unit_of_measurement": "%"}),
            "40 %",
            "40 %",
        ),
        (D::InputNumber, "7", none.clone(), "7", "7"),
        (D::Select, "Eco", none.clone(), "Eco", "Eco"),
        (D::InputSelect, "Away", none, "Away", "Away"),
    ];
    for (domain, state, attributes, english, german) in cases {
        assert_eq!(
            texts(domain, state, &attributes),
            (english.to_owned(), german.to_owned()),
            "{domain:?} {state}"
        );
    }
}

#[test]
fn stale_and_missing_states_are_marked() {
    let mut light = tile(EntityDomain::Light, "off", json!({}));
    light.is_stale = true;
    assert_eq!(state_value(&light, false), "Last known · Off");
    assert_eq!(state_value(&light, true), "Letzter Stand · Aus");
    let mut scene = tile(EntityDomain::Scene, "unknown", json!({}));
    scene.is_stale = true;
    assert_eq!(state_value(&scene, false), "Scene offline");
    assert_eq!(state_value(&scene, true), "Szene offline");
    let mut missing = tile(EntityDomain::Switch, "on", json!({}));
    missing.state = None;
    assert_eq!(state_value(&missing, true), unavailable_label(true));
    assert_eq!(detail_attributes(&missing, false), "");
    assert_eq!(detail_controls(&missing), Control::None);
    let unavailable = tile(
        EntityDomain::Light,
        "unavailable",
        json!({"brightness": 10}),
    );
    assert_eq!(state_value(&unavailable, false), "Unavailable");
    assert_eq!(detail_controls(&unavailable), Control::None);
}

#[test]
fn pending_and_feedback_texts() {
    use EntityDomain as D;
    let scene = tile(D::Scene, "unknown", json!({}));
    let script = tile(D::Script, "off", json!({}));
    let button = tile(D::Button, "unknown", json!({}));
    let cases: Vec<(ActionKind, &EntityTile, &str, &str)> = vec![
        (
            ActionKind::Toggle(TargetState::On),
            &scene,
            "Turning on…",
            "Wird eingeschaltet…",
        ),
        (
            ActionKind::RunScene,
            &scene,
            "Running scene…",
            "Szene wird ausgeführt …",
        ),
        (
            ActionKind::RunScene,
            &script,
            "Running script…",
            "Skript läuft …",
        ),
        (
            ActionKind::RunScene,
            &button,
            "Working…",
            "Wird ausgeführt …",
        ),
        (
            ActionKind::Lock(LockTarget::Locked),
            &button,
            "Locking…",
            "Wird verriegelt …",
        ),
        (
            ActionKind::Lock(LockTarget::Unlocked),
            &button,
            "Unlocking…",
            "Wird entriegelt …",
        ),
        (
            ActionKind::Alarm("disarmed".into()),
            &button,
            "Disarming…",
            "Wird deaktiviert …",
        ),
        (
            ActionKind::Alarm("armed_away".into()),
            &button,
            "Arming…",
            "Wird scharf geschaltet …",
        ),
        (
            ActionKind::Playback(PlaybackTarget::Playing),
            &button,
            "Starting…",
            "Wird gestartet …",
        ),
        (
            ActionKind::Playback(PlaybackTarget::Paused),
            &button,
            "Pausing…",
            "Wird pausiert …",
        ),
        (
            ActionKind::SetBrightness(40),
            &button,
            "Setting…",
            "Wird gesetzt …",
        ),
    ];
    for (action, tile, english, german) in cases {
        assert_eq!(pending_label(&action, tile, false), english, "{action:?}");
        assert_eq!(pending_label(&action, tile, true), german, "{action:?}");
    }
    assert_eq!(turning_label(TargetState::Off, false), "Turning off…");
    assert_eq!(turning_label(TargetState::Off, true), "Wird ausgeschaltet…");

    let switch = tile(D::Switch, "on", json!({}));
    let feedback = |value: ActionFeedback, german| feedback_value(Some(&value), &switch, german);
    assert_eq!(feedback_value(None, &switch, false), None);
    assert_eq!(
        feedback(
            ActionFeedback::AwaitingAcknowledgement {
                action: ActionKind::RunScene
            },
            false
        )
        .as_deref(),
        Some("Working…")
    );
    assert_eq!(
        feedback(
            ActionFeedback::AcceptedAwaitingConfirmation {
                action: ActionKind::SetVolume(3)
            },
            true
        )
        .as_deref(),
        Some("Wird gesetzt …")
    );
    assert_eq!(
        feedback(
            ActionFeedback::AcceptedAwaitingState {
                target: TargetState::Off
            },
            false
        )
        .as_deref(),
        Some("Turning off…")
    );
    assert_eq!(
        feedback(ActionFeedback::SceneRequested, false).as_deref(),
        Some("Requested")
    );
    assert_eq!(
        feedback(ActionFeedback::SceneRequested, true).as_deref(),
        Some("Angefordert")
    );
    assert_eq!(
        feedback(
            ActionFeedback::Confirmed {
                state: TargetState::On
            },
            true
        )
        .as_deref(),
        Some("Ein")
    );
    let rejected = ActionFeedback::Rejected {
        reason: "no".into(),
    };
    assert_eq!(
        feedback(rejected.clone(), false).as_deref(),
        Some("Action failed")
    );
    assert_eq!(
        feedback(rejected, true).as_deref(),
        Some("Aktion fehlgeschlagen")
    );
    let unconfirmed = ActionFeedback::Unconfirmed {
        action: ActionKind::RunScene,
    };
    assert_eq!(
        feedback(unconfirmed.clone(), false).as_deref(),
        Some("Unconfirmed")
    );
    assert_eq!(
        feedback(unconfirmed, true).as_deref(),
        Some("Nicht bestätigt")
    );
}

#[test]
fn active_tiles_and_icons() {
    use EntityDomain as D;
    let active = [
        (D::Light, "on", true),
        (D::Automation, "on", true),
        (D::Script, "off", false),
        (D::MediaPlayer, "playing", true),
        (D::MediaPlayer, "paused", false),
        (D::Lock, "unlocked", true),
        (D::Lock, "open", true),
        (D::Lock, "locked", false),
        (D::AlarmControlPanel, "armed_night", true),
        (D::AlarmControlPanel, "triggered", true),
        (D::AlarmControlPanel, "disarmed", false),
        (D::Sensor, "on", false),
    ];
    for (domain, state, expected) in active {
        assert_eq!(
            tile_active(&tile(domain, state, json!({}))),
            expected,
            "{domain:?} {state}"
        );
    }
    let mut stale = tile(D::Light, "on", json!({}));
    stale.is_stale = true;
    assert!(!tile_active(&stale));

    let icons = [
        (D::Light, "on", json!({}), IconKind::Lightbulb),
        (D::Switch, "on", json!({}), IconKind::Power),
        (D::Scene, "unknown", json!({}), IconKind::Play),
        (
            D::Sensor,
            "21",
            json!({"device_class": "temperature"}),
            IconKind::Thermometer,
        ),
        (
            D::Sensor,
            "70",
            json!({"unit_of_measurement": "°F"}),
            IconKind::Thermometer,
        ),
        (
            D::Sensor,
            "40",
            json!({"unit_of_measurement": "%"}),
            IconKind::Sensor,
        ),
        (D::BinarySensor, "off", json!({}), IconKind::BinarySensor),
        (D::Fan, "on", json!({}), IconKind::Fan),
        (D::Lock, "unlocked", json!({}), IconKind::LockOpen),
        (D::Lock, "locked", json!({}), IconKind::Lock),
        (D::MediaPlayer, "off", json!({}), IconKind::MediaPlayer),
        (D::Camera, "idle", json!({}), IconKind::Camera),
        (
            D::AlarmControlPanel,
            "disarmed",
            json!({}),
            IconKind::Shield,
        ),
        (D::Script, "off", json!({}), IconKind::Script),
        (D::Automation, "on", json!({}), IconKind::Automation),
        (D::InputBoolean, "on", json!({}), IconKind::ToggleSwitch),
        (D::InputButton, "unknown", json!({}), IconKind::Button),
        (D::Number, "1", json!({}), IconKind::Number),
        (D::InputSelect, "a", json!({}), IconKind::Select),
    ];
    for (domain, state, attributes, icon) in icons {
        assert_eq!(
            entity_icon(&tile(domain, state, attributes)),
            icon,
            "{domain:?}"
        );
    }
    let mut custom = tile(D::Switch, "on", json!({}));
    for (name, icon) in [
        ("mdi:lightbulb", IconKind::Lightbulb),
        ("mdi:power", IconKind::Power),
        ("mdi:play", IconKind::Play),
        ("mdi:thermometer", IconKind::Thermometer),
        ("mdi:checkbox-blank-circle-outline", IconKind::BinarySensor),
    ] {
        custom.icon = Some(name.to_owned());
        assert_eq!(entity_icon(&custom), icon, "{name}");
    }
}

#[test]
fn detail_attributes_only_list_values_not_already_represented() {
    let fan = tile(
        EntityDomain::Fan,
        "on",
        json!({"brightness": 255, "percentage": 66.6, "unit_of_measurement": "%"}),
    );
    assert_eq!(detail_attributes(&fan, false), "");
    let light = tile(
        EntityDomain::Light,
        "on",
        json!({"brightness": 153, "supported_color_modes": ["brightness"]}),
    );
    assert_eq!(detail_attributes(&light, false), "");
    let sensor = tile(
        EntityDomain::Sensor,
        "21.4",
        json!({"unit_of_measurement": "°C"}),
    );
    assert_eq!(detail_attributes(&sensor, false), "");
    let switch = tile(EntityDomain::Switch, "on", json!({"battery_level": 66.6}));
    assert_eq!(detail_attributes(&switch, false), "Battery: 67%");
    assert_eq!(detail_attributes(&switch, true), "Batterie: 67%");
}

#[test]
fn slider_details_separate_state_from_the_single_numeric_value() {
    let mut light = tile(
        EntityDomain::Light,
        "on",
        json!({"brightness": 153, "supported_color_modes": ["brightness"]}),
    );
    assert_eq!(detail_value(&light, false), "On");
    assert_eq!(detail_value(&light, true), "Ein");
    light.is_stale = true;
    assert_eq!(detail_value(&light, false), "Last known · On");

    let off = tile(
        EntityDomain::Light,
        "off",
        json!({"supported_color_modes": ["brightness"]}),
    );
    assert_eq!(detail_value(&off, false), "Off");

    let number = tile(
        EntityDomain::Number,
        "21.5",
        json!({"min": 10, "max": 30, "step": 0.5, "unit_of_measurement": "°C"}),
    );
    assert_eq!(detail_value(&number, false), "");
    let sensor = tile(
        EntityDomain::Sensor,
        "21.5",
        json!({"unit_of_measurement": "°C"}),
    );
    assert_eq!(detail_value(&sensor, false), "21.5 °C");
}

#[test]
fn detail_controls_per_domain() {
    use EntityDomain as D;
    let light = tile(
        D::Light,
        "on",
        json!({"supported_color_modes": ["brightness"], "brightness": 51}),
    );
    assert!(light_supports_brightness(&light));
    let view = detail_view(&light, false);
    assert_eq!(view.control, "slider");
    let slider = view.slider.expect("slider");
    assert_eq!(
        (slider.action, slider.min, slider.value),
        ("brightness", 1.0, 20.0)
    );
    let off = tile(D::Light, "off", json!({"supported_color_modes": ["hs"]}));
    assert_eq!(detail_view(&off, false).slider.map(|s| s.value), Some(0.0));
    let full = tile(
        D::Light,
        "on",
        json!({"supported_color_modes": ["color_temp"]}),
    );
    assert_eq!(
        detail_view(&full, false).slider.map(|s| s.value),
        Some(100.0)
    );
    let onoff = tile(D::Light, "on", json!({"supported_color_modes": ["onoff"]}));
    assert!(!light_supports_brightness(&onoff));
    assert_eq!(detail_view(&onoff, false).control, "none");
    assert!(light_supports_brightness(&tile(
        D::Light,
        "on",
        json!({"brightness": 3})
    )));
    assert!(!light_supports_brightness(&tile(
        D::Switch,
        "on",
        json!({"brightness": 3})
    )));

    let fan = tile(
        D::Fan,
        "on",
        json!({"percentage": 50, "percentage_step": 25}),
    );
    let Control::Slider(slider) = detail_controls(&fan) else {
        panic!("fan slider");
    };
    assert_eq!(
        (slider.action, slider.step, slider.value),
        ("fan", 25.0, 50.0)
    );
    assert_eq!(
        detail_controls(&tile(D::Fan, "on", json!({}))),
        Control::None
    );

    let number = tile(
        D::InputNumber,
        "150",
        json!({"min": 0, "max": 100, "step": 0.5, "unit_of_measurement": "W"}),
    );
    let view = detail_view(&number, false);
    let slider = view.slider.expect("number slider");
    assert_eq!(
        (slider.value, slider.step, slider.unit.as_str()),
        (100.0, 0.5, "W")
    );
    let defaults = tile(D::Number, "abc", json!({"step": 0}));
    let Control::Slider(slider) = detail_controls(&defaults) else {
        panic!("number slider");
    };
    assert_eq!(
        (slider.min, slider.max, slider.step, slider.value),
        (0.0, 100.0, 1.0, 0.0)
    );
    assert_eq!(
        detail_controls(&tile(D::Number, "1", json!({"min": 5, "max": 5}))),
        Control::None
    );

    let select = tile(D::Select, "Eco", json!({"options": ["Eco", "Comfort", 3]}));
    let view = detail_view(&select, true);
    assert_eq!(view.control, "options");
    assert_eq!(view.options, ["Eco", "Comfort"]);
    assert_eq!(view.selected_option, "Eco");
    assert_eq!(
        detail_view(&tile(D::InputSelect, "x", json!({})), true)
            .options
            .len(),
        0
    );

    let lock = detail_view(&tile(D::Lock, "locked", json!({})), false);
    assert_eq!((lock.control, lock.locked), ("lock", true));

    let media = detail_view(
        &tile(D::MediaPlayer, "playing", json!({"volume_level": 0.25})),
        false,
    );
    assert_eq!(
        (media.control, media.playing, media.has_volume),
        ("media", true, true)
    );
    assert_eq!(media.slider.map(|s| s.value), Some(25.0));
    let quiet = detail_view(&tile(D::MediaPlayer, "paused", json!({})), false);
    assert_eq!((quiet.playing, quiet.has_volume), (false, false));

    assert_eq!(
        detail_view(&tile(D::Camera, "idle", json!({})), false).control,
        "camera"
    );
    assert_eq!(
        detail_view(&tile(D::Automation, "on", json!({})), false).control,
        "automation"
    );
    assert_eq!(
        detail_view(&tile(D::Switch, "on", json!({})), false).control,
        "none"
    );
}

#[test]
fn alarm_details_offer_supported_modes_or_disarm() {
    use EntityDomain as D;
    let default_modes = detail_view(&tile(D::AlarmControlPanel, "disarmed", json!({})), false);
    assert_eq!(default_modes.control, "alarm");
    assert!(!default_modes.needs_code);
    assert_eq!(
        default_modes.actions,
        [
            ("arm_home".to_owned(), "Arm home".to_owned()),
            ("arm_away".to_owned(), "Arm away".to_owned()),
            ("arm_night".to_owned(), "Arm night".to_owned()),
        ]
    );
    let all = detail_view(
        &tile(
            D::AlarmControlPanel,
            "disarmed",
            json!({"supported_features": 63, "code_format": "number"}),
        ),
        true,
    );
    assert!(all.needs_code);
    let keys: Vec<_> = all.actions.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(keys, AlarmMode::ALL.map(AlarmMode::key));
    assert!(all.actions.iter().all(|(_, label)| !label.is_empty()));
    let armed = detail_view(
        &tile(
            D::AlarmControlPanel,
            "armed_away",
            json!({"code_format": null}),
        ),
        true,
    );
    assert!(!armed.needs_code);
    assert_eq!(
        armed.actions,
        [("disarm".to_owned(), "Deaktivieren".to_owned())]
    );
    for key in [
        "arm_home",
        "arm_away",
        "arm_night",
        "arm_vacation",
        "arm_custom_bypass",
        "disarm",
    ] {
        assert_ne!(action_label(key, false), action_label(key, true), "{key}");
    }
    assert_eq!(action_label("other", false), "");
}

#[test]
fn pairing_codes_are_read_in_groups_of_three() {
    assert_eq!(spaced_code("360971"), "360 971");
    assert_eq!(spaced_code("000042"), "000 042");
    for odd in ["", "12345", "1234567", "12a456", "１２３４５６"] {
        assert_eq!(spaced_code(odd), odd);
    }
}

#[test]
fn the_chosen_theme_decides_the_screens_before_home_assistant_answers() {
    for daylight in [None, Some(true), Some(false)] {
        assert!(dark_mode("dark", daylight));
        assert!(!dark_mode("light", daylight));
        assert!(!dark_mode("unknown", daylight));
    }
    assert!(!dark_mode("automatic", None), "no sun report yet: light");
    assert!(!dark_mode("automatic", Some(true)));
    assert!(dark_mode("automatic", Some(false)));
}

#[test]
fn new_domain_states_keep_their_meaning_and_precision() {
    for (domain, raw, attributes, english, german) in [
        (
            EntityDomain::Sensor,
            "21.456",
            json!({"display_precision":1,"unit_of_measurement":"°C"}),
            "21.5 °C",
            "21,5 °C",
        ),
        (
            EntityDomain::BinarySensor,
            "on",
            json!({"device_class":"lock"}),
            "Unlocked",
            "Entriegelt",
        ),
        (
            EntityDomain::BinarySensor,
            "off",
            json!({"device_class":"update"}),
            "Up to date",
            "Aktuell",
        ),
        (
            EntityDomain::BinarySensor,
            "on",
            json!({"device_class":"running"}),
            "Running",
            "Läuft",
        ),
        (EntityDomain::Script, "on", json!({}), "Running", "Läuft"),
        (EntityDomain::Weather, "rainy", json!({}), "Rain", "Regen"),
        (
            EntityDomain::Cover,
            "opening",
            json!({}),
            "Opening…",
            "Wird geöffnet …",
        ),
    ] {
        let tile = tile(domain, raw, attributes);
        assert_eq!(state_value(&tile, false), english);
        assert_eq!(state_value(&tile, true), german);
    }
}
