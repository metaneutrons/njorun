//! Interactive first-run fixture followed by the real native dashboard demo.

use njorun_app_core::settings::{AppliedSettings, Settings};
use njorun_simulator::{
    RuntimeCommand, RuntimeEvent, commissioning,
    runtime::{self, RuntimeOptions},
    ui,
};
use std::{error::Error, path::PathBuf, sync::mpsc};

fn main() -> Result<(), Box<dyn Error>> {
    let orientation = std::env::args().nth(1).unwrap_or_else(|| "portrait".into());
    let (width, height) = match orientation.as_str() {
        "portrait" => (800, 1280),
        "landscape" => (1280, 800),
        _ => return Err("Usage: first_run [portrait|landscape] [STATE_FILE]".into()),
    };
    let state_file = std::env::args().nth(2).map_or_else(
        || PathBuf::from("target/first-run-state.json"),
        PathBuf::from,
    );
    let mode = std::env::args().nth(3).unwrap_or_else(|| "live".into());
    if !["live", "fixture"].contains(&mode.as_str()) {
        return Err("Usage: first_run [portrait|landscape] [STATE_FILE] [live|fixture]".into());
    }
    let credentials = state_file.with_extension("credentials.json");
    if !commissioning::is_complete(&state_file) || (mode == "live" && !credentials.exists()) {
        if mode == "live" {
            commissioning::run_live(&state_file, &credentials, width, height)?;
        } else {
            commissioning::run_fixture(&state_file, width, height)?;
        }
    }
    if mode == "live" {
        return run_live_dashboard(
            credentials,
            state_file.with_extension("runtime"),
            width,
            height,
        );
    }

    let core = ui::qualification::demo_core()?;
    let settings = AppliedSettings {
        revision: 1,
        settings: Settings {
            language: "en".into(),
            orientation,
            brightness: 100,
            screensaver: "disabled".into(),
            home_view: "rooms".into(),
            ..Settings::default()
        },
    };
    let (events, incoming) = mpsc::sync_channel(8);
    let (commands, _outgoing) = mpsc::sync_channel::<RuntimeCommand>(8);
    events.send(RuntimeEvent::Dashboard(core.dashboard().clone()))?;
    events.send(RuntimeEvent::Settings(Box::new(settings)))?;
    ui::run(incoming, commands, width, height)?;
    Ok(())
}

fn run_live_dashboard(
    credentials: PathBuf,
    state_directory: PathBuf,
    width: u32,
    height: u32,
) -> Result<(), Box<dyn Error>> {
    let options = RuntimeOptions {
        credentials,
        state_directory,
    };
    let (commands, command_rx) = mpsc::sync_channel(32);
    let (event_tx, events) = mpsc::sync_channel(64);
    let worker = std::thread::spawn(move || runtime::run(&options, &command_rx, &event_tx));
    ui::run(events, commands.clone(), width, height)?;
    let _ = commands.send(RuntimeCommand::Stop);
    worker
        .join()
        .map_err(|_| "Simulator worker stopped unexpectedly")??;
    Ok(())
}
