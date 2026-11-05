use std::{rc::Rc, time::Duration};

use esp_idf_sys as _;
use njorun_app_core::settings::Settings;
use njorun_app_core::{AppCore, ConnectionStatus};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, PlatformError, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, VecModel};

#[allow(unsafe_code)]
mod generated {
    slint::include_modules!();
}

use generated::{CommissioningWindow, WifiItem};

struct D1001Platform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for D1001Platform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }
}

struct D1001Ui {
    ui: CommissioningWindow,
    window: Rc<MinimalSoftwareWindow>,
    framebuffer: &'static mut [Rgb565Pixel],
    pointer_pressed: bool,
    pointer_position: LogicalPosition,
}

#[allow(unsafe_code)]
mod board {
    use std::{ffi::CStr, slice};

    use esp_idf_sys::d1001;
    use slint::platform::software_renderer::Rgb565Pixel;

    pub const WIDTH: usize = d1001::NJORUN_D1001_WIDTH as usize;
    pub const HEIGHT: usize = d1001::NJORUN_D1001_HEIGHT as usize;
    const MAX_WIFI_NETWORKS: usize = d1001::NJORUN_D1001_MAX_WIFI_NETWORKS as usize;

    pub struct WifiNetwork {
        pub ssid: String,
        pub rssi: i8,
        pub password_required: bool,
    }

    pub struct TouchPoint {
        pub x: u16,
        pub y: u16,
        pub pressed: bool,
    }

    impl WifiNetwork {
        pub fn signal_bars(&self) -> i32 {
            match self.rssi {
                -50..=i8::MAX => 5,
                -60..=-51 => 4,
                -70..=-61 => 3,
                -80..=-71 => 2,
                _ => 1,
            }
        }
    }

    pub fn display_framebuffer() -> Result<&'static mut [Rgb565Pixel], i32> {
        let mut pointer = std::ptr::null_mut();
        let mut pixel_count = 0;
        let result = unsafe { d1001::njorun_d1001_display_init(&mut pointer, &mut pixel_count) };
        if result != 0 {
            return Err(result);
        }
        if pointer.is_null() || pixel_count != WIDTH * HEIGHT {
            return Err(-1);
        }

        Ok(unsafe { slice::from_raw_parts_mut(pointer.cast::<Rgb565Pixel>(), pixel_count) })
    }

    pub fn display_sync() -> Result<(), i32> {
        let result = unsafe { d1001::njorun_d1001_display_sync() };
        (result == 0).then_some(()).ok_or(result)
    }

    pub fn backlight_set(percent: u8) -> Result<(), i32> {
        let result = unsafe { d1001::njorun_d1001_backlight_set(percent) };
        (result == 0).then_some(()).ok_or(result)
    }

    pub fn touch_init() -> Result<(), i32> {
        let result = unsafe { d1001::njorun_d1001_touch_init() };
        (result == 0).then_some(()).ok_or(result)
    }

    pub fn touch_read() -> Result<TouchPoint, i32> {
        let mut point = d1001::njorun_d1001_touch_point_t::default();
        let result = unsafe { d1001::njorun_d1001_touch_read(&mut point) };
        if result != 0 {
            return Err(result);
        }
        Ok(TouchPoint {
            x: point.x,
            y: point.y,
            pressed: point.pressed,
        })
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

impl D1001Ui {
    fn new() -> Result<Self, String> {
        let framebuffer = board::display_framebuffer().map_err(|error| error.to_string())?;
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        window.set_size(PhysicalSize::new(
            u32::try_from(board::WIDTH).map_err(|_| "display width does not fit u32")?,
            u32::try_from(board::HEIGHT).map_err(|_| "display height does not fit u32")?,
        ));
        // Slint's MCU configuration uses single-threaded global state. All UI
        // creation, updates and rendering remain on this application thread.
        slint::platform::set_platform(Box::new(D1001Platform {
            window: window.clone(),
        }))
        .map_err(|_| "Slint platform was already initialized")?;

        let ui = CommissioningWindow::new().map_err(|error| error.to_string())?;
        ui.set_viewport_width(
            i32::try_from(board::WIDTH).map_err(|_| "display width does not fit i32")?,
        );
        ui.set_viewport_height(
            i32::try_from(board::HEIGHT).map_err(|_| "display height does not fit i32")?,
        );
        ui.set_landscape(false);
        ui.set_stage("wifi".into());
        ui.show().map_err(|error| error.to_string())?;

        let mut result = Self {
            ui,
            window,
            framebuffer,
            pointer_pressed: false,
            pointer_position: LogicalPosition::default(),
        };
        result.render()?;
        board::backlight_set(70).map_err(|error| error.to_string())?;
        board::touch_init().map_err(|error| error.to_string())?;
        Ok(result)
    }

    fn set_wifi_networks(&mut self, networks: &[board::WifiNetwork]) -> Result<(), String> {
        self.ui.set_wifi_networks(ModelRc::new(VecModel::from(
            networks
                .iter()
                .map(|network| WifiItem {
                    name: network.ssid.clone().into(),
                    signal: network.signal_bars(),
                    secured: network.password_required,
                })
                .collect::<Vec<_>>(),
        )));
        self.render()
    }

    fn render(&mut self) -> Result<(), String> {
        let rendered = self.window.draw_if_needed(|renderer| {
            renderer.render(&mut *self.framebuffer, board::WIDTH);
        });
        if rendered {
            board::display_sync().map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn poll(&mut self) -> Result<(), String> {
        let point = board::touch_read().map_err(|error| error.to_string())?;
        if point.pressed {
            let position = LogicalPosition::new(f32::from(point.x), f32::from(point.y));
            self.ui
                .window()
                .dispatch_event(WindowEvent::PointerMoved { position });
            if !self.pointer_pressed {
                self.ui
                    .window()
                    .dispatch_event(WindowEvent::PointerPressed {
                        position,
                        button: PointerEventButton::Left,
                    });
            }
            self.pointer_position = position;
        } else if self.pointer_pressed {
            self.ui
                .window()
                .dispatch_event(WindowEvent::PointerReleased {
                    position: self.pointer_position,
                    button: PointerEventButton::Left,
                });
        }
        self.pointer_pressed = point.pressed;
        slint::platform::update_timers_and_animations();
        self.render()
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

    let mut ui = match D1001Ui::new() {
        Ok(ui) => {
            println!("Njörun D1001 commissioning UI ready");
            Some(ui)
        }
        Err(error) => {
            println!("Njörun D1001 commissioning UI failed: {error}");
            None
        }
    };
    let networks = match board::scan_wifi() {
        Ok(networks) => {
            println!("Njörun D1001 Wi-Fi scan found {} networks", networks.len());
            for network in &networks {
                println!(
                    "Wi-Fi: {:?}, RSSI {}, password required: {}",
                    network.ssid, network.rssi, network.password_required
                );
            }
            networks
        }
        Err(error) => {
            println!("Njörun D1001 Wi-Fi scan failed: {error}");
            Vec::new()
        }
    };
    if let Some(ui) = &mut ui {
        if let Err(error) = ui.set_wifi_networks(&networks) {
            println!("Njörun D1001 commissioning UI update failed: {error}");
        }
    }

    loop {
        if let Some(ui) = &mut ui {
            if let Err(error) = ui.poll() {
                println!("Njörun D1001 touch poll failed: {error}");
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
