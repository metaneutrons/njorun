//! Exercise the real HA runtime through the Slint software-rendered UI.

use std::error::Error;
use std::fs::{self, File};
use std::io::{BufWriter, Error as IoError, ErrorKind};
use std::path::PathBuf;
use std::time::Duration;

use njorun_simulator::ui::{LiveQualificationOptions, qualify_live_ha_ui};

struct Arguments {
    credentials: PathBuf,
    state_directory: PathBuf,
    output: PathBuf,
    entity_id: String,
    room_id: String,
    timeout: Duration,
    width: u32,
    height: u32,
}

fn main() -> Result<(), Box<dyn Error>> {
    let Some(arguments) = parse_arguments()? else {
        return Ok(());
    };
    let options = LiveQualificationOptions {
        credentials: arguments.credentials,
        state_directory: arguments.state_directory,
        entity_id: arguments.entity_id,
        room_id: arguments.room_id,
        timeout: arguments.timeout,
        width: arguments.width,
        height: arguments.height,
    };
    let output = &arguments.output;
    let result = qualify_live_ha_ui(&options, |result| {
        write_png(output, &result.frame)?;
        Ok(())
    })?;
    // Report overall success only after the runtime worker has stopped cleanly.
    let report_path = output.with_extension("json");
    let report = serde_json::json!({
        "status": "ok",
        "worker_shutdown": "graceful",
        "entity_id": result.entity_id,
        "room_id": result.room_id,
        "room_name": result.room_name,
        "state_before": result.state_before,
        "state_after": result.state_after,
        "elapsed_ms": result.elapsed_ms,
        "width": result.frame.width,
        "height": result.frame.height,
        "png": output,
        "report": report_path,
    });
    write_file(&report_path, &serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

fn parse_arguments() -> Result<Option<Arguments>, Box<dyn Error>> {
    let mut credentials = None;
    let mut state_directory = None;
    let mut output = None;
    let mut entity_id = None;
    let mut room_id = None;
    let mut timeout_seconds = 120_u64;
    let mut width = 800_u32;
    let mut height = 1280_u32;
    let mut values = std::env::args_os().skip(1);

    while let Some(argument) = values.next() {
        match argument.to_str() {
            Some("--help" | "-h") => {
                print_usage();
                return Ok(None);
            }
            Some("--credentials") => credentials = Some(take_path(&mut values, "--credentials")?),
            Some("--state-dir") => state_directory = Some(take_path(&mut values, "--state-dir")?),
            Some("--output") => output = Some(take_path(&mut values, "--output")?),
            Some("--entity") => entity_id = Some(take_text(&mut values, "--entity")?),
            Some("--room") => room_id = Some(take_text(&mut values, "--room")?),
            Some("--timeout-seconds") => {
                timeout_seconds = take_text(&mut values, "--timeout-seconds")?.parse()?;
            }
            Some("--width") => width = take_text(&mut values, "--width")?.parse()?,
            Some("--height") => height = take_text(&mut values, "--height")?.parse()?,
            _ => return Err(parse_error("unknown or non-UTF8 option")),
        }
    }

    Ok(Some(Arguments {
        credentials: required(credentials, "--credentials")?,
        state_directory: required(state_directory, "--state-dir")?,
        output: required(output, "--output")?,
        entity_id: required(entity_id, "--entity")?,
        room_id: required(room_id, "--room")?,
        timeout: Duration::from_secs(timeout_seconds),
        width,
        height,
    }))
}

fn take_path(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
    option: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| parse_error(&format!("missing value for {option}")))
}

fn take_text(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
    option: &str,
) -> Result<String, Box<dyn Error>> {
    arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| parse_error(&format!("missing or invalid value for {option}")))
}

fn required<T>(value: Option<T>, option: &str) -> Result<T, Box<dyn Error>> {
    value.ok_or_else(|| parse_error(&format!("missing required option {option}")))
}

fn parse_error(message: &str) -> Box<dyn Error> {
    Box::new(IoError::new(ErrorKind::InvalidInput, message.to_owned()))
}

fn print_usage() {
    println!(
        "Usage: live_ui --credentials FILE --state-dir DIR --output PNG --entity ENTITY_ID --room AREA_ID|unassigned [--timeout-seconds N] [--width PX] [--height PX]"
    );
    println!("Defaults: timeout=120, width=800, height=1280");
}

fn write_png(
    path: &std::path::Path,
    frame: &njorun_simulator::ui::PreviewFrame,
) -> Result<(), Box<dyn Error>> {
    create_parent(path)?;
    let output = BufWriter::new(File::create(path)?);
    let mut encoder = png::Encoder::new(output, frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let pixels: Vec<_> = frame
        .pixels
        .iter()
        .flat_map(|pixel| [pixel.red, pixel.green, pixel.blue, pixel.alpha])
        .collect();
    encoder.write_header()?.write_image_data(&pixels)?;
    Ok(())
}

fn write_file(path: &std::path::Path, contents: &[u8]) -> Result<(), Box<dyn Error>> {
    create_parent(path)?;
    fs::write(path, contents)?;
    Ok(())
}

fn create_parent(path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}
