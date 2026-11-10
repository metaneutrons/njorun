use njorun_app_core::{
    display::{DisplayController, TouchDisposition},
    settings::Settings,
};

#[test]
fn timeout_blanks_display_and_first_touch_only_wakes() {
    let settings = Settings {
        idle_timeout: 60,
        ..Settings::default()
    };
    let mut display = DisplayController::new(100);
    assert_eq!(display.state(60_099, &settings).brightness, 80);
    assert!(display.state(60_100, &settings).is_off);
    assert_eq!(display.state(60_100, &settings).brightness, 0);
    assert_eq!(display.touch(60_100, &settings), TouchDisposition::WakeOnly);
    assert_eq!(display.state(60_101, &settings).brightness, 80);
    assert_eq!(
        display.touch(60_102, &settings),
        TouchDisposition::Interactive
    );
    assert!(!display.state(120_101, &settings).is_off);
    assert!(display.state(120_102, &settings).is_off);
}

#[test]
fn ha_settings_change_timeout_brightness_and_disabled_behavior() {
    let display = DisplayController::new(0);
    let mut settings = Settings {
        idle_timeout: 10,
        brightness: 25,
        ..Settings::default()
    };
    assert!(display.state(10_000, &settings).is_off);
    settings.screensaver = "disabled".into();
    assert_eq!(display.state(u64::MAX, &settings).brightness, 25);
    settings.screensaver = "display_off".into();
    settings.idle_timeout = 60;
    assert!(!display.state(10_000, &settings).is_off);
    settings.brightness = 90;
    assert_eq!(display.state(10_000, &settings).brightness, 90);
}

#[test]
fn motion_activity_wakes_without_generating_a_touch_action() {
    let settings = Settings {
        idle_timeout: 60,
        ..Settings::default()
    };
    let mut display = DisplayController::new(0);
    assert!(display.state(60_000, &settings).is_off);
    display.activity(60_000);
    assert!(!display.state(60_001, &settings).is_off);
    assert_eq!(
        display.touch(60_002, &settings),
        TouchDisposition::Interactive
    );
    assert!(!display.state(0, &settings).is_off);
}

#[test]
fn clock_idle_dims_once_and_wake_touch_is_consumed() {
    let settings = Settings {
        idle_timeout: 10,
        screensaver: "clock".into(),
        idle_brightness: 12,
        ..Settings::default()
    };
    let mut display = DisplayController::new(0);
    let idle = display.state(10_000, &settings);
    assert!(!idle.is_off);
    assert!(idle.show_clock);
    assert_eq!(idle.brightness, 12);
    assert_eq!(display.touch(10_001, &settings), TouchDisposition::WakeOnly);
    assert!(!display.state(10_002, &settings).show_clock);
}

#[test]
fn manual_off_ignores_motion_and_brightness_until_explicit_wake() {
    let mut display = DisplayController::new(0);
    let mut settings = Settings::default();
    display.turn_off();
    display.activity(100);
    settings.brightness = 99;
    settings.screensaver = "disabled".into();
    assert!(display.state(101, &settings).is_off);
    assert_eq!(display.touch(102, &settings), TouchDisposition::WakeOnly);
    assert_eq!(display.state(103, &settings).brightness, 99);
    display.turn_off();
    display.wake(104);
    assert!(!display.state(105, &settings).is_off);
}

#[test]
fn live_backlight_commands_override_the_setting_until_a_configuration() {
    let settings = Settings {
        brightness: 80,
        idle_timeout: 60,
        ..Settings::default()
    };
    let mut display = DisplayController::new(0);
    display.backlight(true, Some(30), 1_000);
    assert_eq!(display.state(1_001, &settings).brightness, 30);
    assert_eq!(settings.brightness, 80, "the setting is untouched");

    display.backlight(false, None, 2_000);
    assert!(display.state(2_001, &settings).is_off);
    assert_eq!(display.state(2_001, &settings).brightness, 0);
    assert_eq!(display.touch(3_000, &settings), TouchDisposition::WakeOnly);
    assert_eq!(
        display.state(3_001, &settings).brightness,
        30,
        "the level stays"
    );

    display.backlight(false, None, 4_000);
    display.backlight(true, None, 5_000);
    assert!(
        !display.state(5_001, &settings).is_off,
        "on ends manual-off"
    );
    assert_eq!(display.state(5_001, &settings).brightness, 30);

    display.reset_live(6_000);
    assert_eq!(
        display.state(6_001, &settings).brightness,
        80,
        "back to the setting"
    );
}

#[test]
fn a_wake_command_restarts_the_idle_interval_and_leaves_manual_off() {
    let settings = Settings {
        idle_timeout: 60,
        ..Settings::default()
    };
    let mut display = DisplayController::new(0);
    assert!(display.state(60_000, &settings).is_off);
    display.wake(60_000);
    assert!(!display.state(60_001, &settings).is_off);
    display.backlight(false, None, 61_000);
    display.wake(62_000);
    assert!(!display.state(62_001, &settings).is_off);
}
