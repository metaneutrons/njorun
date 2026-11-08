use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use qrcode::{types::Color, QrCode};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use slint::{Image, ModelRc, Rgb8Pixel, SharedPixelBuffer, SharedString, VecModel};
use url::Url;

use njorun_app_core::problems::{ConnectionProblem, SetupProblem};

use crate::board;
use crate::generated::{CommissioningWindow, InstanceItem, WifiItem};
use crate::store::{self, HomeAssistantSession, WifiCredentials};

const MAX_CALLBACK_REQUEST_BYTES: usize = 8192;
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(2);
const QR_MODULE_PIXELS: usize = 8;
const QR_QUIET_MODULES: usize = 2;

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
    Browser,
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
            Self::Browser => "browser",
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
    AuthorizationFinished(Result<(Authorization, String, String), String>),
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
    ConnectWifi { ssid: String, password: String },
    Discover,
    StartAuthorization { endpoint: String },
    ScanWifi,
    Restore { wifi: WifiCredentials },
}

pub(crate) fn failed_action(effect: &Effect) -> Action {
    match effect {
        Effect::ConnectWifi { .. } => Action::WifiFinished(Err(-1)),
        Effect::Discover => Action::DiscoveryFinished(Err(-1)),
        Effect::StartAuthorization { .. } => {
            Action::AuthorizationFinished(Err("Not enough memory to start sign-in".to_owned()))
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
    authorization_url: String,
    login_code: String,
    authorization_return: Stage,
}

pub(crate) struct SignedIn {
    pub session: HomeAssistantSession,
    pub access_token: String,
}

pub(crate) struct Authorization {
    listener: TcpListener,
    base_url: String,
    client_id: String,
    authorization_url: String,
    state: String,
    verifier: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
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
            authorization_url: String::new(),
            login_code: String::new(),
            authorization_return: Stage::Discovery,
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

pub(crate) fn handle_action(
    action: Action,
    state: &mut State,
    authorization: &mut Option<Authorization>,
) -> Option<Effect> {
    match action {
        Action::WifiSelected(index) => select_wifi(state, index),
        Action::InstanceSelected(index) => select_instance(state, index),
        Action::ManualUrl => {
            state.stage = Stage::Manual;
            state.error.clear();
            state.clear_input();
            *authorization = None;
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
        Action::AuthorizationFinished(result) => {
            match result {
                Ok((session, url, login_code)) => {
                    state.stage = Stage::Browser;
                    state.authorization_url = url;
                    state.login_code = login_code;
                    *authorization = Some(session);
                }
                Err(error) => {
                    log::warn!(target: "setup", "browser authorization failed: {error}");
                    state.stage = state.authorization_return;
                    state.error = state.localized(
                        "Could not start browser sign-in. Check the address and network.",
                        "Die Browser-Anmeldung konnte nicht gestartet werden. Prüfe Adresse und Netzwerk.",
                    );
                }
            }
            None
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
            state.authorization_return = Stage::Manual;
            Some(start_authorization(state))
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
    state.authorization_url.clear();
    state.login_code.clear();
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
    state.authorization_return = Stage::Discovery;
    Some(start_authorization(state))
}

fn start_authorization(state: &mut State) -> Effect {
    state.stage = Stage::Authorizing;
    state.error.clear();
    state.authorization_url.clear();
    state.login_code.clear();
    Effect::StartAuthorization {
        endpoint: state.selected_endpoint.clone(),
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
        Effect::StartAuthorization { endpoint } => {
            Action::AuthorizationFinished(Authorization::start(&endpoint))
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

impl Authorization {
    fn start(endpoint: &str) -> Result<(Self, String, String), String> {
        let base = validate_endpoint(endpoint)?;
        let local_ip = callback_ipv4()?;
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))
            .map_err(|_| "Could not bind callback listener")?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "Could not configure callback listener")?;
        let port = listener
            .local_addr()
            .map_err(|_| "Could not read callback listener")?
            .port();
        let callback_origin = format!("http://{local_ip}:{port}");
        let client_id = format!("{callback_origin}/client");
        let redirect_uri = format!("{callback_origin}/callback");
        let state = random_token()?;
        let verifier = random_token()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut authorization_url = base.clone();
        authorization_url.set_path(&format!(
            "{}/auth/authorize",
            base.path().trim_end_matches('/')
        ));
        authorization_url.set_query(None);
        authorization_url
            .query_pairs_mut()
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        let login_code = format!(
            "{}-{}",
            state.chars().take(4).collect::<String>().to_uppercase(),
            state
                .chars()
                .skip(4)
                .take(4)
                .collect::<String>()
                .to_uppercase()
        );
        Ok((
            Self {
                listener,
                base_url: endpoint.trim_end_matches('/').to_owned(),
                client_id,
                authorization_url: authorization_url.into(),
                state,
                verifier,
            },
            format!("{callback_origin}/"),
            login_code,
        ))
    }
}

pub(crate) fn poll_authorization(
    authorization: &mut Option<Authorization>,
    german: bool,
) -> Result<Option<SignedIn>, String> {
    let Some(session) = authorization.as_ref() else {
        return Ok(None);
    };
    let (mut stream, _) = match session.listener.accept() {
        Ok(accepted) => accepted,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
        Err(error) => {
            log::warn!(target: "setup", "callback accept failed: {error}");
            return Err(localized(
                german,
                "Browser callback failed.",
                "Der Browser-Rückruf ist fehlgeschlagen.",
            ));
        }
    };
    let code = match read_callback(session, &mut stream).inspect_err(|error| {
        log::warn!(target: "setup", "browser callback failed: {error}");
    })? {
        Some(code) => code,
        None => return Ok(None),
    };
    let session = authorization
        .take()
        .ok_or_else(|| "Authorization state disappeared".to_owned())?;
    let token = exchange_code(&session, &code).map_err(|error| {
        log::warn!(target: "setup", "token exchange failed: {error}");
        localized(
            german,
            "Home Assistant rejected the browser sign-in.",
            "Home Assistant hat die Browser-Anmeldung abgelehnt.",
        )
    })?;
    let signed_in = HomeAssistantSession {
        url: session.base_url.clone(),
        client_id: session.client_id.clone(),
        refresh_token: token.refresh_token,
    };
    store::save_home_assistant(&signed_in);
    Ok(Some(SignedIn {
        session: signed_in,
        access_token: token.access_token,
    }))
}

fn read_callback(
    session: &Authorization,
    stream: &mut TcpStream,
) -> Result<Option<String>, String> {
    stream
        .set_read_timeout(Some(CALLBACK_TIMEOUT))
        .map_err(|_| "Could not configure callback socket")?;
    stream
        .set_write_timeout(Some(CALLBACK_TIMEOUT))
        .map_err(|_| "Could not configure callback socket")?;
    let mut request = [0_u8; MAX_CALLBACK_REQUEST_BYTES];
    let mut length = 0;
    while length < request.len() {
        let read = match stream.read(&mut request[length..]) {
            Ok(read) => read,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(error) => {
                log::warn!(target: "setup", "callback read failed: {error}");
                return Err("Could not read browser callback".to_owned());
            }
        };
        if read == 0 {
            break;
        }
        length += read;
        if request[..length]
            .windows(4)
            .any(|window| window == b"\r\n\r\n")
        {
            break;
        }
    }
    if length == 0 {
        return Ok(None);
    }
    let target = std::str::from_utf8(&request[..length])
        .ok()
        .and_then(|text| text.lines().next())
        .and_then(|line| {
            let mut parts = line.split_whitespace();
            match (parts.next(), parts.next(), parts.next(), parts.next()) {
                (Some("GET"), Some(target), Some(_), None) => Some(target),
                _ => None,
            }
        });
    let callback =
        target.and_then(|target| Url::parse(&format!("http://callback.invalid{target}")).ok());
    if callback.as_ref().is_some_and(|url| url.path() == "/") {
        write_redirect_response(stream, &session.authorization_url)?;
        return Ok(None);
    }
    let mut code = None;
    let mut state = None;
    let mut duplicate = false;
    if let Some(callback) = callback.as_ref().filter(|url| url.path() == "/callback") {
        for (name, value) in callback.query_pairs() {
            match name.as_ref() {
                "code" => duplicate |= code.replace(value.into_owned()).is_some(),
                "state" => duplicate |= state.replace(value.into_owned()).is_some(),
                _ => {}
            }
        }
    }
    let accepted = !duplicate
        && state.as_deref() == Some(session.state.as_str())
        && code
            .as_ref()
            .is_some_and(|value| !value.is_empty() && value.len() <= 8192);
    write_callback_response(stream, accepted)?;
    Ok(if accepted { code } else { None })
}

fn write_redirect_response(stream: &mut TcpStream, location: &str) -> Result<(), String> {
    write!(
        stream,
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n"
    )
    .map_err(|_| "Could not write browser redirect")?;
    stream
        .flush()
        .map_err(|_| "Could not flush browser redirect")?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|_| "Could not close browser redirect".to_owned())
}

fn write_callback_response(stream: &mut TcpStream, accepted: bool) -> Result<(), String> {
    let (status, body) = if accepted {
        (
            "200 OK",
            "<!doctype html><meta name=viewport content=\"width=device-width\"><title>Njörun</title><p>Sign-in complete. You can return to Njörun.</p>",
        )
    } else {
        (
            "400 Bad Request",
            "<!doctype html><meta name=viewport content=\"width=device-width\"><title>Njörun</title><p>This sign-in link is invalid or expired.</p>",
        )
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    )
    .map_err(|_| "Could not write browser callback")?;
    stream
        .flush()
        .map_err(|_| "Could not flush browser callback")?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|_| "Could not close browser callback".to_owned())
}

fn exchange_code(session: &Authorization, code: &str) -> Result<TokenResponse, String> {
    let endpoint = format!("{}/auth/token", session.base_url);
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("client_id", &session.client_id)
        .append_pair("code", code)
        .append_pair("code_verifier", &session.verifier)
        .finish();
    let response = board::post_form(&endpoint, &body)
        .map_err(|code| format!("Token exchange failed: {code}"))?;
    let token: TokenResponse =
        serde_json::from_slice(&response).map_err(|_| "Invalid token response")?;
    if token.access_token.is_empty()
        || token.access_token.len() > 8192
        || token.refresh_token.is_empty()
        || token.refresh_token.len() > 8192
    {
        return Err("Invalid token response".to_owned());
    }
    Ok(token)
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

fn callback_ipv4() -> Result<Ipv4Addr, String> {
    board::local_ipv4().map_err(|error| format!("Could not read tablet IPv4 address: {error}"))
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    board::fill_random(&mut bytes).map_err(|_| "Secure random generation failed")?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn localized(german: bool, english: &str, german_text: &str) -> String {
    if german { german_text } else { english }.to_owned()
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
                | Stage::Browser
                | Stage::Loading
        ) {
            "wifi4"
        } else {
            "wifi"
        }
        .into(),
    );
    ui.set_clock_time("--:--".into());
    ui.set_login_code(SharedString::from(state.login_code.clone()));
    if state.stage == Stage::Browser && !state.authorization_url.is_empty() {
        let code = QrCode::new(state.authorization_url.as_bytes())
            .map_err(|_| "Could not build setup QR code")?;
        ui.set_qr_size(i32::try_from(code.width()).map_err(|_| "QR code is too large")?);
        ui.set_qr_modules(ModelRc::default());
        ui.set_qr_image(qr_image(&code)?);
    } else {
        ui.set_qr_size(0);
        ui.set_qr_modules(ModelRc::default());
        ui.set_qr_image(Image::default());
    }
    Ok(())
}

fn qr_image(code: &QrCode) -> Result<Image, String> {
    let modules = code.width();
    let side = (modules + 2 * QR_QUIET_MODULES) * QR_MODULE_PIXELS;
    let side_pixels = u32::try_from(side).map_err(|_| "QR code is too large")?;
    let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(side_pixels, side_pixels);
    let pixels = buffer.make_mut_slice();
    pixels.fill(Rgb8Pixel::new(255, 255, 255));
    for (index, color) in code.to_colors().into_iter().enumerate() {
        if color != Color::Dark {
            continue;
        }
        let left = (index % modules + QR_QUIET_MODULES) * QR_MODULE_PIXELS;
        let top = (index / modules + QR_QUIET_MODULES) * QR_MODULE_PIXELS;
        for row in top..top + QR_MODULE_PIXELS {
            let start = row * side + left;
            if let Some(span) = pixels.get_mut(start..start + QR_MODULE_PIXELS) {
                span.fill(Rgb8Pixel::new(0, 0, 0));
            }
        }
    }
    Ok(Image::from_rgb8(buffer))
}

pub(crate) fn set_error(state: &mut State, message: String) {
    state.error = message;
}

pub(crate) fn is_german(state: &State) -> bool {
    state.german
}
