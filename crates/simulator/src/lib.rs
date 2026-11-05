//! Native simulator runs the same core state and HA management session in GUI and
//! headless qualification modes. No hardware support is implied.

use njorun_app_core::{Dashboard, settings::AppliedSettings};

/// Native UI and automated qualification share these commands.
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum RuntimeCommand {
    Tap(String),
    SettingsApplied(u64),
    Stop,
}

/// Public state only; credentials and private keys never reach the UI/report.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "event", content = "payload", rename_all = "snake_case")]
pub enum RuntimeEvent {
    Dashboard(Dashboard),
    Settings(Box<AppliedSettings>),
    Status(String),
    Daylight(bool),
}

/// Scales target pixels to a desktop preview that fits on common laptop displays.
///
/// Pixel-exact qualification uses `PhysicalSize` directly. Interactive native
/// windows preserve the target aspect ratio while limiting their long edge to
/// 960 logical points, avoiding macOS resizing only one axis after a layout
/// transition.
#[allow(clippy::cast_precision_loss)]
fn logical_size(width: u32, height: u32) -> slint::LogicalSize {
    debug_assert!(width <= 16_777_216 && height <= 16_777_216);
    let (width, height) = desktop_dimensions(width, height);
    slint::LogicalSize::new(width as f32, height as f32)
}

fn desktop_dimensions(width: u32, height: u32) -> (u32, u32) {
    const MAX_DESKTOP_EDGE: u32 = 960;
    let long_edge = width.max(height);
    if long_edge <= MAX_DESKTOP_EDGE {
        return (width, height);
    }
    let scale = |value: u32| {
        let numerator = u64::from(value) * u64::from(MAX_DESKTOP_EDGE) + u64::from(long_edge) / 2;
        u32::try_from(numerator / u64::from(long_edge)).expect("scaled desktop size fits u32")
    };
    (scale(width), scale(height))
}

fn preview_dimensions(width: u32, height: u32, physical: bool) -> (u32, u32) {
    if physical {
        (width, height)
    } else {
        desktop_dimensions(width, height)
    }
}

fn physical_preview_requested() -> bool {
    std::env::var("NJORUN_PHYSICAL_PREVIEW").is_ok_and(|value| value == "1")
}

pub mod commissioning;
mod network;
pub mod runtime;

pub mod ui;

#[cfg(test)]
mod tests {
    #[test]
    fn desktop_preview_preserves_target_aspect_ratio() {
        let portrait = super::logical_size(800, 1280);
        assert_eq!((portrait.width, portrait.height), (600.0, 960.0));

        let landscape = super::logical_size(1280, 800);
        assert_eq!((landscape.width, landscape.height), (960.0, 600.0));
        assert_eq!(super::desktop_dimensions(640, 480), (640, 480));
        assert_eq!(super::preview_dimensions(800, 1280, true), (800, 1280));
    }
}
