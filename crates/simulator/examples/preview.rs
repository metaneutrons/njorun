//! Render the actual Slint software UI for visual qualification.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("target/ui-previews"), PathBuf::from);
    fs::create_dir_all(&directory)?;
    njorun_simulator::ui::qualification::render_qualification_previews_with(|frame| {
        let path = directory.join(format!("{}.png", frame.name));
        let output = BufWriter::new(File::create(&path)?);
        let mut encoder = png::Encoder::new(output, frame.width, frame.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let pixels: Vec<_> = frame
            .pixels
            .iter()
            .flat_map(|pixel| [pixel.red, pixel.green, pixel.blue, pixel.alpha])
            .collect();
        encoder.write_header()?.write_image_data(&pixels)?;
        println!("{} ({} × {})", path.display(), frame.width, frame.height);
        Ok(())
    })?;
    Ok(())
}
