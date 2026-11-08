//! Checks `Orientation::to_logical` against Slint's real software renderer: every panel pixel
//! the renderer lights for a known logical rectangle must map back into that rectangle.

use std::rc::Rc;

use njorun_app_core::orientation::Orientation;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, RenderingRotation, RepaintBufferType, Rgb565Pixel,
};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize};

slint::slint! {
    export component Probe inherits Window {
        background: #000000;
        Rectangle {
            x: 10px;
            y: 20px;
            width: 30px;
            height: 40px;
            background: #ff0000;
        }
    }
}

struct TestPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }
}

const PANEL_WIDTH: u32 = 800;
const PANEL_HEIGHT: u32 = 1280;
const RED: u16 = 0xF800;

const fn rotation(orientation: Orientation) -> RenderingRotation {
    match orientation.rotation_degrees() {
        0 => RenderingRotation::NoRotation,
        90 => RenderingRotation::Rotate90,
        180 => RenderingRotation::Rotate180,
        _ => RenderingRotation::Rotate270,
    }
}

#[test]
fn touch_mapping_inverts_the_renderers_rotation() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform {
        window: window.clone(),
    }))
    .expect("platform is set once");
    let probe = Probe::new().expect("probe");
    probe.show().expect("show");

    for orientation in Orientation::ALL {
        let (logical_width, logical_height) = orientation.logical_size(PANEL_WIDTH, PANEL_HEIGHT);
        window.set_size(PhysicalSize::new(logical_width, logical_height));
        let mut buffer = vec![Rgb565Pixel(0); (PANEL_WIDTH * PANEL_HEIGHT) as usize];
        let drawn = window.draw_if_needed(|renderer| {
            renderer.set_rendering_rotation(rotation(orientation));
            renderer.render(&mut buffer, PANEL_WIDTH as usize);
        });
        assert!(drawn, "{orientation:?} was not drawn");

        let mut lit = 0;
        for (index, pixel) in buffer.iter().enumerate() {
            if pixel.0 != RED {
                continue;
            }
            let index = u32::try_from(index).expect("index fits");
            let (x, y) = orientation.to_logical(
                index % PANEL_WIDTH,
                index / PANEL_WIDTH,
                PANEL_WIDTH,
                PANEL_HEIGHT,
            );
            assert!(
                (10..40).contains(&x) && (20..60).contains(&y),
                "{orientation:?}: panel pixel {},{} maps to {x},{y}, outside the probe",
                index % PANEL_WIDTH,
                index / PANEL_WIDTH
            );
            lit += 1;
        }
        assert_eq!(lit, 30 * 40, "{orientation:?} lit {lit} pixels");
    }
}
