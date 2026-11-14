//! Check the actual shared Slint palette tokens against WCAG contrast thresholds.

use std::rc::Rc;

use slint::ComponentHandle;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter};

slint::slint! {
    import { MainWindow } from "../ui/main.slint";
    import { Palette } from "../ui/palette.slint";
    export { MainWindow, Palette }
}

struct TestPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }
}

fn rgb(color: slint::Color) -> [u8; 3] {
    let encoded = color.as_argb_encoded();
    [
        u8::try_from((encoded >> 16) & 0xff).expect("red channel"),
        u8::try_from((encoded >> 8) & 0xff).expect("green channel"),
        u8::try_from(encoded & 0xff).expect("blue channel"),
    ]
}

fn luminance(color: [u8; 3]) -> f64 {
    let channel = |value: u8| {
        let srgb = f64::from(value) / 255.0;
        if srgb <= 0.04045 {
            srgb / 12.92
        } else {
            ((srgb + 0.055) / 1.055).powf(2.4)
        }
    };
    0.0722_f64.mul_add(
        channel(color[2]),
        0.7152_f64.mul_add(channel(color[1]), 0.2126 * channel(color[0])),
    )
}

fn contrast_ratio(first: slint::Color, second: slint::Color) -> f64 {
    let first = luminance(rgb(first));
    let second = luminance(rgb(second));
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

#[test]
fn palette_tokens_meet_contrast_thresholds_in_every_mode() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform { window }))
        .expect("software platform is set once");
    let ui = MainWindow::new().expect("main window");
    ui.show().expect("show main window");
    let palette = ui.global::<Palette>();

    for scheme in ["njorun", "graphite", "warm", "porcelain"] {
        for dark in [false, true] {
            for increased_contrast in [false, true] {
                palette.set_scheme(scheme.into());
                palette.set_dark(dark);
                palette.set_increased_contrast(increased_contrast);

                let suffix = format!(
                    "scheme={scheme}, dark={dark}, increased_contrast={increased_contrast}"
                );
                let text = palette.get_text();
                let muted = palette.get_muted();
                let accent = palette.get_accent();
                let error = palette.get_error();
                let warning = palette.get_warning();
                let surface = palette.get_surface();
                let surface_alt = palette.get_surface_alt();
                let active_surface = palette.get_active_surface();
                let selected_surface = palette.get_selected_surface();
                let page = palette.get_page();
                let icon = palette.get_icon();
                let danger_surface = palette.get_danger_surface();
                let danger_surface_pressed = palette.get_danger_surface_pressed();
                let on_danger = palette.get_on_danger();
                let accent_fill = palette.get_accent_fill();
                let accent_fill_pressed = palette.get_accent_fill_pressed();

                let white = slint::Color::from_rgb_u8(255, 255, 255);
                for (label, foreground, background, minimum) in [
                    ("text/surface", text, surface, 4.5),
                    ("text/active-surface", text, active_surface, 4.5),
                    ("text/selected-surface", text, selected_surface, 4.5),
                    ("text/surface-alt", text, surface_alt, 4.5),
                    ("muted/surface", muted, surface, 4.5),
                    ("muted/active-surface", muted, active_surface, 4.5),
                    ("muted/selected-surface", muted, selected_surface, 4.5),
                    ("muted/page", muted, page, 4.5),
                    ("accent/surface", accent, surface, 3.0),
                    ("error/surface", error, surface, 4.5),
                    ("warning/surface", warning, surface, 4.5),
                    ("icon/surface", icon, surface, 3.0),
                    ("icon/surface-alt", icon, surface_alt, 3.0),
                    ("on-danger/danger-surface", on_danger, danger_surface, 4.5),
                    (
                        "on-danger/danger-surface-pressed",
                        on_danger,
                        danger_surface_pressed,
                        4.5,
                    ),
                    ("white/accent-fill", white, accent_fill, 4.5),
                    ("white/accent-fill-pressed", white, accent_fill_pressed, 4.5),
                ] {
                    let ratio = contrast_ratio(foreground, background);
                    assert!(
                        ratio >= minimum,
                        "{label}: {ratio:.2} < {minimum} for {suffix}"
                    );
                }
            }
        }
    }
}
