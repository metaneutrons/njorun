use std::{
    rc::Rc,
    sync::mpsc::{channel, Receiver, TryRecvError},
    time::Duration,
};

use esp_idf_sys as _;
use njorun_app_core::settings::Settings;
use njorun_app_core::{AppCore, ConnectionStatus};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, PlatformError, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, SharedString, VecModel};

mod commissioning;

#[allow(unsafe_code)]
mod generated {
    slint::include_modules!();
}

use generated::{CommissioningWindow, MainWindow, RoomItem};

struct D1001Platform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for D1001Platform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        // ESP-IDF exposes a monotonic microsecond clock from early startup.
        // Slint requires this on embedded/no-std backends for timers and
        // animations; the default implementation deliberately panics.
        #[allow(unsafe_code)]
        let microseconds = unsafe { esp_idf_sys::esp_timer_get_time() };
        Duration::from_micros(u64::try_from(microseconds).unwrap_or_default())
    }
}

struct D1001Ui {
    screen: Screen,
    window: Rc<MinimalSoftwareWindow>,
    framebuffer: &'static mut [Rgb565Pixel],
    pointer_pressed: bool,
    pointer_position: LogicalPosition,
    commissioning: commissioning::State,
    authorization: Option<commissioning::Authorization>,
    actions: Receiver<commissioning::Action>,
}

enum Screen {
    Commissioning(CommissioningWindow),
    Dashboard(MainWindow),
}

#[allow(unsafe_code)]
mod board {
    use std::{
        ffi::{CStr, CString},
        slice,
    };

    use esp_idf_sys::d1001;
    use slint::platform::software_renderer::Rgb565Pixel;

    pub const WIDTH: usize = d1001::NJORUN_D1001_WIDTH as usize;
    pub const HEIGHT: usize = d1001::NJORUN_D1001_HEIGHT as usize;
    const MAX_WIFI_NETWORKS: usize = d1001::NJORUN_D1001_MAX_WIFI_NETWORKS as usize;
    const MAX_HA_INSTANCES: usize = d1001::NJORUN_D1001_MAX_HA_INSTANCES as usize;
    const HTTP_RESPONSE_CAPACITY: usize = 16 * 1024;
    const WEBSOCKET_RESPONSE_CAPACITY: usize = 64 * 1024;

    #[derive(Clone)]
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

    #[derive(Clone)]
    pub struct HaInstance {
        pub name: String,
        pub endpoint: String,
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

    pub fn connect_wifi(ssid: &str, password: &str) -> Result<(), i32> {
        let ssid = CString::new(ssid).map_err(|_| -1)?;
        let password = CString::new(password).map_err(|_| -1)?;
        let result = unsafe { d1001::njorun_d1001_wifi_connect(ssid.as_ptr(), password.as_ptr()) };
        (result == 0).then_some(()).ok_or(result)
    }

    pub fn discover_home_assistant() -> Result<Vec<HaInstance>, i32> {
        let mut records = [d1001::njorun_d1001_ha_instance_t::default(); MAX_HA_INSTANCES];
        let mut count = 0;
        let result = unsafe {
            d1001::njorun_d1001_ha_discover(records.as_mut_ptr(), records.len(), &mut count)
        };
        if result != 0 {
            return Err(result);
        }
        records
            .get(..count)
            .ok_or(-1)?
            .iter()
            .map(|record| {
                let name = unsafe { CStr::from_ptr(record.name.as_ptr()) }
                    .to_str()
                    .map_err(|_| -1)?;
                let endpoint = unsafe { CStr::from_ptr(record.endpoint.as_ptr()) }
                    .to_str()
                    .map_err(|_| -1)?;
                Ok(HaInstance {
                    name: name.to_owned(),
                    endpoint: endpoint.to_owned(),
                })
            })
            .collect()
    }

    pub fn fill_random(buffer: &mut [u8]) -> Result<(), i32> {
        let result =
            unsafe { d1001::njorun_d1001_random(buffer.as_mut_ptr().cast(), buffer.len()) };
        (result == 0).then_some(()).ok_or(result)
    }

    pub fn post_form(url: &str, body: &str) -> Result<Vec<u8>, i32> {
        let url = CString::new(url).map_err(|_| -1)?;
        let body = CString::new(body).map_err(|_| -1)?;
        let mut response = vec![0_u8; HTTP_RESPONSE_CAPACITY];
        let mut length = 0;
        let result = unsafe {
            d1001::njorun_d1001_http_post_form(
                url.as_ptr(),
                body.as_ptr(),
                response.as_mut_ptr().cast(),
                response.len(),
                &mut length,
            )
        };
        if result != 0 || length >= response.len() {
            return Err(if result == 0 { -1 } else { result });
        }
        response.truncate(length);
        Ok(response)
    }

    pub fn fetch_areas(websocket_url: &str, auth_message: &str) -> Result<Vec<u8>, i32> {
        let websocket_url = CString::new(websocket_url).map_err(|_| -1)?;
        let auth_message = CString::new(auth_message).map_err(|_| -1)?;
        let mut response = vec![0_u8; WEBSOCKET_RESPONSE_CAPACITY];
        let mut length = 0;
        let result = unsafe {
            d1001::njorun_d1001_ha_fetch_areas(
                websocket_url.as_ptr(),
                auth_message.as_ptr(),
                response.as_mut_ptr().cast(),
                response.len(),
                &mut length,
            )
        };
        if result != 0 || length >= response.len() {
            return Err(if result == 0 { -1 } else { result });
        }
        response.truncate(length);
        Ok(response)
    }
}

impl D1001Ui {
    fn new() -> Result<Self, String> {
        println!("Njörun D1001: initializing display");
        let framebuffer = board::display_framebuffer().map_err(|error| error.to_string())?;
        println!("Njörun D1001: framebuffer acquired");

        // Establish a visible, deterministic panel baseline before Slint does
        // its first full-frame render. This prevents an uninitialized scanout
        // while the commissioning component is being constructed.
        framebuffer.fill(Rgb565Pixel(0xffff));
        board::display_sync().map_err(|error| error.to_string())?;
        board::backlight_set(70).map_err(|error| error.to_string())?;
        println!("Njörun D1001: initial frame visible");

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
        println!("Njörun D1001: Slint platform ready");

        let ui = CommissioningWindow::new().map_err(|error| error.to_string())?;
        println!("Njörun D1001: commissioning component created");
        ui.set_viewport_width(
            i32::try_from(board::WIDTH).map_err(|_| "display width does not fit i32")?,
        );
        ui.set_viewport_height(
            i32::try_from(board::HEIGHT).map_err(|_| "display height does not fit i32")?,
        );
        ui.set_landscape(false);
        let commissioning = commissioning::State::new(Vec::new());
        commissioning::render(&ui, &commissioning)?;
        println!("Njörun D1001: commissioning model rendered");
        let (action_sender, actions) = channel();
        commissioning::install_callbacks(&ui, action_sender);
        ui.show().map_err(|error| error.to_string())?;
        println!("Njörun D1001: commissioning window shown");

        let mut result = Self {
            screen: Screen::Commissioning(ui),
            window,
            framebuffer,
            pointer_pressed: false,
            pointer_position: LogicalPosition::default(),
            commissioning,
            authorization: None,
            actions,
        };
        println!("Njörun D1001: starting first frame render");
        result.render()?;
        println!("Njörun D1001: first frame rendered");
        board::touch_init().map_err(|error| error.to_string())?;
        Ok(result)
    }

    fn set_wifi_networks(&mut self, networks: Vec<board::WifiNetwork>) -> Result<(), String> {
        self.commissioning = commissioning::State::new(networks);
        if let Screen::Commissioning(ui) = &self.screen {
            commissioning::render(ui, &self.commissioning)?;
        }
        self.render()
    }

    fn dispatch_event(&self, event: WindowEvent) {
        match &self.screen {
            Screen::Commissioning(ui) => ui.window().dispatch_event(event),
            Screen::Dashboard(ui) => ui.window().dispatch_event(event),
        }
    }

    fn process_actions(&mut self) -> Result<(), String> {
        if !matches!(self.screen, Screen::Commissioning(_)) {
            return Ok(());
        }
        loop {
            match self.actions.try_recv() {
                Ok(action) => {
                    commissioning::handle_action(
                        action,
                        &mut self.commissioning,
                        &mut self.authorization,
                    );
                    if let Screen::Commissioning(ui) = &self.screen {
                        commissioning::render(ui, &self.commissioning)?;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err("commissioning action channel disconnected".to_owned());
                }
            }
        }
        Ok(())
    }

    fn poll_authorization(&mut self) -> Result<(), String> {
        if !matches!(self.screen, Screen::Commissioning(_)) {
            return Ok(());
        }
        let german = commissioning::is_german(&self.commissioning);
        match commissioning::poll_authorization(&mut self.authorization, german) {
            Ok(Some(rooms)) => self.show_dashboard(rooms, german),
            Ok(None) => Ok(()),
            Err(error) => {
                commissioning::set_error(&mut self.commissioning, error);
                if let Screen::Commissioning(ui) = &self.screen {
                    commissioning::render(ui, &self.commissioning)?;
                }
                Ok(())
            }
        }
    }

    fn show_dashboard(&mut self, rooms: Vec<RoomItem>, german: bool) -> Result<(), String> {
        if let Screen::Commissioning(ui) = &self.screen {
            ui.hide().map_err(|error| error.to_string())?;
        }
        let ui = MainWindow::new().map_err(|error| error.to_string())?;
        ui.set_rooms(ModelRc::new(VecModel::from(rooms)));
        ui.set_tiles(ModelRc::default());
        ui.set_favorites(ModelRc::default());
        ui.set_selected_room(-1);
        ui.set_home_view("rooms".into());
        ui.set_rooms_page(false);
        ui.set_dark_mode(false);
        ui.set_german(german);
        ui.set_boot_screen(false);
        ui.set_connection_ready(true);
        ui.set_network_link("wifi".into());
        ui.set_connection_text(SharedString::default());
        ui.show().map_err(|error| error.to_string())?;
        self.screen = Screen::Dashboard(ui);
        Ok(())
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
            self.dispatch_event(WindowEvent::PointerMoved { position });
            if !self.pointer_pressed {
                self.dispatch_event(WindowEvent::PointerPressed {
                    position,
                    button: PointerEventButton::Left,
                });
            }
            self.pointer_position = position;
        } else if self.pointer_pressed {
            self.dispatch_event(WindowEvent::PointerReleased {
                position: self.pointer_position,
                button: PointerEventButton::Left,
            });
        }
        self.pointer_pressed = point.pressed;
        self.process_actions()?;
        self.poll_authorization()?;
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
        if let Err(error) = ui.set_wifi_networks(networks) {
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
