//! Renders the dashboard's details sheet for a media player through the software renderer,
//! as the D1001 does, to catch crashes in that UI path on the host.

use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

slint::slint! {
    import { MainWindow, DetailsItem, EntityIcon, ActionItem, TileItem, RoomItem } from "../ui/main.slint";
    export { MainWindow, DetailsItem, EntityIcon, ActionItem, TileItem, RoomItem }
}

struct TestPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }
}

fn media_details(volume: f32, title: &str) -> DetailsItem {
    DetailsItem {
        entity_id: "media_player.kitchen".into(),
        name: "Audio Küche".into(),
        value: title.into(),
        update_time: "08.11.2026 · 09:41".into(),
        reason: "".into(),
        attributes: "".into(),
        icon: EntityIcon::MediaPlayer,
        unavailable: false,
        control: "media".into(),
        slider_action: "volume".into(),
        slider_min: 0.0,
        slider_max: 100.0,
        slider_step: 1.0,
        slider_value: volume,
        slider_unit: "%".into(),
        options: ModelRc::default(),
        selected_option: "".into(),
        locked: false,
        playing: true,
        has_volume: true,
        needs_code: false,
        actions: ModelRc::default(),
        camera_image: slint::Image::default(),
        camera_loading: false,
    }
}

fn render_all() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform {
        window: window.clone(),
    }))
    .expect("platform is set once");
    let ui = MainWindow::new().expect("window");
    ui.set_boot_screen(false);
    ui.set_rooms(ModelRc::new(VecModel::from(vec![RoomItem {
        id: "kitchen".into(),
        name: "Küche".into(),
        summary: "1 control".into(),
    }])));
    ui.set_selected_room(0);
    ui.set_tiles(ModelRc::new(VecModel::from(vec![TileItem {
        entity_id: "media_player.kitchen".into(),
        name: "Audio Küche".into(),
        value: "Playing".into(),
        icon: EntityIcon::MediaPlayer,
        active: true,
        pending: false,
        failed: false,
        unavailable: false,
        stale: false,
        command_enabled: true,
    }])));
    ui.show().expect("show");

    for (width, height) in [(800_u32, 1280_u32), (1280, 800)] {
        window.set_size(PhysicalSize::new(width, height));
        for (volume, title) in [
            (35.0, "Radio"),
            (0.0, ""),
            (100.0, &"Ein sehr langer Titel ".repeat(40)[..]),
        ] {
            ui.set_details_data(media_details(volume, title));
            ui.set_details_open(true);
            let mut buffer = vec![Rgb565Pixel(0); (width * height) as usize];
            window.request_redraw();
            let drawn = window.draw_if_needed(|renderer| {
                renderer.render(&mut buffer, width as usize);
            });
            assert!(drawn, "{width}x{height} volume {volume} not drawn");
        }
        ui.set_details_open(false);
    }
}

/// Runs the render on a thread with `stack` bytes; a panic or overflow fails the test.
fn on_stack(stack: usize) -> bool {
    std::thread::Builder::new()
        .stack_size(stack)
        .spawn(render_all)
        .expect("thread")
        .join()
        .is_ok()
}

#[test]
fn media_player_details_render_in_both_orientations() {
    let stack = std::env::var("NJORUN_TEST_STACK")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(8 * 1024 * 1024);
    assert!(
        on_stack(stack),
        "rendering failed with a {stack} byte stack"
    );
}
