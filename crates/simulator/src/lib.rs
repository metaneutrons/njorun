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

pub mod runtime;

pub mod ui;
