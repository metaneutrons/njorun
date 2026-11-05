use std::time::Duration;

use esp_idf_sys as _;
use njorun_app_core::settings::Settings;
use njorun_app_core::{AppCore, ConnectionStatus};

#[allow(unsafe_code)]
mod board {
    use std::{ffi::CStr, slice};

    use esp_idf_sys::d1001;

    const WIDTH: usize = d1001::NJORUN_D1001_WIDTH as usize;
    const HEIGHT: usize = d1001::NJORUN_D1001_HEIGHT as usize;
    const MAX_WIFI_NETWORKS: usize = d1001::NJORUN_D1001_MAX_WIFI_NETWORKS as usize;

    pub struct WifiNetwork {
        pub ssid: String,
        pub rssi: i8,
        pub password_required: bool,
    }

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

    pub fn scan_wifi() -> Result<Vec<WifiNetwork>, i32> {
        let mut records = [d1001::njorun_d1001_wifi_network_t::default(); MAX_WIFI_NETWORKS];
        let mut count = 0;
        let result = unsafe {
            d1001::njorun_d1001_wifi_scan(records.as_mut_ptr(), records.len(), &mut count)
        };
        if result != 0 {
            return Err(result);
        }
        records
            .get(..count)
            .ok_or(-1)?
            .iter()
            .map(|record| {
                let name = unsafe { CStr::from_ptr(record.ssid.as_ptr()) }
                    .to_str()
                    .map_err(|_| -1)?;
                Ok(WifiNetwork {
                    ssid: name.to_owned(),
                    rssi: record.rssi,
                    password_required: record.password_required,
                })
            })
            .collect()
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
    match board::scan_wifi() {
        Ok(networks) => {
            println!("Njörun D1001 Wi-Fi scan found {} networks", networks.len());
            for network in networks {
                println!(
                    "Wi-Fi: {:?}, RSSI {}, password required: {}",
                    network.ssid, network.rssi, network.password_required
                );
            }
        }
        Err(error) => println!("Njörun D1001 Wi-Fi scan failed: {error}"),
    }

    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}
