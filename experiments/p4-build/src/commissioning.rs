use std::sync::mpsc::Sender;
use std::time::Duration;

use serde::Deserialize;
use slint::{ModelRc, VecModel};
use url::Url;

use njorun_app_core::problems::{ConnectionProblem, SetupProblem};

use crate::board;
use crate::generated::{CommissioningWindow, InstanceItem, WifiItem};
use crate::pairing;
use crate::store::{self, HomeAssistantSession, WifiCredentials};

/// Wait between two announcements while the tablet waits for approval.
const PAIR_EVERY_MS: u32 = 10_000;
/// Wait before the next announcement after something an administrator has to fix.
const PAIR_RETRY_MS: u32 = 30_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Boot,
    Wifi,
    Password,
    Connecting,
    Discovering,
    Discovery,
    Manual,
    Authorizing,
    Pairing,
    Loading,
}

impl Stage {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Boot => "boot",
            Self::Wifi => "wifi",
            Self::Password => "password",
            Self::Connecting => "connecting",
            Self::Discovering => "discovering",
            Self::Discovery => "discovery",
            Self::Manual => "manual",
            Self::Authorizing => "authorizing",
            Self::Pairing => "pairing",
            Self::Loading => "loading",
        }
    }
}

pub(crate) enum Action {
    WifiSelected(i32),
    InstanceSelected(i32),
    ManualUrl,
    Retry,
    Key(String),
    CursorPositioned(i32),
    ToggleLanguage,
    WifiFinished(Result<(), i32>),
    DiscoveryFinished(Result<Vec<board::HaInstance>, i32>),
    /// One announcement to Home Assistant (plan 004); the number tells which pairing it belongs to.
    PairingFinished(u32, Result<pairing::Round, pairing::Failure>),
    ScanFinished(Result<Vec<board::WifiNetwork>, i32>),
    RestoreFinished(Result<(), RestoreError>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestoreError {
    Wifi,
    HomeAssistantUnreachable(ConnectionProblem),
    HomeAssistantRejected,
}

pub(crate) enum Effect {
    ConnectWifi {
        ssid: String,
        password: String,
    },
    Discover,
    Pair {
        endpoint: String,
        need_grant: bool,
        wait_ms: u32,
        generation: u32,
    },
    ScanWifi,
    Restore {
        wifi: WifiCredentials,
    },
}

pub(crate) fn failed_action(effect: &Effect) -> Action {
    match effect {
        Effect::ConnectWifi { .. } => Action::WifiFinished(Err(-1)),
        Effect::Discover => Action::DiscoveryFinished(Err(-1)),
        Effect::Pair { generation, .. } => {
            Action::PairingFinished(*generation, Err(pairing::Failure::BadAnswer))
        }
        Effect::ScanWifi => Action::ScanFinished(Err(-1)),
        Effect::Restore { .. } => Action::RestoreFinished(Err(RestoreError::Wifi)),
    }
}

pub(crate) struct State {
    stage: Stage,
    networks: Vec<board::WifiNetwork>,
    instances: Vec<board::HaInstance>,
    selected_wifi: String,
    selected_instance: String,
    selected_endpoint: String,
    input: String,
    cursor: usize,
    error: String,
    german: bool,
    /// The stage to go back to when the address cannot be used.
    pairing_return: Stage,
    pairing_name: String,
    pairing_code: String,
    /// Whether the tablet has already asked for a grant after an approval without one.
    asked_grant: bool,
    /// Counts the pairings so that a late answer of an abandoned one is dropped.
    pairing_generation: u32,
}

impl State {
    pub(crate) fn booting() -> Self {
        let mut state = Self::new(Vec::new());
        state.stage = Stage::Boot;
        state
    }

    pub(crate) fn new(networks: Vec<board::WifiNetwork>) -> Self {
        Self {
            stage: Stage::Wifi,
            networks,
            instances: Vec::new(),
            selected_wifi: String::new(),
            selected_instance: String::new(),
            selected_endpoint: String::new(),
            input: String::new(),
            cursor: 0,
            error: String::new(),
            german: false,
            pairing_return: Stage::Discovery,
            pairing_name: String::new(),
            pairing_code: String::new(),
            asked_grant: false,
            pairing_generation: 0,
        }
    }

    fn localized(&self, english: &str, german: &str) -> String {
        if self.german { german } else { english }.to_owned()
    }

    fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
    }

    fn position_cursor(&mut self, offset: i32) {
        let mut offset = usize::try_from(offset)
            .unwrap_or_default()
            .min(self.input.len());
        while !self.input.is_char_boundary(offset) {
            offset = offset.saturating_sub(1);
        }
        self.cursor = offset;
    }

    fn key(&mut self, key: &str) -> bool {
        match key {
            "backspace" => {
                let previous = self.previous_boundary();
                if previous != self.cursor {
                    self.input.replace_range(previous..self.cursor, "");
                    self.cursor = previous;
                }
            }
            "left" => self.cursor = self.previous_boundary(),
            "right" => self.cursor = self.next_boundary(),
            "submit" => return true,
            value if value.chars().count() == 1 && self.input.len() < 2048 => {
                self.input.insert_str(self.cursor, value);
                self.cursor += value.len();
            }
            _ => {}
        }
        self.error.clear();
        false
    }

    /// The host of the chosen Home Assistant address, for the texts of a problem.
    fn host_of_endpoint(&self) -> String {
        self.selected_endpoint
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_owned()
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
}

pub(crate) fn install_callbacks(ui: &CommissioningWindow, actions: Sender<Action>) {
    let sender = actions.clone();
    ui.on_wifi_selected(move |index| {
        let _ = sender.send(Action::WifiSelected(index));
    });
    let sender = actions.clone();
    ui.on_instance_selected(move |index| {
        let _ = sender.send(Action::InstanceSelected(index));
    });
    let sender = actions.clone();
    ui.on_manual_url(move || {
        let _ = sender.send(Action::ManualUrl);
    });
    let sender = actions.clone();
    ui.on_retry(move || {
        let _ = sender.send(Action::Retry);
    });
    let sender = actions.clone();
    ui.on_key_pressed(move |key| {
        let _ = sender.send(Action::Key(key.to_string()));
    });
    let sender = actions.clone();
    ui.on_input_cursor_positioned(move |offset| {
        let _ = sender.send(Action::CursorPositioned(offset));
    });
    ui.on_toggle_language(move || {
        let _ = actions.send(Action::ToggleLanguage);
    });
}

pub(crate) fn handle_action(action: Action, state: &mut State) -> Option<Effect> {
    match action {
        Action::WifiSelected(index) => select_wifi(state, index),
        Action::InstanceSelected(index) => select_instance(state, index),
        Action::ManualUrl => {
            state.stage = Stage::Manual;
            state.error.clear();
            state.clear_input();
            None
        }
        Action::Retry => Some(start_discovery(state)),
        Action::Key(key) => {
            if state.key(&key) {
                submit(state)
            } else {
                None
            }
        }
        Action::CursorPositioned(offset) => {
            state.position_cursor(offset);
            None
        }
        Action::ToggleLanguage => {
            state.german = !state.german;
            state.error.clear();
            store::save_language(state.german);
            None
        }
        Action::ScanFinished(result) => {
            state.stage = Stage::Wifi;
            match result {
                Ok(networks) => state.networks = networks,
                Err(error) => {
                    log::warn!(target: "wifi", "Wi-Fi scan failed: {error}");
                    state.networks.clear();
                    state.error = state.localized(
                        "Wi-Fi scan failed. Try again in a moment.",
                        "Die WLAN-Suche ist fehlgeschlagen. Versuche es gleich erneut.",
                    );
                }
            }
            None
        }
        Action::RestoreFinished(result) => match result {
            // Success is intercepted by the screen owner, which starts the Home Assistant session.
            Ok(()) => None,
            Err(_) => {
                state.stage = Stage::Connecting;
                state.error = state.localized(
                    "The saved Wi-Fi network could not be reached. Choose a network.",
                    "Das gespeicherte WLAN ist nicht erreichbar. Wähle ein Netzwerk.",
                );
                Some(Effect::ScanWifi)
            }
        },
        Action::WifiFinished(result) => match result {
            Ok(()) => {
                state.clear_input();
                Some(start_discovery(state))
            }
            Err(error) => {
                log::warn!(target: "wifi", "Wi-Fi connection failed: {error}");
                state.stage = Stage::Password;
                state.error = state.localized(
                    "That password did not connect. Check it and try again.",
                    "Mit diesem Passwort konnte keine Verbindung hergestellt werden.",
                );
                None
            }
        },
        Action::DiscoveryFinished(result) => {
            state.stage = Stage::Discovery;
            match result {
                Ok(instances) if instances.is_empty() => {
                    state.stage = Stage::Manual;
                    state.clear_input();
                    state.error.clear();
                }
                Ok(instances) => state.instances = instances,
                Err(error) => {
                    log::warn!(target: "setup", "Home Assistant discovery failed: {error}");
                    state.stage = Stage::Manual;
                    state.clear_input();
                    state.error = state.localized(
                        "Local discovery is unavailable. Enter the URL manually.",
                        "Die lokale Suche ist nicht verfügbar. Gib die URL manuell ein.",
                    );
                }
            }
            None
        }
        Action::PairingFinished(generation, result) => {
            if generation != state.pairing_generation {
                return None;
            }
            pairing_finished(state, result)
        }
    }
}

fn select_wifi(state: &mut State, index: i32) -> Option<Effect> {
    let Some(index) = usize::try_from(index).ok() else {
        return None;
    };
    let Some(network) = state.networks.get(index).cloned() else {
        return None;
    };
    state.selected_wifi.clone_from(&network.ssid);
    state.error.clear();
    state.clear_input();
    if network.password_required {
        state.stage = Stage::Password;
        None
    } else {
        Some(start_connecting(state, String::new()))
    }
}

fn submit(state: &mut State) -> Option<Effect> {
    match state.stage {
        Stage::Password => {
            let password = state.input.clone();
            Some(start_connecting(state, password))
        }
        Stage::Manual => {
            let endpoint = state.input.clone();
            if validate_endpoint(&endpoint).is_err() {
                state.error = state.localized(
                    "Enter a valid http:// or https:// Home Assistant address.",
                    "Gib eine gültige http://- oder https://-Adresse ein.",
                );
                return None;
            }
            state.selected_instance = "Home Assistant".to_owned();
            state.selected_endpoint = endpoint;
            state.pairing_return = Stage::Manual;
            Some(start_pairing(state))
        }
        _ => None,
    }
}

fn start_connecting(state: &mut State, password: String) -> Effect {
    state.stage = Stage::Connecting;
    state.error.clear();
    Effect::ConnectWifi {
        ssid: state.selected_wifi.clone(),
        password,
    }
}

fn start_discovery(state: &mut State) -> Effect {
    state.stage = Stage::Discovering;
    state.error.clear();
    state.instances.clear();
    state.pairing_code.clear();
    state.pairing_name.clear();
    Effect::Discover
}

fn select_instance(state: &mut State, index: i32) -> Option<Effect> {
    let Some(index) = usize::try_from(index).ok() else {
        return None;
    };
    let Some(instance) = state.instances.get(index).cloned() else {
        return None;
    };
    state.selected_instance.clone_from(&instance.name);
    state.selected_endpoint.clone_from(&instance.endpoint);
    state.pairing_return = Stage::Discovery;
    Some(start_pairing(state))
}

fn start_pairing(state: &mut State) -> Effect {
    state.stage = Stage::Authorizing;
    state.error.clear();
    state.pairing_code.clear();
    state.pairing_name.clear();
    state.asked_grant = false;
    state.pairing_generation = state.pairing_generation.wrapping_add(1);
    Effect::Pair {
        endpoint: state.selected_endpoint.clone(),
        need_grant: false,
        wait_ms: 0,
        generation: state.pairing_generation,
    }
}

/// Whether `generation` is the pairing the screen is showing now.
pub(crate) fn is_current_pairing(state: &State, generation: u32) -> bool {
    generation == state.pairing_generation
        && matches!(state.stage, Stage::Authorizing | Stage::Pairing)
}

fn pairing_finished(
    state: &mut State,
    result: Result<pairing::Round, pairing::Failure>,
) -> Option<Effect> {
    use pairing::{Failure, Round};
    if !matches!(state.stage, Stage::Authorizing | Stage::Pairing) {
        return None;
    }
    let first_contact = state.stage == Stage::Authorizing;
    let endpoint = state.selected_endpoint.clone();
    let generation = state.pairing_generation;
    let again = |need_grant: bool, wait_ms: u32| {
        Some(Effect::Pair {
            endpoint: endpoint.clone(),
            need_grant,
            wait_ms,
            generation,
        })
    };
    match result {
        Ok(Round::Waiting { name, code }) => {
            state.stage = Stage::Pairing;
            state.pairing_name = name;
            state.pairing_code = code;
            state.error.clear();
            state.asked_grant = false;
            again(false, PAIR_EVERY_MS)
        }
        // The screen owner starts the session; nothing is left to do here.
        Ok(Round::Granted(_)) => None,
        Ok(Round::ApprovedWithoutGrant) => {
            if state.asked_grant {
                state.stage = Stage::Pairing;
                state.error = state.localized(
                    "Approved, but Home Assistant sent no access. Remove this tablet in Home Assistant and pair it again.",
                    "Freigegeben, aber Home Assistant hat keinen Zugang gesendet. Entferne dieses Tablet in Home Assistant und koppele es neu.",
                );
                again(true, PAIR_RETRY_MS)
            } else {
                state.asked_grant = true;
                again(true, 0)
            }
        }
        Ok(Round::Conflict) => {
            state.stage = Stage::Pairing;
            state.error = state.localized(
                "Another tablet is registered in Home Assistant under this tablet's ID. Remove it there, then this tablet connects.",
                "In Home Assistant ist unter der ID dieses Tablets ein anderes Tablet eingetragen. Entferne es dort, dann verbindet sich dieses Tablet.",
            );
            again(false, PAIR_RETRY_MS)
        }
        Ok(Round::NotInstalled) => {
            let message = state.localized(
                "Home Assistant answers, but the Njörun integration is not installed there. Add it under Settings, Devices & services.",
                "Home Assistant antwortet, aber die Njörun-Integration ist dort nicht installiert. Füge sie unter Einstellungen, Geräte & Dienste hinzu.",
            );
            fail_pairing(state, first_contact, message, again(false, PAIR_RETRY_MS))
        }
        Ok(Round::Rejected) => {
            let message = state.localized(
                "Home Assistant did not accept the request. Use matching versions of the Njörun integration and of this tablet.",
                "Home Assistant hat die Anfrage nicht angenommen. Nutze zusammenpassende Versionen von Njörun-Integration und Tablet.",
            );
            fail_pairing(state, first_contact, message, again(false, PAIR_RETRY_MS))
        }
        Ok(Round::Busy) => {
            let message = state.localized(
                "Home Assistant is busy with other tablets. This tablet tries again shortly.",
                "Home Assistant ist mit anderen Tablets beschäftigt. Dieses Tablet versucht es gleich erneut.",
            );
            fail_pairing(state, first_contact, message, again(false, PAIR_RETRY_MS))
        }
        Err(Failure::Connection(problem)) => {
            let host = state.host_of_endpoint();
            let message = problem.message(&host, state.german);
            fail_pairing(state, first_contact, message, again(false, PAIR_EVERY_MS))
        }
        Err(Failure::BadAnswer) => {
            let message = state.localized(
                "Home Assistant answered in an unexpected way. Check the address.",
                "Home Assistant hat unerwartet geantwortet. Prüfe die Adresse.",
            );
            fail_pairing(state, first_contact, message, again(false, PAIR_RETRY_MS))
        }
    }
}

/// Before the first answer a problem sends the user back to the address; while the tablet is
/// waiting it only shows the problem and keeps trying.
fn fail_pairing(
    state: &mut State,
    first_contact: bool,
    message: String,
    retry: Option<Effect>,
) -> Option<Effect> {
    state.error = message;
    if first_contact {
        state.stage = state.pairing_return;
        None
    } else {
        retry
    }
}

pub(crate) fn run_effect(effect: Effect) -> Action {
    match effect {
        Effect::ConnectWifi { ssid, password } => {
            let result = board::connect_wifi(&ssid, &password);
            if result.is_ok() {
                store::save_wifi(&ssid, &password);
            }
            Action::WifiFinished(result)
        }
        Effect::ScanWifi => Action::ScanFinished(board::scan_wifi()),
        Effect::Restore { wifi } => Action::RestoreFinished(
            board::connect_wifi(&wifi.ssid, &wifi.password).map_err(|error| {
                log::warn!(target: "wifi", "saved Wi-Fi did not connect: {error}");
                RestoreError::Wifi
            }),
        ),
        Effect::Discover => Action::DiscoveryFinished(board::discover_home_assistant()),
        Effect::Pair {
            endpoint,
            need_grant,
            wait_ms,
            generation,
        } => {
            if wait_ms > 0 {
                std::thread::sleep(Duration::from_millis(u64::from(wait_ms)));
            }
            Action::PairingFinished(generation, pairing::round(&endpoint, need_grant))
        }
    }
}

pub(crate) fn refresh_access_token(session: &HomeAssistantSession) -> Result<String, RestoreError> {
    #[derive(Deserialize)]
    struct Refreshed {
        access_token: String,
    }
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("client_id", &session.client_id)
        .append_pair("refresh_token", &session.refresh_token)
        .finish();
    let response =
        board::post_form(&format!("{}/auth/token", session.url), &body).map_err(|error| {
            let problem = ConnectionProblem::from_code(error);
            log::warn!(target: "setup", "token refresh failed: {error:#x} ({problem:?})");
            if problem == ConnectionProblem::SignInRejected {
                RestoreError::HomeAssistantRejected
            } else {
                RestoreError::HomeAssistantUnreachable(problem)
            }
        })?;
    let bad = RestoreError::HomeAssistantUnreachable(ConnectionProblem::BadResponse);
    let refreshed: Refreshed = serde_json::from_slice(&response).map_err(|_| bad)?;
    if refreshed.access_token.is_empty() || refreshed.access_token.len() > 8192 {
        return Err(bad);
    }
    Ok(refreshed.access_token)
}

pub(crate) fn begin_restore(state: &mut State, wifi: WifiCredentials) -> Effect {
    state.stage = Stage::Connecting;
    state.selected_wifi.clone_from(&wifi.ssid);
    state.error.clear();
    Effect::Restore { wifi }
}

/// Shows the "loading" screen while the Home Assistant session fetches its first snapshot.
pub(crate) fn begin_loading(state: &mut State, url: &str) {
    state.stage = Stage::Loading;
    state.selected_instance = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned();
    state.error.clear();
}

pub(crate) fn loading_host(state: &State) -> String {
    state.selected_instance.clone()
}

/// Shows why Home Assistant cannot be reached on the loading screen; `None` clears it.
pub(crate) fn set_loading_error(state: &mut State, problem: Option<ConnectionProblem>) {
    state.error = problem.map_or_else(String::new, |problem| {
        format!(
            "{} {}",
            problem.message(&state.selected_instance, state.german),
            state.localized("Retrying…", "Neuer Versuch …")
        )
    });
}

/// Shows why the saved setup could not be used, on the Wi-Fi screen after a restart.
pub(crate) fn set_setup_problem(state: &mut State, problem: SetupProblem) {
    state.error = problem.message(state.german).to_owned();
}

/// The stored sign-in was refused: forget it and offer a fresh sign-in.
pub(crate) fn session_rejected(state: &mut State) -> Effect {
    store::clear_home_assistant();
    let effect = start_discovery(state);
    state.error = state.localized(
        "Home Assistant ended this sign-in. Sign in again.",
        "Home Assistant hat diese Anmeldung beendet. Melde dich erneut an.",
    );
    effect
}

pub(crate) fn set_german(state: &mut State, german: bool) {
    state.german = german;
}

pub(crate) fn websocket_url(base_url: &str) -> Result<String, String> {
    let mut websocket = Url::parse(base_url).map_err(|_| "Invalid Home Assistant URL")?;
    let scheme = match websocket.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => return Err("Invalid Home Assistant URL".to_owned()),
    };
    websocket
        .set_scheme(scheme)
        .map_err(|_| "Invalid Home Assistant URL")?;
    websocket.set_path(&format!(
        "{}/api/websocket",
        websocket.path().trim_end_matches('/')
    ));
    websocket.set_query(None);
    Ok(websocket.into())
}

fn validate_endpoint(endpoint: &str) -> Result<Url, String> {
    let url = Url::parse(endpoint).map_err(|_| "Invalid Home Assistant URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Invalid Home Assistant URL".to_owned());
    }
    Ok(url)
}

pub(crate) fn render(ui: &CommissioningWindow, state: &State) -> Result<(), String> {
    ui.set_stage(state.stage.as_str().into());
    ui.set_wifi_networks(ModelRc::new(VecModel::from(
        state
            .networks
            .iter()
            .map(|network| WifiItem {
                name: network.ssid.clone().into(),
                signal: network.signal_bars(),
                secured: network.password_required,
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
    ui.set_input_value(state.input.clone().into());
    ui.set_input_cursor_offset(i32::try_from(state.cursor).unwrap_or(i32::MAX));
    ui.set_input_valid_url(validate_endpoint(&state.input).is_ok());
    ui.set_selected_wifi(state.selected_wifi.clone().into());
    ui.set_selected_instance(state.selected_instance.clone().into());
    ui.set_error_message(state.error.clone().into());
    ui.set_demo_controls(false);
    ui.set_german(state.german);
    ui.set_network_link(
        if matches!(
            state.stage,
            Stage::Discovering
                | Stage::Discovery
                | Stage::Manual
                | Stage::Authorizing
                | Stage::Pairing
                | Stage::Loading
        ) {
            "wifi4"
        } else {
            "wifi"
        }
        .into(),
    );
    ui.set_clock_time("--:--".into());
    ui.set_pairing_name(state.pairing_name.clone().into());
    ui.set_pairing_code(state.pairing_code.clone().into());
    Ok(())
}

pub(crate) fn set_error(state: &mut State, message: String) {
    state.error = message;
}

pub(crate) fn is_german(state: &State) -> bool {
    state.german
}
