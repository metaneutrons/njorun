//! Exercises the generated entity controls through the same adapter and Slint components used
//! by the simulator, then writes compact review frames to `target/entity-controls-review`.
#![recursion_limit = "256"]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::PathBuf;
use std::rc::Rc;

use njorun_app_core::controls;
use njorun_app_core::{DetailAction, EntityDomain, EntityTile, HaEntityState};
use serde_json::{Value, json};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::{ComponentHandle, Model, ModelRc, PhysicalSize, SharedString, VecModel};

slint::slint! {
    import {
        MainWindow,
        DetailsItem,
        EntityIcon,
        ControlItem,
        InputItem,
        ResultItem,
    } from "../ui/main.slint";
    export {
        MainWindow,
        DetailsItem,
        EntityIcon,
        ControlItem,
        InputItem,
        ResultItem
    }
}

#[path = "../src/control_adapter.rs"]
mod control_adapter;

struct TestPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }
}

fn fixture(domain: EntityDomain, state_name: &str, features: u64, stale: bool) -> EntityTile {
    let entity_id = format!("{}.sample", domain.service_domain());
    let attributes = json!({
        "supported_features": features,
        "supported_color_modes": ["color_temp", "rgb", "hs"],
        "brightness": 173,
        "min_color_temp_kelvin": 2000,
        "max_color_temp_kelvin": 6500,
        "color_temp_kelvin": 3200,
        "effect_list": ["pulse", "rainbow"],
        "effect": "pulse",
        "percentage": 45,
        "preset_modes": ["auto", "sleep"],
        "preset_mode": "auto",
        "oscillating": false,
        "direction": "forward",
        "code_format": "number",
        "code_arm_required": false,
        "supported_modes": ["heat", "cool"],
        "hvac_modes": ["off", "heat", "cool"],
        "preset_modes": ["eco", "comfort"],
        "fan_modes": ["auto", "low"],
        "swing_modes": ["off", "vertical"],
        "min_temp": 7,
        "max_temp": 35,
        "target_temp_step": 0.5,
        "target_temp_low": 18,
        "target_temp_high": 22,
        "current_temperature": 20,
        "min_humidity": 20,
        "max_humidity": 80,
        "humidity": 45,
        "current_position": 40,
        "current_tilt_position": 20,
        "available_modes": ["normal", "sleep"],
        "operation_list": ["heat", "eco"],
        "available_tones": ["alarm", "chime"],
        "fan_speed_list": ["low", "high"],
        "options": ["eco", "boost"],
        "min": 0,
        "max": 100,
        "step": 1,
        "unit_of_measurement": "%",
        "source_list": ["HDMI 1", "TV"],
        "sound_mode_list": ["movie", "music"],
        "volume_level": 0.35,
        "is_volume_muted": false,
        "shuffle": true,
        "repeat": "all",
        "media_position": 130,
        "media_duration": 3600,
        "media_position_updated_at": "2026-11-14T10:00:00Z",
        "media_title": "A long but realistic media title",
        "media_artist": "Sample Artist",
        "media_album_name": "Sample Album",
        "entity_picture": "/api/media_player_proxy/media_player.sample?token=not-used",
        "current": 1,
        "minimum": 0,
        "maximum": 100,
        "available_tones": ["alarm", "chime"],
        "has_date": true,
        "has_time": true,
        "source": "HDMI 1",
        "sound_mode": "movie",
        "code_format": "number",
        "state": "on",
        "app_name": "Njörun test device",
        "friendly_name": "Sample device"
    });
    let attributes = attributes
        .as_object()
        .expect("object fixture")
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    let name = if domain == EntityDomain::MediaPlayer {
        "Living room speakers".to_owned()
    } else {
        format!("{} sample", domain.service_domain())
    };
    EntityTile {
        entity_id: entity_id.clone(),
        name,
        icon: None,
        domain,
        state: Some(HaEntityState {
            entity_id,
            state: state_name.to_owned(),
            attributes,
            last_updated: Some("2026-11-14T10:00:00Z".to_owned()),
        }),
        is_stale: stale,
        command_enabled: true,
        action_feedback: None,
    }
}

const fn icon(domain: EntityDomain) -> EntityIcon {
    use EntityDomain as D;
    match domain {
        D::Light => EntityIcon::Lightbulb,
        D::Switch | D::InputBoolean => EntityIcon::ToggleSwitch,
        D::Scene => EntityIcon::Play,
        D::Sensor
        | D::AirQuality
        | D::Event
        | D::Image
        | D::GeoLocation
        | D::ImageProcessing
        | D::Group
        | D::Counter
        | D::Schedule
        | D::Todo => EntityIcon::Sensor,
        D::BinarySensor => EntityIcon::BinarySensor,
        D::Fan => EntityIcon::Fan,
        D::Lock => EntityIcon::Lock,
        D::MediaPlayer => EntityIcon::MediaPlayer,
        D::Camera => EntityIcon::Camera,
        D::AlarmControlPanel => EntityIcon::Shield,
        D::Script => EntityIcon::Script,
        D::Automation => EntityIcon::Automation,
        D::Button | D::InputButton => EntityIcon::Button,
        D::Number | D::InputNumber => EntityIcon::Number,
        D::Select | D::InputSelect => EntityIcon::Select,
        D::Climate => EntityIcon::Climate,
        D::Cover => EntityIcon::Cover,
        D::Humidifier | D::WaterHeater | D::Valve => EntityIcon::Water,
        D::Vacuum | D::LawnMower => EntityIcon::Robot,
        D::Weather | D::Sun => EntityIcon::Weather,
        D::Calendar => EntityIcon::Calendar,
        D::DeviceTracker | D::Person | D::Zone => EntityIcon::Presence,
        D::Timer | D::Date | D::Datetime | D::Time | D::InputDatetime => EntityIcon::Timer,
        D::Update => EntityIcon::Update,
        D::AssistSatellite | D::Conversation | D::Stt | D::Tts | D::WakeWord | D::AiTask => {
            EntityIcon::Voice
        }
        D::Remote | D::Infrared | D::RadioFrequency => EntityIcon::Remote,
        D::Text | D::InputText | D::Notify => EntityIcon::Text,
        D::Siren => EntityIcon::Power,
    }
}

fn details(tile: &EntityTile) -> DetailsItem {
    DetailsItem {
        entity_id: tile.entity_id.clone().into(),
        name: tile.name.clone().into(),
        value: tile
            .state
            .as_ref()
            .map_or("", |state| state.state.as_str())
            .into(),
        update_time: "14.11.2026 · 10:00".into(),
        reason: "".into(),
        attributes: "".into(),
        icon: icon(tile.domain),
        unavailable: !tile.is_available(),
        can_toggle: false,
        controls_enabled: !tile.is_stale && tile.is_available(),
        control: "".into(),
        slider_action: "".into(),
        slider_min: 0.0,
        slider_max: 100.0,
        slider_step: 1.0,
        slider_value: 0.0,
        slider_unit: "".into(),
        options: ModelRc::default(),
        selected_option: "".into(),
        locked: false,
        playing: tile
            .state
            .as_ref()
            .is_some_and(|state| state.state == "playing"),
        has_volume: true,
        needs_code: false,
        actions: ModelRc::default(),
        camera_image: slint::Image::default(),
        camera_loading: false,
    }
}

fn render(
    ui: &MainWindow,
    window: &MinimalSoftwareWindow,
    width: u32,
    height: u32,
) -> Vec<Rgb565Pixel> {
    window.set_size(PhysicalSize::new(width, height));
    window.request_redraw();
    let mut pixels = vec![Rgb565Pixel(0); (width * height) as usize];
    let drawn = window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, width as usize);
    });
    assert!(drawn, "Slint did not draw {width}x{height}");
    ui.window().request_redraw();
    pixels
}

fn assert_header_region_unchanged(before: &[Rgb565Pixel], after: &[Rgb565Pixel], width: u32) {
    const HEADER_ROWS: usize = 100;
    let width = usize::try_from(width).expect("frame width fits usize");
    let header_pixels = width * HEADER_ROWS;
    assert!(before.len() >= header_pixels && after.len() >= header_pixels);
    assert_eq!(
        &before[..header_pixels],
        &after[..header_pixels],
        "the Details header must stay fixed while its contents scroll"
    );
}

fn swipe_up(ui: &MainWindow, x: f32, start_y: f32, end_y: f32) {
    use slint::platform::{PointerEventButton, WindowEvent};
    let at = |y| slint::LogicalPosition::new(x, y);
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position: at(start_y),
        button: PointerEventButton::Left,
    });
    for step in 1..=8_u8 {
        let fraction = f32::from(step) / 8.0;
        let y = (end_y - start_y).mul_add(fraction, start_y);
        ui.window()
            .dispatch_event(WindowEvent::PointerMoved { position: at(y) });
        slint::platform::update_timers_and_animations();
    }
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position: at(end_y),
        button: PointerEventButton::Left,
    });
    slint::platform::update_timers_and_animations();
}

fn click_at(ui: &MainWindow, x: f32, y: f32) {
    use slint::platform::{PointerEventButton, WindowEvent};
    let position = slint::LogicalPosition::new(x, y);
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
    slint::platform::update_timers_and_animations();
}

fn save_png(path: &PathBuf, pixels: &[Rgb565Pixel], width: u32, height: u32) {
    let mut rgb = Vec::with_capacity(pixels.len() * 3);
    for pixel in pixels {
        let value = pixel.0;
        let red = u32::from((value >> 11) & 0x1f) * 255 / 31;
        let green = u32::from((value >> 5) & 0x3f) * 255 / 63;
        let blue = u32::from(value & 0x1f) * 255 / 31;
        rgb.extend_from_slice(&[
            u8::try_from(red).expect("red channel is 8-bit"),
            u8::try_from(green).expect("green channel is 8-bit"),
            u8::try_from(blue).expect("blue channel is 8-bit"),
        ]);
    }
    let file = File::create(path).expect("create review PNG");
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder
        .write_header()
        .expect("write PNG header")
        .write_image_data(&rgb)
        .expect("write PNG pixels");
}

fn input(key: &str, kind: &str, value: &str, required: bool) -> InputItem {
    InputItem {
        key: key.into(),
        label: key.into(),
        kind: kind.into(),
        value: value.into(),
        options: ModelRc::new(VecModel::from(Vec::<SharedString>::new())),
        required,
    }
}

fn all_domains() -> Vec<EntityDomain> {
    use EntityDomain as D;
    vec![
        D::Light,
        D::Switch,
        D::Scene,
        D::Sensor,
        D::BinarySensor,
        D::Fan,
        D::Lock,
        D::MediaPlayer,
        D::Camera,
        D::AlarmControlPanel,
        D::Script,
        D::Automation,
        D::InputBoolean,
        D::Button,
        D::InputButton,
        D::Number,
        D::InputNumber,
        D::Select,
        D::InputSelect,
        D::Climate,
        D::Cover,
        D::Humidifier,
        D::WaterHeater,
        D::Valve,
        D::Vacuum,
        D::LawnMower,
        D::Siren,
        D::Remote,
        D::Weather,
        D::Calendar,
        D::Todo,
        D::DeviceTracker,
        D::AirQuality,
        D::Event,
        D::Image,
        D::GeoLocation,
        D::Text,
        D::Date,
        D::Datetime,
        D::Time,
        D::Update,
        D::AssistSatellite,
        D::Conversation,
        D::Stt,
        D::Tts,
        D::WakeWord,
        D::AiTask,
        D::Notify,
        D::ImageProcessing,
        D::Infrared,
        D::RadioFrequency,
        D::Person,
        D::Zone,
        D::Sun,
        D::Group,
        D::Counter,
        D::Timer,
        D::Schedule,
        D::InputText,
        D::InputDatetime,
    ]
}

const fn state_for(domain: EntityDomain) -> &'static str {
    match domain {
        EntityDomain::MediaPlayer => "playing",
        EntityDomain::AlarmControlPanel => "disarmed",
        EntityDomain::Lock => "unlocked",
        EntityDomain::Climate => "heat",
        EntityDomain::Cover => "open",
        EntityDomain::Button | EntityDomain::InputButton => "unknown",
        EntityDomain::Timer => "active",
        _ => "on",
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One renderer session covers the complete entity matrix.
fn generated_entity_controls_render_and_preserve_action_values() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform {
        window: window.clone(),
    }))
    .expect("set offscreen renderer");
    let ui = MainWindow::new().expect("create simulator window");
    ui.set_boot_screen(false);
    ui.set_details_open(true);
    ui.set_details_fullscreen(true);
    ui.set_editing_controls(false);
    ui.show().expect("show simulator window");
    control_adapter::virtual_key(&ui, "left");

    // Every original and added domain goes through the actual renderer adapter. The fixture
    // supplies broad but bounded attributes so capability-dependent rows are generated.
    let mut rendered_domains = 0;
    let mut controls_seen = 0;
    for domain in all_domains() {
        let tile = fixture(domain, state_for(domain), u64::MAX, false);
        let specs = controls::for_entity(&tile, false);
        controls_seen += specs.len();
        control_adapter::render(&ui, Some(&tile), false);
        ui.set_details_data(details(&tile));
        let transport = ui.get_transport_controls();
        let entity = ui.get_entity_controls();
        assert!(transport.row_count() + entity.row_count() <= 80);
        let pixels = render(&ui, &window, 800, 1280);
        assert_ne!(pixels.len(), 0);
        rendered_domains += 1;
    }
    assert_eq!(rendered_domains, 60, "rendered all current entity domains");
    assert!(
        controls_seen >= 50,
        "rendered generated controls across domains"
    );

    let mut media = fixture(EntityDomain::MediaPlayer, "playing", u64::MAX, false);
    let updated_at = (chrono::Utc::now() - chrono::Duration::seconds(8)).to_rfc3339();
    media.state.as_mut().unwrap().attributes.insert(
        "media_position_updated_at".to_owned(),
        Value::String(updated_at),
    );
    ui.set_details_data(details(&media));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let expected_position = njorun_app_core::media::view(&media, now)
        .elapsed_seconds
        .expect("playing media position");
    control_adapter::render(&ui, Some(&media), false);
    let controls_model: ModelRc<ControlItem> = ui.get_entity_controls();
    let media_keys = (0..controls_model.row_count())
        .filter_map(|index| controls_model.row_data(index))
        .map(|control| control.key.to_string())
        .collect::<Vec<_>>();
    for key in [
        "media_volume",
        "media_mute",
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
        assert!(
            media_keys.iter().any(|found| found == key),
            "UI omitted {key}"
        );
    }
    let seek_model: ModelRc<ControlItem> = ui.get_seek_controls();
    assert_eq!(seek_model.row_count(), 1, "seek control has its own model");
    let seek = seek_model.row_data(0).unwrap();
    assert_eq!(seek.key.as_str(), "media_seek");
    assert_eq!(seek.value.as_str(), "130.0");
    assert!(seek.enabled);
    let live_position = ui.get_media_position();
    assert!(
        (f64::from(live_position) - expected_position).abs() < 1.0,
        "expected position near {expected_position}, got {live_position}"
    );
    assert!(ui.get_media_title().contains("realistic media title"));
    assert_eq!(ui.get_media_artist().as_str(), "Sample Artist");
    assert_eq!(ui.get_media_album().as_str(), "Sample Album");

    // Disabled, stale, unavailable, off, and no-feature snapshots each have their own UI state.
    let mut disabled = fixture(EntityDomain::MediaPlayer, "playing", u64::MAX, false);
    disabled.command_enabled = false;
    control_adapter::render(&ui, Some(&disabled), false);
    assert_eq!(ui.get_transport_controls().row_count(), 0);
    assert_eq!(ui.get_seek_controls().row_count(), 0);
    let stale = fixture(EntityDomain::MediaPlayer, "playing", u64::MAX, true);
    control_adapter::render(&ui, Some(&stale), false);
    assert_eq!(ui.get_transport_controls().row_count(), 0);
    assert_eq!(ui.get_seek_controls().row_count(), 0);
    let unavailable = fixture(EntityDomain::MediaPlayer, "unavailable", u64::MAX, false);
    control_adapter::render(&ui, Some(&unavailable), false);
    assert_eq!(ui.get_transport_controls().row_count(), 0);
    assert_eq!(ui.get_seek_controls().row_count(), 0);
    assert_eq!(ui.get_entity_controls().row_count(), 0);
    let off = fixture(EntityDomain::MediaPlayer, "off", 128 | 16_384 | 4, false);
    control_adapter::render(&ui, Some(&off), false);
    let off_transport = ui.get_transport_controls();
    assert_eq!(off_transport.row_count(), 1);
    assert_eq!(
        off_transport.row_data(0).unwrap().key.as_str(),
        "media_power_on"
    );
    let no_features = fixture(EntityDomain::MediaPlayer, "playing", 0, false);
    control_adapter::render(&ui, Some(&no_features), false);
    assert_eq!(ui.get_transport_controls().row_count(), 0);
    assert_eq!(ui.get_seek_controls().row_count(), 0);
    assert_eq!(ui.get_entity_controls().row_count(), 0);

    // The generated callback and shared adapter retain numeric and form values as JSON.
    let emitted = Rc::new(RefCell::new(Vec::<DetailAction>::new()));
    let emitted_by_callback = Rc::clone(&emitted);
    let media_for_callback = media.clone();
    ui.on_detail_action(move |entity_id, key, argument| {
        assert_eq!(entity_id.as_str(), media_for_callback.entity_id);
        let action = control_adapter::action(&media_for_callback, key.as_str(), argument.as_str())
            .expect("generated media control maps to an action");
        emitted_by_callback.borrow_mut().push(action);
    });
    ui.invoke_detail_action(
        media.entity_id.clone().into(),
        "entity:media_volume".into(),
        "35".into(),
    );
    assert_eq!(
        serde_json::to_value(emitted.borrow().last().unwrap()).unwrap(),
        json!({"entity":{"key":"media_volume","argument":35}})
    );
    ui.invoke_detail_action(
        media.entity_id.clone().into(),
        "entity:media_browse".into(),
        "".into(),
    );
    let browse_action = emitted.borrow().last().unwrap().clone();
    assert_eq!(
        serde_json::to_value(&browse_action).unwrap(),
        json!({"entity":{"key":"media_browse","argument":null}})
    );
    let DetailAction::Entity { key, argument } = browse_action else {
        panic!("media browse callback must keep its entity action");
    };
    let (_, browse_call) = controls::plan(&media, &key, &argument).expect("browse root is valid");
    assert_eq!(
        (browse_call.domain, browse_call.service),
        ("media_player", "browse_media")
    );
    assert_eq!(browse_call.data, json!({}));

    let submitted = Rc::new(RefCell::new(None::<DetailAction>));
    let submitted_by_callback = Rc::clone(&submitted);
    ui.on_control_submit(move |_entity_id, key, fields| {
        let argument = control_adapter::form_argument(&fields).expect("valid form values");
        *submitted_by_callback.borrow_mut() = Some(DetailAction::Entity {
            key: key.to_string(),
            argument,
        });
    });
    let form_fields = ModelRc::new(VecModel::from(vec![
        input("media_content_type", "text", "music", true),
        input("media_content_id", "text", "library/track-42", true),
        input("enqueue", "select", "replace", false),
        input("announce", "toggle", "", false),
        input("empty_optional", "text", "", false),
    ]));
    ui.invoke_control_submit(
        media.entity_id.clone().into(),
        "media_play_item".into(),
        form_fields,
    );
    let form_action = submitted.borrow().clone().expect("form callback action");
    assert_eq!(
        serde_json::to_value(form_action).unwrap(),
        json!({"entity":{"key":"media_play_item","argument":{
            "media_content_type":"music",
            "media_content_id":"library/track-42",
            "enqueue":"replace"
        }}})
    );

    // Response rows from the HA media browse command are represented by ResultItem models.
    control_adapter::render(&ui, Some(&media), false);
    control_adapter::response(
        &ui,
        &json!({"response":{"title":"Music library","children":[{
            "media_content_id":"library/track-42",
            "media_content_type":"music",
            "title":"Blue Train",
            "can_play":true,
            "can_expand":true
        }]}}),
    );
    let results: ModelRc<ResultItem> = ui.get_entity_results();
    assert_eq!(results.row_count(), 2);
    assert_eq!(results.row_data(1).unwrap().title.as_str(), "Blue Train");
    assert_eq!(results.row_data(1).unwrap().action.as_str(), "media_browse");
    assert_eq!(
        results.row_data(1).unwrap().secondary_action.as_str(),
        "media_play_item"
    );
    ui.set_entity_results(ModelRc::default());
    ui.set_entity_result_message("".into());
    ui.set_entity_error("".into());
    ui.set_showing_results(false);

    let long_response = "spoken text ".repeat(200);
    control_adapter::response(&ui, &json!({"response":{"text":long_response}}));
    let text_rows = ui.get_entity_results();
    let restored_text = (0..text_rows.row_count())
        .filter_map(|index| text_rows.row_data(index))
        .map(|row| row.subtitle.to_string())
        .collect::<String>();
    assert_eq!(restored_text, long_response);

    control_adapter::response(
        &ui,
        &json!({"response":[{"summary":"Calendar event","start":{
            "dateTime":"2026-11-14T14:30:00+01:00",
            "timeZone":"Europe/Berlin"
        }}]}),
    );
    let calendar_rows = ui.get_entity_results();
    assert!(
        calendar_rows
            .row_data(0)
            .unwrap()
            .subtitle
            .contains("2026-11-14T14:30:00+01:00")
    );
    ui.set_entity_results(ModelRc::default());
    ui.set_entity_result_message("".into());
    ui.set_showing_results(false);

    // Both display modes, all four architectural palettes, both contrast settings and both
    // languages use the actual MainWindow and software renderer.
    let palettes = ["njorun", "graphite", "warm", "porcelain"];
    for dark in [false, true] {
        for palette in palettes {
            for contrast in [false, true] {
                for german in [false, true] {
                    ui.set_dark_mode(dark);
                    ui.set_color_scheme(palette.into());
                    ui.set_increased_contrast(contrast);
                    ui.set_german(german);
                    control_adapter::render(&ui, Some(&media), german);
                    ui.set_details_data(details(&media));
                    let portrait = render(&ui, &window, 800, 1280);
                    let landscape = render(&ui, &window, 1280, 800);
                    if !dark && palette == "njorun" && !contrast && !german {
                        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                            .join("../../target/entity-controls-review");
                        fs::create_dir_all(&directory).expect("create review output directory");
                        save_png(
                            &directory.join("media-controls-portrait.png"),
                            &portrait,
                            800,
                            1280,
                        );
                        save_png(
                            &directory.join("media-controls-landscape.png"),
                            &landscape,
                            1280,
                            800,
                        );

                        // A second pair shows the lower playback/library cards that are reached
                        // by scrolling on the real display sizes.
                        for (
                            width,
                            height,
                            x,
                            start_y,
                            end_y,
                            form_x,
                            form_button_y,
                            name,
                            form_name,
                        ) in [
                            (
                                800,
                                1280,
                                760.0,
                                1160.0,
                                320.0,
                                400.0,
                                950.0,
                                "media-library-portrait.png",
                                "media-form-portrait.png",
                            ),
                            (
                                1280,
                                800,
                                1180.0,
                                700.0,
                                180.0,
                                640.0,
                                465.0,
                                "media-library-landscape.png",
                                "media-form-landscape.png",
                            ),
                        ] {
                            ui.set_showing_results(true);
                            ui.set_showing_results(false);
                            let before_scroll = render(&ui, &window, width, height);
                            let action_count = emitted.borrow().len();
                            for _ in 0..3 {
                                swipe_up(&ui, x, start_y, end_y);
                                render(&ui, &window, width, height);
                            }
                            assert_eq!(emitted.borrow().len(), action_count);
                            let lower = render(&ui, &window, width, height);
                            assert_header_region_unchanged(&before_scroll, &lower, width);
                            save_png(&directory.join(name), &lower, width, height);
                            click_at(&ui, form_x, form_button_y);
                            assert!(ui.get_editing_controls(), "form button should expand");
                            let expanded_form = render(&ui, &window, width, height);
                            save_png(&directory.join(form_name), &expanded_form, width, height);
                            click_at(&ui, form_x, form_button_y);
                            assert!(!ui.get_editing_controls(), "form button should collapse");
                        }
                    }
                }
            }
        }
    }

    // Long titles and artist names still render in both orientations and remain sourced from HA.
    let mut long_metadata = media.clone();
    let metadata_state = long_metadata.state.as_mut().unwrap();
    metadata_state.attributes.insert(
        "media_title".to_owned(),
        Value::String("Long title · ".repeat(80)),
    );
    metadata_state.attributes.insert(
        "media_artist".to_owned(),
        Value::String("Long artist · ".repeat(60)),
    );
    control_adapter::render(&ui, Some(&long_metadata), false);
    ui.set_details_data(details(&long_metadata));
    for (width, height) in [(800, 1280), (1280, 800)] {
        let pixels = render(&ui, &window, width, height);
        assert_ne!(pixels.len(), 0);
    }
}
