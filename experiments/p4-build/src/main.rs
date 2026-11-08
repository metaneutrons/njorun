use std::{
    rc::Rc,
    sync::mpsc::{channel, Receiver, Sender, TryRecvError},
    time::Duration,
};

use esp_idf_sys as _;
use njorun_app_core::orientation::Orientation;
use njorun_app_core::presentation;
use njorun_app_core::problems::{ConnectionProblem, SetupProblem};
use njorun_app_core::settings::Settings;
use njorun_app_core::{AppCore, ConnectionStatus, EntityDomain, RoomId};
use slint::platform::software_renderer::{
    LineBufferProvider, MinimalSoftwareWindow, RenderingRotation, RepaintBufferType, Rgb565Pixel,
};
use slint::platform::{Platform, PlatformError, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, Image, LogicalPosition, PhysicalSize, Rgb8Pixel, SharedPixelBuffer};

mod commissioning;
mod dashboard;
mod ha;
mod store;

#[allow(unsafe_code)]
mod generated {
    slint::include_modules!();
}

use generated::{CommissioningWindow, MainWindow};

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
    line_buffer: Vec<Rgb565Pixel>,
    pointer_pressed: bool,
    pointer_position: LogicalPosition,
    commissioning: commissioning::State,
    authorization: Option<commissioning::Authorization>,
    restoring: Option<store::HomeAssistantSession>,
    session: Option<dashboard::Session>,
    host: String,
    selected_room: Option<RoomId>,
    details: Option<String>,
    dashboard_dirty: bool,
    last_dashboard_render: std::time::Instant,
    last_clock_check: std::time::Instant,
    clock_label: String,
    camera_image: Option<(String, Image)>,
    camera_in_flight: bool,
    camera_last: std::time::Instant,
    setup_problem: Option<SetupProblem>,
    orientation: Orientation,
    /// `light`, `dark` or `automatic`.
    theme: String,
    /// Repaint everything on the next frame, after the orientation changed.
    full_repaint: bool,
    ui_action_sender: Sender<dashboard::UiAction>,
    ui_actions: Receiver<dashboard::UiAction>,
    action_sender: Sender<commissioning::Action>,
    actions: Receiver<commissioning::Action>,
}

/// Composes each scanline in internal RAM and writes it once to the PSRAM
/// framebuffer, avoiding PSRAM read-modify-write blending and L2 cache churn.
struct LineCopier<'a> {
    framebuffer: &'a mut [Rgb565Pixel],
    line: &'a mut [Rgb565Pixel],
}

impl LineBufferProvider for LineCopier<'_> {
    type TargetPixel = Rgb565Pixel;

    fn process_line(
        &mut self,
        line: usize,
        range: std::ops::Range<usize>,
        render_fn: impl FnOnce(&mut [Rgb565Pixel]),
    ) {
        let Some(buffer) = self.line.get_mut(range.clone()) else {
            return;
        };
        render_fn(buffer);
        let start = line * board::WIDTH + range.start;
        let end = line * board::WIDTH + range.end;
        if let Some(target) = self.framebuffer.get_mut(start..end) {
            target.copy_from_slice(buffer);
        }
    }
}

enum Screen {
    Commissioning(CommissioningWindow),
    Dashboard(MainWindow),
}

#[allow(unsafe_code)]
mod board {
    use std::{
        ffi::{CStr, CString},
        net::Ipv4Addr,
        slice,
    };

    use esp_idf_sys::d1001;
    use slint::platform::software_renderer::Rgb565Pixel;

    unsafe extern "C" {
        fn njorun_d1001_wifi_ipv4(address: *mut u8) -> i32;
    }

    pub const WIDTH: usize = d1001::NJORUN_D1001_WIDTH as usize;
    pub const HEIGHT: usize = d1001::NJORUN_D1001_HEIGHT as usize;
    const MAX_WIFI_NETWORKS: usize = d1001::NJORUN_D1001_MAX_WIFI_NETWORKS as usize;
    const MAX_HA_INSTANCES: usize = d1001::NJORUN_D1001_MAX_HA_INSTANCES as usize;
    const HTTP_RESPONSE_CAPACITY: usize = 16 * 1024;

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

    const STORE_VALUE_CAPACITY: usize = 2200;

    const ERR_NOT_FOUND: i32 = 0x105;

    /// `Ok(None)` when the key is not saved; `Err` when storage cannot be read or the value
    /// is not text.
    pub fn store_get(key: &str) -> Result<Option<String>, i32> {
        let key = CString::new(key).map_err(|_| -1)?;
        let mut value = vec![0_u8; STORE_VALUE_CAPACITY];
        let mut length = 0;
        let result = unsafe {
            d1001::njorun_d1001_store_get(
                key.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
                &mut length,
            )
        };
        if result == ERR_NOT_FOUND {
            return Ok(None);
        }
        if result != 0 || length >= value.len() {
            return Err(if result == 0 { -1 } else { result });
        }
        value.truncate(length);
        String::from_utf8(value).map(Some).map_err(|_| -2)
    }

    pub fn store_set(key: &str, value: &str) -> Result<(), i32> {
        let key = CString::new(key).map_err(|_| -1)?;
        let value = CString::new(value).map_err(|_| -1)?;
        let result = unsafe { d1001::njorun_d1001_store_set(key.as_ptr(), value.as_ptr()) };
        (result == 0).then_some(()).ok_or(result)
    }

    pub fn store_erase() -> Result<(), i32> {
        let result = unsafe { d1001::njorun_d1001_store_erase_all() };
        (result == 0).then_some(()).ok_or(result)
    }

    pub fn restart() -> ! {
        unsafe { esp_idf_sys::esp_restart() }
    }

    pub const ERR_REJECTED: i32 = 0x10D;

    pub fn internal_heap() -> (usize, usize) {
        let caps = esp_idf_sys::MALLOC_CAP_INTERNAL | esp_idf_sys::MALLOC_CAP_8BIT;
        unsafe {
            (
                esp_idf_sys::heap_caps_get_free_size(caps),
                esp_idf_sys::heap_caps_get_largest_free_block(caps),
            )
        }
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

    pub fn local_ipv4() -> Result<Ipv4Addr, i32> {
        let mut octets = [0_u8; 4];
        let result = unsafe { njorun_d1001_wifi_ipv4(octets.as_mut_ptr()) };
        if result == 0 {
            Ok(Ipv4Addr::from(octets))
        } else {
            Err(result)
        }
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

    const ERR_INVALID_STATE: i32 = 0x103;
    const ERR_TIMEOUT: i32 = 0x107;

    pub fn ha_open(websocket_url: &str, auth_message: &str) -> Result<(), i32> {
        let url = CString::new(websocket_url).map_err(|_| -1)?;
        let auth = CString::new(auth_message).map_err(|_| -1)?;
        let result = unsafe { d1001::njorun_d1001_ha_session_open(url.as_ptr(), auth.as_ptr()) };
        (result == 0).then_some(()).ok_or(result)
    }

    pub fn ha_request(message: &str, buffer: &mut [u8], timeout_ms: u32) -> Result<usize, i32> {
        let message = CString::new(message).map_err(|_| -1)?;
        let mut length = 0;
        let result = unsafe {
            d1001::njorun_d1001_ha_session_request(
                message.as_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut length,
                timeout_ms,
            )
        };
        (result == 0).then_some(length).ok_or(result)
    }

    /// `Ok(None)` when no event arrived in time; `Err` once the session has ended.
    pub fn ha_poll_event(buffer: &mut [u8], timeout_ms: u32) -> Result<Option<usize>, i32> {
        let mut length = 0;
        let result = unsafe {
            d1001::njorun_d1001_ha_session_poll_event(
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut length,
                timeout_ms,
            )
        };
        match result {
            0 => Ok(Some(length)),
            ERR_TIMEOUT => Ok(None),
            other => Err(other),
        }
    }

    const CAMERA_MAX_BYTES: usize = 1536 * 1024;

    /// GET with a bearer token; HTTP 401 and 403 come back as `ERR_REJECTED`.
    pub fn http_get_bearer(url: &str, token: &str) -> Result<Vec<u8>, i32> {
        let url = CString::new(url).map_err(|_| -1)?;
        let token = CString::new(token).map_err(|_| -1)?;
        let mut body = vec![0_u8; CAMERA_MAX_BYTES];
        let mut length = 0;
        let result = unsafe {
            d1001::njorun_d1001_http_get_bearer(
                url.as_ptr(),
                token.as_ptr(),
                body.as_mut_ptr().cast(),
                body.len(),
                &mut length,
            )
        };
        if result != 0 || length >= body.len() {
            return Err(if result == 0 { -1 } else { result });
        }
        body.truncate(length);
        Ok(body)
    }

    pub fn ha_send(message: &str) -> Result<(), i32> {
        let message = CString::new(message).map_err(|_| -1)?;
        let result = unsafe { d1001::njorun_d1001_ha_session_send(message.as_ptr()) };
        (result == 0).then_some(()).ok_or(result)
    }

    pub fn internal_min_free() -> usize {
        let caps = esp_idf_sys::MALLOC_CAP_INTERNAL | esp_idf_sys::MALLOC_CAP_8BIT;
        unsafe { esp_idf_sys::heap_caps_get_minimum_free_size(caps) }
    }

    pub fn ha_events_lost() -> bool {
        unsafe { d1001::njorun_d1001_ha_session_take_events_lost() }
    }

    pub fn ha_close() {
        unsafe { d1001::njorun_d1001_ha_session_close() };
    }

    #[allow(dead_code)]
    pub const SESSION_ENDED: i32 = ERR_INVALID_STATE;
}

impl D1001Ui {
    fn new() -> Result<Self, String> {
        println!("Njörun D1001: initializing display");
        let framebuffer = board::display_framebuffer().map_err(|error| error.to_string())?;
        println!("Njörun D1001: framebuffer acquired");

        // Establish a visible, deterministic panel baseline before Slint does
        // its first full-frame render. This prevents an uninitialized scanout
        // while the commissioning component is being constructed.
        framebuffer.fill(Rgb565Pixel(0x0000));
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
        let commissioning = commissioning::State::booting();
        commissioning::render(&ui, &commissioning)?;
        println!("Njörun D1001: commissioning model rendered");
        let (action_sender, actions) = channel();
        let (ui_action_sender, ui_actions) = channel();
        commissioning::install_callbacks(&ui, action_sender.clone());
        ui.show().map_err(|error| error.to_string())?;
        println!("Njörun D1001: commissioning window shown");

        let mut result = Self {
            screen: Screen::Commissioning(ui),
            window,
            framebuffer,
            line_buffer: vec![Rgb565Pixel(0); board::WIDTH],
            pointer_pressed: false,
            pointer_position: LogicalPosition::default(),
            commissioning,
            authorization: None,
            restoring: None,
            session: None,
            host: String::new(),
            selected_room: None,
            details: None,
            dashboard_dirty: false,
            last_dashboard_render: std::time::Instant::now(),
            last_clock_check: std::time::Instant::now() - Duration::from_secs(2),
            clock_label: String::new(),
            camera_image: None,
            camera_in_flight: false,
            camera_last: std::time::Instant::now(),
            setup_problem: None,
            orientation: Orientation::default(),
            theme: "light".to_owned(),
            full_repaint: false,
            ui_action_sender,
            ui_actions,
            action_sender,
            actions,
        };
        println!("Njörun D1001: starting first frame render");
        result.render()?;
        println!("Njörun D1001: first frame rendered");
        board::touch_init().map_err(|error| error.to_string())?;
        Ok(result)
    }

    /// Reconnects with saved Wi-Fi and Home Assistant credentials instead of showing setup.
    fn begin_restore(
        &mut self,
        wifi: store::WifiCredentials,
        session: store::HomeAssistantSession,
    ) -> Result<(), String> {
        self.restoring = Some(session);
        let effect = commissioning::begin_restore(&mut self.commissioning, wifi);
        if let Screen::Commissioning(ui) = &self.screen {
            commissioning::render(ui, &self.commissioning)?;
        }
        self.start_effect(effect)?;
        self.render()
    }

    fn set_wifi_networks(&mut self, networks: Vec<board::WifiNetwork>) -> Result<(), String> {
        let german = commissioning::is_german(&self.commissioning);
        self.commissioning = commissioning::State::new(networks);
        commissioning::set_german(&mut self.commissioning, german);
        if let Some(problem) = self.setup_problem.take() {
            commissioning::set_setup_problem(&mut self.commissioning, problem);
        }
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
                Ok(commissioning::Action::RestoreFinished(Ok(()))) => {
                    if let Some(session) = self.restoring.take() {
                        self.start_session(session, None)?;
                    }
                }
                Ok(action) => {
                    let effect = commissioning::handle_action(
                        action,
                        &mut self.commissioning,
                        &mut self.authorization,
                    );
                    if let Screen::Commissioning(ui) = &self.screen {
                        commissioning::render(ui, &self.commissioning)?;
                    }
                    if let Some(effect) = effect {
                        self.start_effect(effect)?;
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

    fn start_effect(&self, effect: commissioning::Effect) -> Result<(), String> {
        let sender = self.action_sender.clone();
        let failure = commissioning::failed_action(&effect);
        let (free, largest) = board::internal_heap();
        println!("Njörun D1001 starting task, internal heap free {free} largest {largest}");
        let spawned = std::thread::Builder::new()
            .name("commissioning".to_owned())
            .stack_size(EFFECT_STACK_BYTES)
            .spawn(move || {
                let _ = sender.send(commissioning::run_effect(effect));
            });
        if let Err(error) = spawned {
            println!("Njörun D1001 could not start commissioning task: {error}");
            let _ = self.action_sender.send(failure);
        }
        Ok(())
    }

    fn poll_authorization(&mut self) -> Result<(), String> {
        if !matches!(self.screen, Screen::Commissioning(_)) {
            return Ok(());
        }
        let german = commissioning::is_german(&self.commissioning);
        match commissioning::poll_authorization(&mut self.authorization, german) {
            Ok(Some(signed_in)) => {
                self.start_session(signed_in.session, Some(signed_in.access_token))
            }
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

    /// Starts the Home Assistant session; the dashboard appears once its first snapshot lands.
    fn start_session(
        &mut self,
        session: store::HomeAssistantSession,
        access_token: Option<String>,
    ) -> Result<(), String> {
        commissioning::begin_loading(&mut self.commissioning, &session.url);
        self.host = self.commissioning_host();
        if let Screen::Commissioning(ui) = &self.screen {
            commissioning::render(ui, &self.commissioning)?;
        }
        match dashboard::Session::start(session, access_token) {
            Ok(started) => self.session = Some(started),
            Err(error) => commissioning::set_error(&mut self.commissioning, error),
        }
        Ok(())
    }

    fn commissioning_host(&self) -> String {
        commissioning::loading_host(&self.commissioning)
    }

    fn process_session(&mut self) -> Result<(), String> {
        let Some(session) = self.session.as_mut() else {
            return Ok(());
        };
        let mut changes = session.poll();
        if session.tick() {
            changes.push(dashboard::Change::Updated);
        }
        for change in changes {
            match change {
                dashboard::Change::FirstSnapshot => self.show_dashboard()?,
                dashboard::Change::Camera(entity_id, result) => {
                    self.camera_in_flight = false;
                    self.camera_last = std::time::Instant::now();
                    if let Ok(frame) = result {
                        let pixels = SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(
                            &frame.rgb,
                            frame.width,
                            frame.height,
                        );
                        self.camera_image = Some((entity_id, Image::from_rgb8(pixels)));
                    }
                    self.dashboard_dirty = true;
                }
                dashboard::Change::Updated | dashboard::Change::Theme => {
                    self.dashboard_dirty = true;
                }
                dashboard::Change::Offline(_) | dashboard::Change::DataError => {
                    if matches!(self.screen, Screen::Dashboard(_)) {
                        self.dashboard_dirty = true;
                    } else {
                        let problem = match change {
                            dashboard::Change::Offline(problem) => problem,
                            _ => ConnectionProblem::BadResponse,
                        };
                        commissioning::set_loading_error(&mut self.commissioning, Some(problem));
                        if let Screen::Commissioning(ui) = &self.screen {
                            commissioning::render(ui, &self.commissioning)?;
                        }
                    }
                }
                dashboard::Change::Rejected => {
                    self.session = None;
                    if matches!(self.screen, Screen::Dashboard(_)) {
                        store::clear_home_assistant();
                        board::restart();
                    }
                    let effect = commissioning::session_rejected(&mut self.commissioning);
                    if let Screen::Commissioning(ui) = &self.screen {
                        commissioning::render(ui, &self.commissioning)?;
                    }
                    self.start_effect(effect)?;
                    return Ok(());
                }
            }
        }
        if self.dashboard_dirty
            && self.last_dashboard_render.elapsed() >= Duration::from_millis(250)
        {
            self.render_dashboard();
        }
        self.update_clock();
        self.update_camera();
        Ok(())
    }

    /// While a camera's details sheet is open, fetches a new picture every few seconds.
    fn update_camera(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let camera = self.details.as_deref().filter(|id| {
            session
                .dashboard()
                .entity(id)
                .is_some_and(|tile| tile.domain == EntityDomain::Camera)
        });
        let Some(entity_id) = camera else {
            if self.camera_image.take().is_some() {
                self.dashboard_dirty = true;
            }
            return;
        };
        if !self.camera_in_flight && self.camera_last.elapsed() >= CAMERA_REFRESH {
            if session.fetch_camera(entity_id) {
                self.camera_in_flight = true;
                self.dashboard_dirty = true;
            } else {
                self.camera_last = std::time::Instant::now();
            }
        }
    }

    /// Shows local time: device UTC (NTP) plus the offset Home Assistant reports.
    fn update_clock(&mut self) {
        if self.last_clock_check.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_clock_check = std::time::Instant::now();
        let (Screen::Dashboard(ui), Some(session)) = (&self.screen, &self.session) else {
            return;
        };
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0));
        let german = commissioning::is_german(&self.commissioning);
        let label = session.utc_offset().map_or_else(
            || "--:--".to_owned(),
            |offset| presentation::local_clock_label(unix, offset, german),
        );
        if label != self.clock_label {
            ui.set_clock_time(label.as_str().into());
            self.clock_label = label;
        }
    }

    fn process_ui_actions(&mut self) {
        while let Ok(action) = self.ui_actions.try_recv() {
            match action {
                dashboard::UiAction::SelectRoom(index) => {
                    self.selected_room = self
                        .session
                        .as_ref()
                        .and_then(|session| dashboard::room_at(session.dashboard(), index));
                    self.details = None;
                    self.dashboard_dirty = true;
                }
                dashboard::UiAction::Activate(entity_id) => {
                    if let Some(session) = self.session.as_mut() {
                        if let dashboard::Tap::Inspect(id) = session.tap(&entity_id) {
                            self.details = Some(id);
                        }
                        self.dashboard_dirty = true;
                    }
                }
                dashboard::UiAction::Details(entity_id) => {
                    self.details = Some(entity_id);
                    self.dashboard_dirty = true;
                }
                dashboard::UiAction::Orientation(choice) => {
                    let next = match choice.as_str() {
                        "flip" => self.orientation.flipped(),
                        "landscape" => self.orientation.with_landscape(true),
                        _ => self.orientation.with_landscape(false),
                    };
                    if next != self.orientation {
                        store::save_orientation(next.key());
                        self.apply_orientation(next);
                        self.dashboard_dirty = true;
                    }
                }
                dashboard::UiAction::Theme(choice) => {
                    if ["light", "dark", "automatic"].contains(&choice.as_str()) {
                        store::save_theme(&choice);
                        self.theme = choice;
                        self.dashboard_dirty = true;
                    }
                }
                dashboard::UiAction::DismissDetails => {
                    self.details = None;
                    self.dashboard_dirty = true;
                }
                dashboard::UiAction::Detail(_, key, _) if key == "refresh_camera" => {
                    self.camera_last = std::time::Instant::now() - CAMERA_REFRESH;
                }
                dashboard::UiAction::Detail(entity_id, key, argument) => {
                    if let Some(session) = self.session.as_mut() {
                        session.detail_action(&entity_id, &key, &argument);
                        self.dashboard_dirty = true;
                    }
                }
            }
        }
    }

    fn render_dashboard(&mut self) {
        self.dashboard_dirty = false;
        self.last_dashboard_render = std::time::Instant::now();
        let dark_mode = self.dark_mode();
        let (Screen::Dashboard(ui), Some(session)) = (&self.screen, &self.session) else {
            return;
        };
        let german = commissioning::is_german(&self.commissioning);
        ui.set_dark_mode(dark_mode);
        ui.set_theme(self.theme.as_str().into());
        dashboard::render(
            ui,
            session.dashboard(),
            self.selected_room.as_ref(),
            self.details.as_deref(),
            german,
            &self.host,
            session.utc_offset(),
            &dashboard::CameraView {
                image: self
                    .camera_image
                    .as_ref()
                    .filter(|(id, _)| Some(id.as_str()) == self.details.as_deref())
                    .map(|(_, image)| image),
                loading: self.camera_in_flight,
            },
            session.problem(),
        );
    }

    fn show_dashboard(&mut self) -> Result<(), String> {
        if let Screen::Commissioning(ui) = &self.screen {
            ui.hide().map_err(|error| error.to_string())?;
        }
        let ui = MainWindow::new().map_err(|error| error.to_string())?;
        ui.set_home_view("rooms".into());
        ui.set_rooms_page(false);
        ui.set_network_link("wifi".into());
        ui.set_allow_forget(true);
        let orientation = self.ui_action_sender.clone();
        ui.on_set_orientation(move |choice| {
            let _ = orientation.send(dashboard::UiAction::Orientation(choice.to_string()));
        });
        let theme = self.ui_action_sender.clone();
        ui.on_set_theme(move |choice| {
            let _ = theme.send(dashboard::UiAction::Theme(choice.to_string()));
        });
        ui.on_forget_setup(|| {
            store::forget_all();
            board::restart();
        });
        for register in [
            Self::wire_room_selection as fn(&MainWindow, &Sender<dashboard::UiAction>),
            Self::wire_entities,
        ] {
            register(&ui, &self.ui_action_sender);
        }
        ui.show().map_err(|error| error.to_string())?;
        self.screen = Screen::Dashboard(ui);
        self.apply_orientation(self.orientation);
        self.render_dashboard_now();
        Ok(())
    }

    /// Sizes the logical screen for `orientation` and repaints everything; touches follow
    /// through `Orientation::to_logical` in `poll`.
    fn apply_orientation(&mut self, orientation: Orientation) {
        self.orientation = orientation;
        let (width, height) = orientation.logical_size(PANEL_WIDTH, PANEL_HEIGHT);
        self.window.set_size(PhysicalSize::new(width, height));
        match &self.screen {
            Screen::Commissioning(ui) => {
                ui.set_viewport_width(i32::try_from(width).unwrap_or(i32::MAX));
                ui.set_viewport_height(i32::try_from(height).unwrap_or(i32::MAX));
                ui.set_landscape(orientation.is_landscape());
            }
            Screen::Dashboard(ui) => {
                ui.set_orientation_landscape(orientation.is_landscape());
                ui.set_orientation_flipped(orientation.is_flipped());
            }
        }
        self.full_repaint = true;
        self.window.request_redraw();
    }

    /// Dark mode for the chosen theme; `automatic` follows Home Assistant's sun.
    fn dark_mode(&self) -> bool {
        match self.theme.as_str() {
            "dark" => true,
            "automatic" => self
                .session
                .as_ref()
                .and_then(dashboard::Session::daylight)
                .is_some_and(|up| !up),
            _ => false,
        }
    }

    fn render_dashboard_now(&mut self) {
        self.dashboard_dirty = true;
        self.last_dashboard_render = std::time::Instant::now() - Duration::from_secs(1);
        self.render_dashboard();
    }

    fn wire_room_selection(ui: &MainWindow, sender: &Sender<dashboard::UiAction>) {
        let room = sender.clone();
        ui.on_room_selected(move |index| {
            let _ = room.send(dashboard::UiAction::SelectRoom(index));
        });
        let home = sender.clone();
        ui.on_go_home(move || {
            let _ = home.send(dashboard::UiAction::SelectRoom(-1));
        });
        let rooms = sender.clone();
        ui.on_show_rooms(move || {
            let _ = rooms.send(dashboard::UiAction::SelectRoom(-1));
        });
    }

    fn wire_entities(ui: &MainWindow, sender: &Sender<dashboard::UiAction>) {
        let activate = sender.clone();
        ui.on_activate_entity(move |id| {
            let _ = activate.send(dashboard::UiAction::Activate(id.to_string()));
        });
        let details = sender.clone();
        ui.on_request_details(move |id| {
            let _ = details.send(dashboard::UiAction::Details(id.to_string()));
        });
        let dismiss = sender.clone();
        ui.on_dismiss_details(move || {
            let _ = dismiss.send(dashboard::UiAction::DismissDetails);
        });
        let detail = sender.clone();
        ui.on_detail_action(move |entity_id, key, argument| {
            let _ = detail.send(dashboard::UiAction::Detail(
                entity_id.to_string(),
                key.to_string(),
                argument.to_string(),
            ));
        });
    }

    fn render(&mut self) -> Result<(), String> {
        let started = std::time::Instant::now();
        let mut dirty = (0, 0);
        let rotation = match self.orientation.rotation_degrees() {
            90 => RenderingRotation::Rotate90,
            180 => RenderingRotation::Rotate180,
            270 => RenderingRotation::Rotate270,
            _ => RenderingRotation::NoRotation,
        };
        let full_repaint = std::mem::take(&mut self.full_repaint);
        let rendered = self.window.draw_if_needed(|renderer| {
            renderer.set_rendering_rotation(rotation);
            if full_repaint {
                // Switching the buffer type is the renderer's only way to drop its cache.
                renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
                renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
            }
            let region = renderer.render_by_line(LineCopier {
                framebuffer: &mut *self.framebuffer,
                line: &mut self.line_buffer,
            });
            let size = region.bounding_box_size();
            dirty = (size.width, size.height);
        });
        if rendered {
            let drawn = started.elapsed();
            board::display_sync().map_err(|error| error.to_string())?;
            if drawn.as_millis() >= 120 {
                println!(
                    "Njörun D1001 slow frame: dirty {}x{} drawn in {} ms",
                    dirty.0,
                    dirty.1,
                    drawn.as_millis()
                );
            }
        }
        Ok(())
    }

    fn poll(&mut self) -> Result<(), String> {
        let point = board::touch_read().map_err(|error| error.to_string())?;
        if point.pressed {
            let (x, y) = self.orientation.to_logical(
                u32::from(point.x),
                u32::from(point.y),
                PANEL_WIDTH,
                PANEL_HEIGHT,
            );
            #[allow(clippy::cast_precision_loss)]
            let position = LogicalPosition::new(x as f32, y as f32);
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
        self.process_session()?;
        self.process_ui_actions();
        slint::platform::update_timers_and_animations();
        self.render()
    }
}

const CAMERA_REFRESH: Duration = Duration::from_secs(3);
/// The D1001 panel is natively portrait.
const PANEL_WIDTH: u32 = board::WIDTH as u32;
const PANEL_HEIGHT: u32 = board::HEIGHT as u32;
const UI_STACK_BYTES: usize = 28 * 1024;
const EFFECT_STACK_BYTES: usize = 24 * 1024;

fn main() -> ! {
    // The ESP-IDF main task's stack can land in slow LP SRAM; run the UI in a
    // regular thread whose stack is allocated from fast L2 RAM.
    let ui_thread = std::thread::Builder::new()
        .name("ui".to_owned())
        .stack_size(UI_STACK_BYTES)
        .spawn(|| {
            ui_main();
        });
    if let Err(error) = ui_thread {
        println!("Njörun D1001 UI thread failed: {error}");
    }
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn ui_main() -> ! {
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
    let saved = store::load();
    let mut restoring = false;
    if let Some(ui) = &mut ui {
        if let Some(german) = saved.german {
            commissioning::set_german(&mut ui.commissioning, german);
        }
        if let Some(theme) =
            store::theme().filter(|theme| ["light", "dark", "automatic"].contains(&theme.as_str()))
        {
            ui.theme = theme;
        }
        let orientation = store::orientation()
            .as_deref()
            .and_then(Orientation::from_key)
            .unwrap_or_default();
        ui.apply_orientation(orientation);
        ui.setup_problem = saved.problem;
        if let (Some(wifi), Some(session)) = (saved.wifi.clone(), saved.home_assistant.clone()) {
            println!("Njörun D1001 restoring saved setup for {}", wifi.ssid);
            match ui.begin_restore(wifi, session) {
                Ok(()) => restoring = true,
                Err(error) => println!("Njörun D1001 restore could not start: {error}"),
            }
        }
    }
    let mut wifi_results = None;
    if !restoring {
        let (wifi_sender, receiver) = channel();
        let wifi_thread = std::thread::Builder::new()
            .name("wifi-scan".to_owned())
            .stack_size(16 * 1024)
            .spawn(move || {
                let _ = wifi_sender.send(board::scan_wifi());
            });
        if let Err(error) = wifi_thread {
            println!("Njörun D1001 Wi-Fi scan task failed: {error}");
        }
        wifi_results = Some(receiver);
    }

    loop {
        if let Some(receiver) = &wifi_results {
            match receiver.try_recv() {
                Ok(Ok(networks)) => {
                    let (free, largest) = board::internal_heap();
                    println!(
                        "Njörun D1001 Wi-Fi scan found {} networks, internal heap free {free} largest {largest}",
                        networks.len()
                    );
                    for network in &networks {
                        println!(
                            "Wi-Fi: {:?}, RSSI {}, password required: {}",
                            network.ssid, network.rssi, network.password_required
                        );
                    }
                    if let Some(ui) = &mut ui {
                        if let Err(error) = ui.set_wifi_networks(networks) {
                            println!("Njörun D1001 commissioning UI update failed: {error}");
                        }
                    }
                    wifi_results = None;
                }
                Ok(Err(error)) => {
                    println!("Njörun D1001 Wi-Fi scan failed: {error}");
                    wifi_results = None;
                }
                Err(TryRecvError::Disconnected) => {
                    println!("Njörun D1001 Wi-Fi scan task disconnected");
                    wifi_results = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(ui) = &mut ui {
            if let Err(error) = ui.poll() {
                println!("Njörun D1001 touch poll failed: {error}");
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
