use std::time::Duration;

use esp_idf_sys as _;
use njorun_app_core::settings::Settings;
use njorun_app_core::{AppCore, ConnectionStatus};

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

    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}
