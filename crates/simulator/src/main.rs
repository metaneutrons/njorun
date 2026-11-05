//! Desktop simulator entry point. Credentials are accepted only through private files.

use njorun_ha_client::{generate_identity, storage};
use njorun_simulator::{
    RuntimeCommand, RuntimeEvent,
    runtime::{self, RuntimeOptions},
};
use std::io::{self, BufRead};
use std::path::PathBuf;
use std::sync::mpsc;

fn main() {
    if let Err(message) = start() {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

fn start() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|value| value == "identity") && args.len() == 2 {
        let path = PathBuf::from(&args[1]);
        if path.exists() {
            return Err("Identity file already exists; refusing to replace trust".into());
        }
        let identity = generate_identity().map_err(|error| error.to_string())?;
        storage::write(&path, &serde_json::json!({"private_key":identity.private_key,"public_key":identity.public_key}))
            .map_err(|error| error.to_string())?;
        println!("Identity created in the selected private file");
        return Ok(());
    }
    if args.first().is_none_or(|value| value != "run") {
        return Err("Usage: njorun-simulator identity FILE | run --credentials FILE --state-dir DIR [--headless] [--width N --height N]".into());
    }
    let value = |flag: &str| -> Option<String> {
        args.windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| pair[1].clone())
    };
    let options = RuntimeOptions {
        credentials: PathBuf::from(value("--credentials").ok_or("Missing --credentials FILE")?),
        state_directory: PathBuf::from(value("--state-dir").ok_or("Missing --state-dir DIR")?),
    };
    let width: u32 = value("--width")
        .unwrap_or_else(|| "800".into())
        .parse()
        .map_err(|_| "Invalid width")?;
    let height: u32 = value("--height")
        .unwrap_or_else(|| "1280".into())
        .parse()
        .map_err(|_| "Invalid height")?;
    if !(320..=2048).contains(&width) || !(320..=2048).contains(&height) {
        return Err("Unsupported simulator dimensions".into());
    }
    let (command_tx, command_rx) = mpsc::sync_channel(32);
    let (event_tx, event_rx) = mpsc::sync_channel(64);
    let worker = std::thread::spawn(move || runtime::run(&options, &command_rx, &event_tx));
    if args.iter().any(|value| value == "--headless") {
        let input_tx = command_tx.clone();
        std::thread::spawn(move || {
            for line in io::stdin().lock().lines() {
                let Ok(line) = line else {
                    break;
                };
                if line.len() > 4096 {
                    continue;
                }
                if let Ok(command) = serde_json::from_str::<RuntimeCommand>(&line)
                    && input_tx.send(command).is_err()
                {
                    return;
                }
            }
            let _ = input_tx.send(RuntimeCommand::Stop);
        });
        for event in event_rx {
            if let RuntimeEvent::Settings(settings) = &event {
                command_tx
                    .send(RuntimeCommand::SettingsApplied(settings.revision))
                    .map_err(|_| "Worker stopped")?;
            }
            println!(
                "{}",
                serde_json::to_string(&event).map_err(|_| "Could not encode public report")?
            );
        }
    } else {
        // Native frontend is compiled from the shared Slint candidate.
        njorun_simulator::ui::run(event_rx, command_tx.clone(), width, height)
            .map_err(|error| error.to_string())?;
        let _ = command_tx.send(RuntimeCommand::Stop);
    }
    worker
        .join()
        .map_err(|_| "Simulator worker stopped unexpectedly")?
        .map_err(|error| error.to_string())
}
