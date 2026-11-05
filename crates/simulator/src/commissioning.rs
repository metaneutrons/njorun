//! Recoverable first-run state and native simulator presentation.
//!
//! The desktop fixture exercises the same decisions as the tablet flow. Wi-Fi
//! scanning, mDNS and browser authorization are deterministic adapters here and
//! remain hardware/network integration points for the D1001.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use njorun_ha_client::{
    ClientError, Credentials,
    browser_auth::BrowserAuthorization,
    discovery::{DiscoveredHomeAssistant, discover as discover_home_assistant},
    generate_identity, storage, websocket_url,
};
use qrcode::{QrCode, types::Color};
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

#[allow(unsafe_code)]
mod generated {
    slint::include_modules!();
}

use generated::{CommissioningWindow, InstanceItem, WifiItem};

/// Durable non-secret progress used by the simulator fixture.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SavedCommissioning {
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub wifi_connected: bool,
    pub wifi_name: String,
    pub ha_endpoint: String,
    pub complete: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Wifi,
    Password,
    Discovery,
    Manual,
    Browser,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserPresentation {
    Live,
    Fixture,
}

impl Stage {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Wifi => "wifi",
            Self::Password => "password",
            Self::Discovery => "discovery",
            Self::Manual => "manual",
            Self::Browser => "browser",
            Self::Complete => "complete",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WifiNetwork {
    name: String,
    signal: i32,
    secured: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiscoveredInstance {
    name: String,
    endpoint: String,
}

#[derive(Debug)]
struct Commissioning {
    stage: Stage,
    networks: Vec<WifiNetwork>,
    instances: Vec<DiscoveredInstance>,
    input: String,
    cursor: usize,
    selected_wifi: String,
    wifi_connected: bool,
    selected_instance: String,
    selected_endpoint: String,
    authorization_url: String,
    login_code: String,
    browser_presentation: BrowserPresentation,
    discovery_in_progress: bool,
    error: String,
    german: bool,
}

impl Commissioning {
    fn fixture(german: bool) -> Self {
        Self {
            stage: Stage::Wifi,
            networks: vec![
                WifiNetwork {
                    name: "Njörun Lab".into(),
                    signal: 5,
                    secured: true,
                },
                WifiNetwork {
                    name: "Guest network".into(),
                    signal: 3,
                    secured: true,
                },
                WifiNetwork {
                    name: "Open test network".into(),
                    signal: 2,
                    secured: false,
                },
            ],
            instances: Vec::new(),
            input: String::new(),
            cursor: 0,
            selected_wifi: String::new(),
            wifi_connected: false,
            selected_instance: String::new(),
            selected_endpoint: String::new(),
            authorization_url: String::new(),
            login_code: String::new(),
            browser_presentation: BrowserPresentation::Live,
            discovery_in_progress: false,
            error: String::new(),
            german,
        }
    }

    fn resume(saved: &SavedCommissioning) -> Self {
        let mut setup = Self::fixture(saved.language == "de");
        setup.selected_wifi.clone_from(&saved.wifi_name);
        setup.wifi_connected = saved.wifi_connected;
        setup.selected_endpoint.clone_from(&saved.ha_endpoint);
        if !saved.ha_endpoint.is_empty() {
            setup.selected_instance = "Home Assistant".into();
            setup.stage = Stage::Browser;
        } else if saved.wifi_connected {
            setup.discover();
        } else if !saved.wifi_name.is_empty() {
            setup.stage = Stage::Password;
        }
        setup
    }

    fn select_wifi(&mut self, index: usize) {
        let Some(network) = self.networks.get(index).cloned() else {
            return;
        };
        self.selected_wifi.clone_from(&network.name);
        self.error.clear();
        self.clear_input();
        if network.secured {
            self.wifi_connected = false;
            self.stage = Stage::Password;
        } else {
            self.wifi_connected = true;
            self.discover();
        }
    }

    fn discover(&mut self) {
        self.stage = Stage::Discovery;
        self.error.clear();
        self.instances.clear();
    }

    fn populate_fixture_instances(&mut self) {
        self.instances = vec![
            DiscoveredInstance {
                name: "Home".into(),
                endpoint: "http://homeassistant.local:8123".into(),
            },
            DiscoveredInstance {
                name: "Home Assistant · Office".into(),
                endpoint: "https://ha-office.example.test".into(),
            },
        ];
    }

    fn show_manual(&mut self) {
        self.stage = Stage::Manual;
        self.error.clear();
        self.clear_input();
    }

    fn select_instance(&mut self, index: usize) {
        let Some(instance) = self.instances.get(index) else {
            return;
        };
        self.selected_instance.clone_from(&instance.name);
        self.selected_endpoint.clone_from(&instance.endpoint);
        self.stage = Stage::Browser;
        self.error.clear();
        self.authorization_url.clear();
        self.login_code.clear();
    }

    fn submit(&mut self) {
        match self.stage {
            Stage::Password => {
                if self.input == "njorun-demo!" {
                    self.clear_input();
                    self.wifi_connected = true;
                    self.discover();
                } else {
                    self.error = self.localized(
                        "That password did not connect. Check it and try again.",
                        "Mit diesem Passwort konnte keine Verbindung hergestellt werden.",
                    );
                }
            }
            Stage::Manual => match websocket_url(&self.input, true) {
                Ok(_) => {
                    self.selected_instance = "Manual Home Assistant".into();
                    self.selected_endpoint.clone_from(&self.input);
                    self.stage = Stage::Browser;
                    self.error.clear();
                    self.authorization_url.clear();
                    self.login_code.clear();
                }
                Err(_) => {
                    self.error = self.localized(
                        "Enter a valid http:// or https:// Home Assistant address.",
                        "Gib eine gültige http://- oder https://-Adresse ein.",
                    );
                }
            },
            _ => {}
        }
    }

    fn browser_approved(&mut self) {
        if self.stage == Stage::Browser {
            self.stage = Stage::Complete;
        }
    }

    fn key(&mut self, key: &str) {
        match key {
            "backspace" => self.backspace(),
            "left" => self.cursor = self.previous_boundary(),
            "right" => self.cursor = self.next_boundary(),
            "submit" => self.submit(),
            value if value.chars().count() == 1 && self.input.len() < 2048 => self.insert(value),
            _ => {}
        }
    }

    fn insert(&mut self, value: &str) {
        self.input.insert_str(self.cursor, value);
        self.cursor += value.len();
        self.error.clear();
    }

    fn backspace(&mut self) {
        let previous = self.previous_boundary();
        if previous != self.cursor {
            self.input.replace_range(previous..self.cursor, "");
            self.cursor = previous;
        }
        self.error.clear();
    }

    fn previous_boundary(&self) -> usize {
        self.input[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next_boundary(&self) -> usize {
        self.input[self.cursor..]
            .char_indices()
            .nth(1)
            .map_or(self.input.len(), |(index, _)| self.cursor + index)
    }

    fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
    }

    fn localized(&self, english: &str, german: &str) -> String {
        if self.german { german } else { english }.into()
    }

    fn toggle_language(&mut self) {
        self.german = !self.german;
        self.error.clear();
    }

    fn saved(&self, complete: bool) -> SavedCommissioning {
        SavedCommissioning {
            language: if self.german { "de" } else { "en" }.into(),
            wifi_connected: self.wifi_connected,
            wifi_name: self.selected_wifi.clone(),
            ha_endpoint: self.selected_endpoint.clone(),
            complete,
        }
    }
}

#[derive(Clone)]
enum AuthorizationMode {
    Fixture,
    Live { credentials_path: PathBuf },
}

/// Return whether the simulator should skip setup after a completed prior run.
#[must_use]
pub fn is_complete(path: &Path) -> bool {
    storage::read_private::<SavedCommissioning>(path).is_ok_and(|state| state.complete)
}

/// Run the deterministic native first-run fixture.
///
/// # Errors
/// Returns a sanitized storage, QR-generation or Slint error.
pub fn run_fixture(
    state_path: &Path,
    width: u32,
    height: u32,
) -> Result<SavedCommissioning, String> {
    run_with_mode(state_path, width, height, &AuthorizationMode::Fixture)
}

/// Run first-use setup with the standard Home Assistant browser flow.
///
/// # Errors
/// Returns a sanitized storage, authorization, QR-generation or Slint error.
pub fn run_live(
    state_path: &Path,
    credentials_path: &Path,
    width: u32,
    height: u32,
) -> Result<SavedCommissioning, String> {
    run_with_mode(
        state_path,
        width,
        height,
        &AuthorizationMode::Live {
            credentials_path: credentials_path.to_path_buf(),
        },
    )
}

fn run_with_mode(
    state_path: &Path,
    width: u32,
    height: u32,
    mode: &AuthorizationMode,
) -> Result<SavedCommissioning, String> {
    let ui = CommissioningWindow::new().map_err(|error| error.to_string())?;
    let physical_preview = crate::physical_preview_requested();
    let (viewport_width, viewport_height) =
        crate::preview_dimensions(width, height, physical_preview);
    ui.set_viewport_width(
        i32::try_from(viewport_width).map_err(|_| "Simulator width is too large")?,
    );
    ui.set_viewport_height(
        i32::try_from(viewport_height).map_err(|_| "Simulator height is too large")?,
    );
    if physical_preview {
        ui.window()
            .set_size(slint::PhysicalSize::new(width, height));
    }
    ui.set_landscape(width > height);
    let saved = storage::read_private::<SavedCommissioning>(state_path).unwrap_or_default();
    let state = Rc::new(RefCell::new(Commissioning::resume(&saved)));
    let completed = Rc::new(RefCell::new(None));
    let (authorization_tx, authorization_rx) = mpsc::channel();
    let (discovery_tx, discovery_rx) = mpsc::channel();
    install_callbacks(
        &ui,
        &state,
        &completed,
        state_path.to_path_buf(),
        mode.clone(),
        authorization_tx.clone(),
        discovery_tx.clone(),
    );
    if state.borrow().stage == Stage::Discovery {
        begin_discovery(&mut state.borrow_mut(), mode, &discovery_tx);
    }
    if state.borrow().stage == Stage::Browser {
        begin_authorization(&mut state.borrow_mut(), mode, &authorization_tx);
    }
    let weak = ui.as_weak();
    let state_for_authorization = Rc::clone(&state);
    let completed_for_authorization = Rc::clone(&completed);
    let completion_path = state_path.to_path_buf();
    let authorization_timer = slint::Timer::default();
    authorization_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            if let Ok(result) = authorization_rx.try_recv() {
                apply_authorization_result(&mut state_for_authorization.borrow_mut(), result);
                if !complete_if_ready(
                    &weak,
                    &state_for_authorization,
                    &completed_for_authorization,
                    &completion_path,
                ) {
                    render_if_alive(&weak, &state_for_authorization);
                }
            }
            if let Ok(result) = discovery_rx.try_recv() {
                apply_discovery_result(&mut state_for_authorization.borrow_mut(), result);
                render_if_alive(&weak, &state_for_authorization);
            }
        },
    );
    render(&ui, &state.borrow())?;
    ui.run().map_err(|error| error.to_string())?;
    completed
        .borrow_mut()
        .take()
        .ok_or_else(|| "First-run simulator closed before setup completed".into())
}

fn apply_authorization_result(state: &mut Commissioning, result: Result<(), ClientError>) {
    match result {
        Ok(()) => state.browser_approved(),
        Err(error) => {
            state.error = state.localized(
                "Browser sign-in failed or expired. Choose another installation and try again.",
                "Die Browser-Anmeldung ist fehlgeschlagen oder abgelaufen. Wähle eine andere Installation und versuche es erneut.",
            );
            if error == ClientError::StorageCommitUncertain {
                state.error = state.localized(
                    "Sign-in succeeded, but credential durability is unconfirmed.",
                    "Die Anmeldung war erfolgreich, aber die dauerhafte Speicherung ist nicht bestätigt.",
                );
            }
        }
    }
}

fn apply_discovery_result(
    state: &mut Commissioning,
    result: Result<Vec<DiscoveredHomeAssistant>, ClientError>,
) {
    state.discovery_in_progress = false;
    if state.stage != Stage::Discovery {
        return;
    }
    match result {
        Ok(instances) if instances.is_empty() => {
            state.error = state.localized(
                "No installation found. Rescan or enter its URL.",
                "Keine Installation gefunden. Suche erneut oder gib ihre URL ein.",
            );
        }
        Ok(instances) => {
            state.instances = instances
                .into_iter()
                .map(|instance| DiscoveredInstance {
                    name: instance.name,
                    endpoint: instance.endpoint,
                })
                .collect();
            state.error.clear();
        }
        Err(_) => {
            state.error = state.localized(
                "Local discovery is unavailable. Enter the URL manually.",
                "Die lokale Suche ist nicht verfügbar. Gib die URL manuell ein.",
            );
        }
    }
}

fn install_callbacks(
    ui: &CommissioningWindow,
    state: &Rc<RefCell<Commissioning>>,
    completed: &Rc<RefCell<Option<SavedCommissioning>>>,
    state_path: std::path::PathBuf,
    authorization_mode: AuthorizationMode,
    authorization_tx: Sender<Result<(), njorun_ha_client::ClientError>>,
    discovery_tx: Sender<Result<Vec<DiscoveredHomeAssistant>, ClientError>>,
) {
    let weak = ui.as_weak();
    let state_for_wifi = Rc::clone(state);
    let wifi_path = state_path.clone();
    let wifi_mode = authorization_mode.clone();
    let wifi_discovery_tx = discovery_tx.clone();
    ui.on_wifi_selected(move |index| {
        let mut state = state_for_wifi.borrow_mut();
        state.select_wifi(usize::try_from(index).unwrap_or(usize::MAX));
        persist_or_report(&wifi_path, &mut state);
        if state.stage == Stage::Discovery {
            begin_discovery(&mut state, &wifi_mode, &wifi_discovery_tx);
        }
        drop(state);
        render_if_alive(&weak, &state_for_wifi);
    });

    let weak = ui.as_weak();
    let state_for_instance = Rc::clone(state);
    let instance_path = state_path.clone();
    let instance_mode = authorization_mode.clone();
    let instance_tx = authorization_tx.clone();
    ui.on_instance_selected(move |index| {
        let mut state = state_for_instance.borrow_mut();
        state.select_instance(usize::try_from(index).unwrap_or(usize::MAX));
        persist_or_report(&instance_path, &mut state);
        if state.stage == Stage::Browser {
            begin_authorization(&mut state, &instance_mode, &instance_tx);
        }
        drop(state);
        render_if_alive(&weak, &state_for_instance);
    });

    let weak = ui.as_weak();
    let state_for_manual = Rc::clone(state);
    ui.on_manual_url(move || {
        state_for_manual.borrow_mut().show_manual();
        render_if_alive(&weak, &state_for_manual);
    });

    let weak = ui.as_weak();
    let state_for_retry = Rc::clone(state);
    let retry_mode = authorization_mode.clone();
    let retry_discovery_tx = discovery_tx.clone();
    ui.on_retry(move || {
        let mut state = state_for_retry.borrow_mut();
        state.authorization_url.clear();
        state.login_code.clear();
        state.discover();
        begin_discovery(&mut state, &retry_mode, &retry_discovery_tx);
        drop(state);
        render_if_alive(&weak, &state_for_retry);
    });

    let weak = ui.as_weak();
    let state_for_key = Rc::clone(state);
    let key_path = state_path.clone();
    let key_mode = authorization_mode;
    let key_tx = authorization_tx;
    let key_discovery_tx = discovery_tx;
    ui.on_key_pressed(move |key| {
        let mut state = state_for_key.borrow_mut();
        let previous_stage = state.stage;
        state.key(key.as_str());
        if state.stage != previous_stage {
            persist_or_report(&key_path, &mut state);
            if state.stage == Stage::Browser {
                begin_authorization(&mut state, &key_mode, &key_tx);
            } else if state.stage == Stage::Discovery {
                begin_discovery(&mut state, &key_mode, &key_discovery_tx);
            }
        }
        drop(state);
        render_if_alive(&weak, &state_for_key);
    });

    let weak = ui.as_weak();
    let state_for_browser = Rc::clone(state);
    let completed_for_browser = Rc::clone(completed);
    let browser_path = state_path.clone();
    ui.on_browser_login(move || {
        state_for_browser.borrow_mut().browser_approved();
        if !complete_if_ready(
            &weak,
            &state_for_browser,
            &completed_for_browser,
            &browser_path,
        ) {
            render_if_alive(&weak, &state_for_browser);
        }
    });

    install_language_callback(ui, state, state_path);
}

fn install_language_callback(
    ui: &CommissioningWindow,
    state: &Rc<RefCell<Commissioning>>,
    state_path: PathBuf,
) {
    let weak = ui.as_weak();
    let state_for_language = Rc::clone(state);
    let language_path = state_path;
    ui.on_toggle_language(move || {
        let mut state = state_for_language.borrow_mut();
        state.toggle_language();
        persist_or_report(&language_path, &mut state);
        drop(state);
        render_if_alive(&weak, &state_for_language);
    });
}

fn complete_if_ready(
    ui: &slint::Weak<CommissioningWindow>,
    state: &Rc<RefCell<Commissioning>>,
    completed: &Rc<RefCell<Option<SavedCommissioning>>>,
    state_path: &Path,
) -> bool {
    let saved = {
        let state = state.borrow();
        if state.stage != Stage::Complete {
            return false;
        }
        state.saved(true)
    };
    if storage::write(state_path, &saved).is_err() {
        let mut state = state.borrow_mut();
        state.stage = Stage::Browser;
        state.error = state.localized(
            "Sign-in succeeded, but setup completion could not be saved.",
            "Die Anmeldung war erfolgreich, aber der Abschluss konnte nicht gespeichert werden.",
        );
        return false;
    }
    completed.replace(Some(saved));
    if let Some(ui) = ui.upgrade() {
        let _ = ui.hide();
    }
    true
}

fn begin_discovery(
    state: &mut Commissioning,
    mode: &AuthorizationMode,
    result: &Sender<Result<Vec<DiscoveredHomeAssistant>, ClientError>>,
) {
    state.discover();
    match mode {
        AuthorizationMode::Fixture => state.populate_fixture_instances(),
        AuthorizationMode::Live { .. } if !state.discovery_in_progress => {
            state.discovery_in_progress = true;
            let result = result.clone();
            std::thread::spawn(move || {
                let discovered = discover_home_assistant(Duration::from_secs(3));
                let _ = result.send(discovered);
            });
        }
        AuthorizationMode::Live { .. } => {}
    }
}

fn begin_authorization(
    state: &mut Commissioning,
    mode: &AuthorizationMode,
    result: &Sender<Result<(), njorun_ha_client::ClientError>>,
) {
    state.error.clear();
    match mode {
        AuthorizationMode::Fixture => {
            state.browser_presentation = BrowserPresentation::Fixture;
            state.login_code = "NJRN-4821".into();
            state.authorization_url = format!(
                "{}/auth/authorize?client_id=http%3A%2F%2Fnjorun-demo.local%2F&redirect_uri=http%3A%2F%2Fnjorun-demo.local%2Fcallback&state=fixture-state&code_challenge=fixture-pkce&code_challenge_method=S256&response_type=code",
                state.selected_endpoint.trim_end_matches('/')
            );
        }
        AuthorizationMode::Live { credentials_path } => {
            state.browser_presentation = BrowserPresentation::Live;
            let allow_http = state.selected_endpoint.starts_with("http://");
            match BrowserAuthorization::start(&state.selected_endpoint, allow_http) {
                Ok(authorization) => {
                    authorization
                        .authorization_url()
                        .clone_into(&mut state.authorization_url);
                    authorization.login_code().clone_into(&mut state.login_code);
                    let endpoint = state.selected_endpoint.clone();
                    let credentials_path = credentials_path.clone();
                    let result = result.clone();
                    std::thread::spawn(move || {
                        let completed = complete_authorization(
                            authorization,
                            &endpoint,
                            allow_http,
                            &credentials_path,
                        );
                        let _ = result.send(completed);
                    });
                }
                Err(error) => {
                    state.error = error.to_string();
                }
            }
        }
    }
}

fn complete_authorization(
    authorization: BrowserAuthorization,
    endpoint: &str,
    allow_http: bool,
    credentials_path: &Path,
) -> Result<(), njorun_ha_client::ClientError> {
    let tokens = authorization.wait_and_exchange(Duration::from_secs(300))?;
    let identity = generate_identity()?;
    let credentials = Credentials {
        ha_url: endpoint.to_owned(),
        access_token: tokens.access_token,
        refresh_token: Some(tokens.refresh_token),
        client_id: Some(tokens.client_id),
        allow_http,
        tablet_id: "njorun-simulator".into(),
        binding_id: String::new(),
        tablet_private_key: identity.private_key,
        ha_public_key: String::new(),
    };
    storage::write(credentials_path, &credentials)
}

fn persist_or_report(path: &Path, state: &mut Commissioning) {
    if storage::write(path, &state.saved(false)).is_err() {
        state.error = state.localized(
            "Setup progress could not be saved.",
            "Der Einrichtungsfortschritt konnte nicht gespeichert werden.",
        );
    }
}

fn render_if_alive(ui: &slint::Weak<CommissioningWindow>, state: &Rc<RefCell<Commissioning>>) {
    if let Some(ui) = ui.upgrade() {
        let _ = render(&ui, &state.borrow());
    }
}

fn render(ui: &CommissioningWindow, state: &Commissioning) -> Result<(), String> {
    ui.set_stage(state.stage.as_str().into());
    ui.set_wifi_networks(ModelRc::new(VecModel::from(
        state
            .networks
            .iter()
            .map(|network| WifiItem {
                name: network.name.clone().into(),
                signal: network.signal,
                secured: network.secured,
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_instances(ModelRc::new(VecModel::from(
        state
            .instances
            .iter()
            .map(|instance| InstanceItem {
                name: instance.name.clone().into(),
                endpoint: instance.endpoint.clone().into(),
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_input_display_value(if state.stage == Stage::Password {
        "•".repeat(state.input.chars().count()).into()
    } else {
        state.input.clone().into()
    });
    ui.set_selected_wifi(state.selected_wifi.clone().into());
    ui.set_selected_instance(state.selected_instance.clone().into());
    ui.set_error_message(state.error.clone().into());
    ui.set_demo_controls(state.browser_presentation == BrowserPresentation::Fixture);
    ui.set_german(state.german);
    ui.set_login_code(SharedString::from(state.login_code.clone()));
    if state.stage == Stage::Browser && !state.authorization_url.is_empty() {
        let code = QrCode::new(state.authorization_url.as_bytes())
            .map_err(|_| "Could not build setup QR code")?;
        ui.set_qr_size(i32::try_from(code.width()).map_err(|_| "QR code is too large")?);
        ui.set_qr_modules(ModelRc::new(VecModel::from(
            code.to_colors()
                .into_iter()
                .map(|color| color == Color::Dark)
                .collect::<Vec<_>>(),
        )));
    } else {
        ui.set_qr_size(0);
        ui.set_qr_modules(ModelRc::default());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AuthorizationMode, Commissioning, CommissioningWindow, SavedCommissioning, Stage,
        apply_authorization_result, apply_discovery_result, begin_authorization, begin_discovery,
        complete_if_ready, install_callbacks, is_complete, persist_or_report, render,
    };
    use njorun_ha_client::{ClientError, discovery::DiscoveredHomeAssistant, storage};
    use slint::{ComponentHandle, Model};
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn password_manual_url_and_cursor_editing_are_recoverable() {
        let mut setup = Commissioning::fixture(false);
        setup.select_wifi(0);
        assert_eq!(setup.stage, Stage::Password);
        for key in ["n", "j", "o", "r", "u", "n", "-", "d", "e", "m", "o", "!"] {
            setup.key(key);
        }
        setup.key("left");
        setup.key("backspace");
        setup.key("submit");
        assert_eq!(setup.stage, Stage::Password);
        assert_ne!(setup.error, "");
        setup.clear_input();
        setup.insert("njorun-demo!");
        setup.submit();
        assert_eq!(setup.stage, Stage::Discovery);

        setup.show_manual();
        setup.clear_input();
        setup.insert("not a URL");
        setup.submit();
        assert_eq!(setup.stage, Stage::Manual);
        setup.clear_input();
        setup.insert("http://homeassistant.local:8123");
        setup.submit();
        assert_eq!(setup.stage, Stage::Browser);
        setup.browser_approved();
        assert_eq!(setup.stage, Stage::Complete);
    }

    #[test]
    fn setup_resumes_without_persisting_the_password() {
        let mut setup = Commissioning::fixture(false);
        setup.select_wifi(0);
        setup.insert("secret password");
        let saved = setup.saved(false);
        let resumed = Commissioning::resume(&saved);

        assert_eq!(resumed.stage, Stage::Password);
        assert_eq!(resumed.selected_wifi, "Njörun Lab");
        assert_eq!(resumed.input, "");
        assert_eq!(saved.wifi_name, "Njörun Lab");
    }

    #[test]
    fn language_callback_releases_state_before_rendering() {
        i_slint_backend_testing::init_no_event_loop();
        let path = std::env::temp_dir().join(format!(
            "njorun-language-callback-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let ui = CommissioningWindow::new().expect("commissioning window");
        let state = Rc::new(RefCell::new(Commissioning::fixture(false)));
        let completed = Rc::new(RefCell::new(None));
        let (authorization_tx, _authorization_rx) = std::sync::mpsc::channel();
        let (discovery_tx, _discovery_rx) = std::sync::mpsc::channel();
        install_callbacks(
            &ui,
            &state,
            &completed,
            path.clone(),
            AuthorizationMode::Fixture,
            authorization_tx,
            discovery_tx,
        );
        render(&ui, &state.borrow()).expect("initial render");

        ui.invoke_toggle_language();

        assert!(ui.get_german());
        let saved = storage::read_private::<SavedCommissioning>(&path).expect("saved language");
        assert_eq!(saved.language, "de");
        assert!(!saved.complete);
        std::fs::remove_file(path).expect("remove test state");
    }

    #[test]
    fn fixture_callbacks_complete_in_portrait_and_landscape() {
        i_slint_backend_testing::init_no_event_loop();

        for (orientation, width, height) in [("portrait", 800, 1280), ("landscape", 1280, 800)] {
            let path = std::env::temp_dir().join(format!(
                "njorun-first-run-{orientation}-{}-{}.json",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock after epoch")
                    .as_nanos()
            ));
            let ui = CommissioningWindow::new().expect("commissioning window");
            ui.set_viewport_width(i32::try_from(width).expect("test width fits i32"));
            ui.set_viewport_height(i32::try_from(height).expect("test height fits i32"));
            ui.window()
                .set_size(slint::PhysicalSize::new(width, height));
            let state = Rc::new(RefCell::new(Commissioning::fixture(false)));
            let completed = Rc::new(RefCell::new(None));
            let (authorization_tx, _authorization_rx) = std::sync::mpsc::channel();
            let (discovery_tx, _discovery_rx) = std::sync::mpsc::channel();
            install_callbacks(
                &ui,
                &state,
                &completed,
                path.clone(),
                AuthorizationMode::Fixture,
                authorization_tx,
                discovery_tx,
            );
            render(&ui, &state.borrow()).expect("initial render");

            ui.invoke_wifi_selected(2);
            assert_eq!(state.borrow().stage, Stage::Discovery);
            assert_eq!(state.borrow().instances.len(), 2);

            ui.invoke_instance_selected(0);
            assert_eq!(state.borrow().stage, Stage::Browser);
            assert!(
                state
                    .borrow()
                    .authorization_url
                    .contains("/auth/authorize?")
            );
            let qr_size = usize::try_from(ui.get_qr_size()).expect("positive QR size");
            assert!(qr_size > 0);
            assert_eq!(ui.get_qr_modules().row_count(), qr_size * qr_size);

            ui.invoke_browser_login();
            assert_eq!(state.borrow().stage, Stage::Complete);

            let saved = completed
                .borrow()
                .clone()
                .expect("completed commissioning state");
            assert!(saved.complete);
            assert_eq!(saved.wifi_name, "Open test network");
            assert_eq!(saved.ha_endpoint, "http://homeassistant.local:8123");
            let persisted = storage::read_private::<SavedCommissioning>(&path)
                .expect("persisted commissioning state");
            assert_eq!(persisted, saved);
            std::fs::remove_file(path).expect("remove test state");
        }
    }

    #[test]
    fn recovery_edge_cases_and_async_results_are_explicit() {
        let resumed_browser = Commissioning::resume(&SavedCommissioning {
            language: "de".into(),
            wifi_connected: true,
            wifi_name: "Home Wi-Fi".into(),
            ha_endpoint: "https://ha.example.test".into(),
            complete: false,
        });
        assert_eq!(resumed_browser.stage, Stage::Browser);
        assert_eq!(resumed_browser.selected_instance, "Home Assistant");
        assert!(resumed_browser.german);

        let resumed_discovery = Commissioning::resume(&SavedCommissioning {
            wifi_connected: true,
            wifi_name: "Home Wi-Fi".into(),
            ..SavedCommissioning::default()
        });
        assert_eq!(resumed_discovery.stage, Stage::Discovery);

        let mut setup = Commissioning::fixture(false);
        setup.select_wifi(usize::MAX);
        setup.select_instance(usize::MAX);
        setup.submit();
        setup.browser_approved();
        assert_eq!(setup.stage, Stage::Wifi);

        setup.input = "åb".into();
        setup.cursor = 0;
        setup.key("right");
        assert_eq!(setup.cursor, "å".len());
        setup.key("right");
        assert_eq!(setup.cursor, setup.input.len());
        setup.key("ignored");
        assert_eq!(setup.input, "åb");

        setup.stage = Stage::Browser;
        apply_authorization_result(&mut setup, Err(ClientError::Connection));
        assert!(setup.error.starts_with("Browser sign-in failed"));
        apply_authorization_result(&mut setup, Err(ClientError::StorageCommitUncertain));
        assert!(setup.error.starts_with("Sign-in succeeded"));
        apply_authorization_result(&mut setup, Ok(()));
        assert_eq!(setup.stage, Stage::Complete);

        setup.stage = Stage::Discovery;
        setup.discovery_in_progress = true;
        apply_discovery_result(&mut setup, Ok(Vec::new()));
        assert!(setup.error.starts_with("No installation found"));
        apply_discovery_result(
            &mut setup,
            Ok(vec![DiscoveredHomeAssistant {
                name: "Home".into(),
                endpoint: "http://homeassistant.local:8123".into(),
            }]),
        );
        assert_eq!(setup.instances.len(), 1);
        assert_eq!(setup.instances[0].name, "Home");
        assert_eq!(setup.error, "");
        apply_discovery_result(&mut setup, Err(ClientError::Connection));
        assert!(setup.error.starts_with("Local discovery is unavailable"));

        setup.stage = Stage::Browser;
        setup.discovery_in_progress = true;
        let previous = setup.instances.clone();
        apply_discovery_result(&mut setup, Ok(Vec::new()));
        assert_eq!(setup.instances, previous);
        assert!(!setup.discovery_in_progress);
    }

    #[test]
    fn callbacks_cover_manual_and_password_paths() {
        i_slint_backend_testing::init_no_event_loop();
        let path = std::env::temp_dir().join(format!(
            "njorun-callback-paths-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let ui = CommissioningWindow::new().expect("commissioning window");
        let state = Rc::new(RefCell::new(Commissioning::fixture(false)));
        let completed = Rc::new(RefCell::new(None));
        let (authorization_tx, _authorization_rx) = std::sync::mpsc::channel();
        let (discovery_tx, _discovery_rx) = std::sync::mpsc::channel();
        install_callbacks(
            &ui,
            &state,
            &completed,
            path.clone(),
            AuthorizationMode::Fixture,
            authorization_tx,
            discovery_tx,
        );
        render(&ui, &state.borrow()).expect("initial render");

        ui.invoke_manual_url();
        assert_eq!(state.borrow().stage, Stage::Manual);
        assert_eq!(state.borrow().input, "");
        state.borrow_mut().clear_input();
        state.borrow_mut().insert("invalid");
        ui.invoke_key_pressed("submit".into());
        assert_eq!(state.borrow().stage, Stage::Manual);
        assert_ne!(state.borrow().error, "");

        state.borrow_mut().clear_input();
        state.borrow_mut().insert("http://homeassistant.local:8123");
        ui.invoke_key_pressed("submit".into());
        assert_eq!(state.borrow().stage, Stage::Browser);
        assert_ne!(state.borrow().authorization_url, "");
        ui.invoke_retry();
        assert_eq!(state.borrow().stage, Stage::Discovery);
        assert_eq!(state.borrow().instances.len(), 2);

        ui.invoke_wifi_selected(0);
        assert_eq!(state.borrow().stage, Stage::Password);
        state.borrow_mut().insert("njorun-demo!");
        render(&ui, &state.borrow()).expect("password render");
        assert_eq!(ui.get_input_display_value(), "••••••••••••");
        ui.invoke_key_pressed("submit".into());
        assert_eq!(state.borrow().stage, Stage::Discovery);
        assert_eq!(state.borrow().instances.len(), 2);

        std::fs::remove_file(path).expect("remove test state");
    }

    #[test]
    fn completion_and_live_adapter_failures_are_reported() {
        let root = std::env::temp_dir().join(format!(
            "njorun-storage-contract-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir(&root).expect("create test directory");
        let state_path = root.join("state.json");
        assert!(!is_complete(&state_path));
        storage::write(
            &state_path,
            &SavedCommissioning {
                complete: true,
                ..SavedCommissioning::default()
            },
        )
        .expect("write completed state");
        assert!(is_complete(&state_path));

        let mut setup = Commissioning::fixture(true);
        persist_or_report(&root, &mut setup);
        assert!(setup.error.starts_with("Der Einrichtungsfortschritt"));

        i_slint_backend_testing::init_no_event_loop();
        let ui = CommissioningWindow::new().expect("commissioning window");
        let failed_state = Rc::new(RefCell::new(Commissioning::fixture(true)));
        failed_state.borrow_mut().stage = Stage::Complete;
        let completed = Rc::new(RefCell::new(None));
        assert!(!complete_if_ready(
            &ui.as_weak(),
            &failed_state,
            &completed,
            &root,
        ));
        assert_eq!(failed_state.borrow().stage, Stage::Browser);
        assert!(
            failed_state
                .borrow()
                .error
                .starts_with("Die Anmeldung war erfolgreich")
        );
        assert!(completed.borrow().is_none());

        setup.stage = Stage::Browser;
        setup.selected_endpoint = "not a Home Assistant URL".into();
        let (authorization_tx, _authorization_rx) = std::sync::mpsc::channel();
        begin_authorization(
            &mut setup,
            &AuthorizationMode::Live {
                credentials_path: root.join("credentials.json"),
            },
            &authorization_tx,
        );
        assert_ne!(setup.error, "");

        setup.discovery_in_progress = true;
        let (discovery_tx, _discovery_rx) = std::sync::mpsc::channel();
        begin_discovery(
            &mut setup,
            &AuthorizationMode::Live {
                credentials_path: root.join("credentials.json"),
            },
            &discovery_tx,
        );
        assert_eq!(setup.stage, Stage::Discovery);
        assert!(setup.discovery_in_progress);

        std::fs::remove_dir_all(root).expect("remove test directory");
    }
}
