use njorun_app_core::settings::{AppliedSettings, Settings};

#[test]
fn defaults_match_ha_and_only_supported_values_are_accepted() {
    let settings = Settings::default();
    assert!(settings.is_valid(&["portrait", "landscape"]));
    assert_eq!(settings.home_view, "mixed");
    assert_eq!(settings.color_scheme, "njorun");
    assert_eq!(settings.idle_timeout, 300);
    assert_eq!(settings.update_time, "02:00");
    assert_eq!(settings.update_policy, "automatic");
    let mut invalid = settings.clone();
    invalid.brightness = 101;
    assert!(!invalid.is_valid(&["portrait"]));
    invalid = settings.clone();
    invalid.orientation = "landscape".into();
    assert!(!invalid.is_valid(&["portrait"]));
    invalid = settings.clone();
    invalid.favorites = vec!["light.a".into(), "light.a".into()];
    assert!(!invalid.is_valid(&["portrait"]));
    for time in ["24:00", "02:60", "2:00", "ab:cd", "１２:00"] {
        invalid = settings.clone();
        invalid.update_time = time.into();
        assert!(!invalid.is_valid(&["portrait"]));
    }
}

#[test]
fn stale_or_conflicting_revision_never_replaces_applied_state() {
    let applied = AppliedSettings {
        revision: 3,
        settings: Settings::default(),
    };
    assert_eq!(
        applied.candidate(3, applied.settings.clone(), &["portrait"]),
        Some(applied.clone())
    );
    assert_eq!(
        applied.candidate(2, applied.settings.clone(), &["portrait"]),
        None
    );
    let changed = Settings {
        brightness: 30,
        ..Settings::default()
    };
    assert_eq!(applied.candidate(3, changed.clone(), &["portrait"]), None);
    let next = applied
        .candidate(4, changed, &["portrait"])
        .expect("valid candidate");
    assert_eq!(applied.settings.brightness, 80);
    assert_eq!(next.settings.brightness, 30);
}

#[test]
fn ha_and_rust_default_schemas_do_not_diverge() {
    let settings = Settings::default();
    let json = serde_json::to_value(&settings).expect("serialize");
    let shared: serde_json::Value = serde_json::from_str(include_str!(
        "../../../protocol/fixtures/default-settings.json"
    ))
    .expect("shared fixture");
    assert_eq!(json, shared);
    let decoded: Settings = serde_json::from_value(json.clone()).expect("decode");
    assert_eq!(decoded, settings);
    let mut unknown = json;
    unknown["unknown"] = true.into();
    assert!(serde_json::from_value::<Settings>(unknown).is_err());
}

#[test]
fn ha_and_rust_agree_on_shared_validation_cases() {
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../protocol/fixtures/settings-cases.json"
    ))
    .expect("shared cases");
    for case in cases {
        let mut value = serde_json::to_value(Settings::default()).expect("defaults");
        for (key, field) in case["patch"].as_object().expect("patch") {
            value[key] = field.clone();
        }
        let accepted = serde_json::from_value::<Settings>(value)
            .is_ok_and(|settings| settings.is_valid(&["portrait", "landscape"]));
        assert_eq!(
            accepted,
            case["valid"].as_bool().expect("expectation"),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn the_interface_size_stays_within_its_range_and_old_records_get_the_default() {
    for scale in [75_u8, 100, 200] {
        let settings = Settings {
            ui_scale: scale,
            ..Settings::default()
        };
        assert!(settings.is_valid(&["portrait"]), "{scale}");
    }
    for scale in [0_u8, 74, 201, 255] {
        let settings = Settings {
            ui_scale: scale,
            ..Settings::default()
        };
        assert!(!settings.is_valid(&["portrait"]), "{scale}");
    }
    // A record stored before the setting existed has no such key.
    let mut stored = serde_json::to_value(Settings::default()).expect("defaults");
    stored.as_object_mut().expect("object").remove("ui_scale");
    let decoded: Settings = serde_json::from_value(stored).expect("old record");
    assert_eq!(decoded.ui_scale, 100);
}

#[test]
fn old_records_get_the_default_color_scheme() {
    let mut stored = serde_json::to_value(Settings::default()).expect("defaults");
    stored
        .as_object_mut()
        .expect("object")
        .remove("color_scheme");
    let decoded: Settings = serde_json::from_value(stored).expect("old record");
    assert_eq!(decoded.color_scheme, "njorun");
}

#[test]
fn split_palettes_fall_back_to_legacy_and_keep_high_contrast() {
    let mut legacy = serde_json::to_value(Settings::default()).expect("defaults");
    legacy["color_scheme"] = "warm".into();
    legacy
        .as_object_mut()
        .expect("object")
        .remove("light_color_scheme");
    legacy
        .as_object_mut()
        .expect("object")
        .remove("dark_color_scheme");
    let decoded: Settings = serde_json::from_value(legacy).expect("legacy record");
    assert_eq!(decoded.light_color_scheme, "warm");
    assert_eq!(decoded.dark_color_scheme, "warm");
    assert_eq!(decoded.effective_color_scheme(false), "warm");
    assert_eq!(decoded.effective_color_scheme(true), "warm");
    assert!(!decoded.effective_increased_contrast());

    let mut legacy = serde_json::to_value(Settings::default()).expect("defaults");
    legacy["color_scheme"] = "high_contrast".into();
    legacy
        .as_object_mut()
        .expect("object")
        .remove("light_color_scheme");
    legacy
        .as_object_mut()
        .expect("object")
        .remove("dark_color_scheme");
    legacy
        .as_object_mut()
        .expect("object")
        .remove("increased_contrast");
    let decoded: Settings = serde_json::from_value(legacy).expect("legacy high contrast");
    assert_eq!(decoded.effective_color_scheme(false), "njorun");
    assert!(decoded.effective_increased_contrast());
}

#[test]
fn legacy_contrast_migration_preserves_explicit_false_and_partial_palettes() {
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../protocol/fixtures/settings-accessibility-cases.json"
    ))
    .expect("shared accessibility cases");

    for case in cases {
        let mut value = serde_json::to_value(Settings::default()).expect("defaults");
        for key in case["remove"].as_array().expect("remove list") {
            value
                .as_object_mut()
                .expect("settings object")
                .remove(key.as_str().expect("field name"));
        }
        for (key, field) in case["patch"].as_object().expect("patch") {
            value[key] = field.clone();
        }

        let decoded = serde_json::from_value::<Settings>(value);
        if case["valid"].as_bool() == Some(false) {
            assert!(
                decoded.is_err()
                    || decoded.is_ok_and(|settings| !settings.is_valid(&["portrait", "landscape"])),
                "{}",
                case["name"]
            );
            continue;
        }
        let settings = decoded.expect("valid accessibility case");
        assert!(
            settings.is_valid(&["portrait", "landscape"]),
            "{} is accepted by Rust validation",
            case["name"]
        );
        assert_eq!(
            settings.effective_color_scheme(false),
            case["light"].as_str().expect("light palette"),
            "{} light palette",
            case["name"]
        );
        assert_eq!(
            settings.effective_color_scheme(true),
            case["dark"].as_str().expect("dark palette"),
            "{} dark palette",
            case["name"]
        );
        assert_eq!(
            settings.effective_increased_contrast(),
            case["contrast"].as_bool().expect("contrast value"),
            "{} contrast",
            case["name"]
        );
        let round_trip = serde_json::to_value(settings).expect("serialize normalized settings");
        assert!(
            ["njorun", "graphite", "warm", "porcelain"].contains(
                &round_trip["light_color_scheme"]
                    .as_str()
                    .expect("light field")
            ),
            "{} serializes a valid light palette",
            case["name"]
        );
        assert!(
            ["njorun", "graphite", "warm", "porcelain"].contains(
                &round_trip["dark_color_scheme"]
                    .as_str()
                    .expect("dark field")
            ),
            "{} serializes a valid dark palette",
            case["name"]
        );
        assert_eq!(
            round_trip["increased_contrast"].as_bool(),
            Some(case["contrast"].as_bool().expect("contrast value")),
            "{} serializes normalized contrast",
            case["name"]
        );
    }
}

#[test]
fn explicit_palette_and_accessibility_settings_take_precedence() {
    let settings = Settings {
        color_scheme: "high_contrast".into(),
        light_color_scheme: "porcelain".into(),
        dark_color_scheme: "graphite".into(),
        increased_contrast: false,
        reduced_motion: true,
        ..Settings::default()
    };
    assert_eq!(settings.effective_color_scheme(false), "porcelain");
    assert_eq!(settings.effective_color_scheme(true), "graphite");
    assert!(!settings.effective_increased_contrast());
    assert!(settings.reduced_motion);

    let explicit = Settings {
        increased_contrast: true,
        ..settings
    };
    assert!(explicit.effective_increased_contrast());
}

#[test]
fn aliases_and_room_icons_are_validated_and_inferred() {
    let mut settings = Settings::default();
    settings
        .entity_names
        .insert("light.kitchen".into(), "Island".into());
    settings
        .room_names
        .insert("kitchen".into(), "Cooking".into());
    settings
        .room_icons
        .insert("kitchen".into(), "kitchen".into());
    assert!(settings.is_valid(&["portrait"]));
    assert_eq!(
        settings.entity_name("light.kitchen", "Kitchen light"),
        "Island"
    );
    assert_eq!(settings.room_name("kitchen", "Kitchen"), "Cooking");
    assert_eq!(settings.room_icon("kitchen", "Kitchen"), "kitchen");
    assert_eq!(settings.room_icon("bedroom", "Bedroom"), "bed");
    assert_eq!(settings.room_icon("unassigned", "Other"), "home");
    settings
        .room_names
        .insert("guest_room".into(), "Bedroom".into());
    assert_eq!(settings.room_icon("guest_room", "Room"), "bed");

    settings.room_icons.insert("kitchen".into(), "lamp".into());
    assert!(!settings.is_valid(&["portrait"]));

    let too_many = Settings {
        room_names: (0..=njorun_app_core::MAX_AREA_COUNT)
            .map(|index| (format!("r{index}"), "Room".to_owned()))
            .collect(),
        ..Settings::default()
    };
    assert!(!too_many.is_valid(&["portrait"]));
}
