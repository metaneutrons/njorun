use std::time::Duration;

use esp_idf_sys as _;
use njorun_app_core::settings::Settings;
use njorun_app_core::{AppCore, ConnectionStatus};

#[allow(unsafe_code)]
mod board {
    use std::slice;

    use esp_idf_sys::d1001;

    const WIDTH: usize = d1001::NJORUN_D1001_WIDTH as usize;
    const HEIGHT: usize = d1001::NJORUN_D1001_HEIGHT as usize;

    fn rgb565(red: u8, green: u8, blue: u8) -> u16 {
        (u16::from(red >> 3) << 11) | (u16::from(green >> 2) << 5) | u16::from(blue >> 3)
    }

    pub fn show_diagnostic() -> Result<(), i32> {
        let mut pointer = std::ptr::null_mut();
        let mut pixel_count = 0;
        let result = unsafe { d1001::njorun_d1001_display_init(&mut pointer, &mut pixel_count) };
        if result != 0 {
            return Err(result);
        }
        if pointer.is_null() || pixel_count != WIDTH * HEIGHT {
            return Err(-1);
        }

        let pixels = unsafe { slice::from_raw_parts_mut(pointer, pixel_count) };
        let teal = rgb565(41, 124, 111);
        let navy = rgb565(18, 50, 60);
        let mint = rgb565(120, 224, 207);
        let white = rgb565(239, 246, 244);
        for (index, pixel) in pixels.iter_mut().enumerate() {
            let x = index % WIDTH;
            let y = index / WIDTH;
            let border = !(24..WIDTH - 24).contains(&x) || !(24..HEIGHT - 24).contains(&y);
            let cross = x.abs_diff(WIDTH / 2) < 5 || y.abs_diff(HEIGHT / 2) < 5;
            *pixel = if border {
                mint
            } else if cross {
                white
            } else if (x / 100 + y / 100) % 2 == 0 {
                teal
            } else {
                navy
            };
        }
        let result = unsafe { d1001::njorun_d1001_display_sync() };
        if result != 0 {
            return Err(result);
        }
        let result = unsafe { d1001::njorun_d1001_backlight_set(70) };
        if result != 0 {
            return Err(result);
        }
        Ok(())
    }
}

fn main() -> ! {
    let settings = Settings::default();
    let mut core = AppCore::default();
    core.set_connection(ConnectionStatus::Synchronizing);
    let dashboard = core.dashboard();

    println!(
        "Njörun P4 smoke: ESP-IDF v6.1; default settings valid: {}; dashboard empty: {}",
        settings.is_valid(&["portrait", "landscape"]),
        dashboard.is_empty(),
    );

    match board::show_diagnostic() {
        Ok(()) => println!("Njörun D1001 display diagnostic ready"),
        Err(error) => println!("Njörun D1001 display diagnostic failed: {error}"),
    }

    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}
