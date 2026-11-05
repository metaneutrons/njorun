use njorun_app_core::settings::{AppliedSettings, Settings};

#[test]
fn defaults_match_ha_and_only_supported_values_are_accepted() {
    let settings = Settings::default();
    assert!(settings.is_valid(&["portrait", "landscape"]));
    assert_eq!(settings.home_view, "rooms");
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
