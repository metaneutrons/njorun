use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::Sender;
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use qrcode::{types::Color, QrCode};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use slint::{ModelRc, SharedString, VecModel};
use url::Url;

use crate::board;
use crate::generated::{CommissioningWindow, InstanceItem, RoomItem, WifiItem};

const MAX_CALLBACK_REQUEST_BYTES: usize = 8192;
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Wifi,
    Password,
    Discovery,
    Manual,
    Browser,
}

impl Stage {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Wifi => "wifi",
            Self::Password => "password",
            Self::Discovery => "discovery",
            Self::Manual => "manual",
            Self::Browser => "browser",
        }
    }
}

pub(crate) enum Action {
    WifiSelected(i32),
    InstanceSelected(i32),
    ManualUrl,
    Retry,
    Key(String),
    ToggleLanguage,
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
}

pub(crate) struct Authorization {
    listener: TcpListener,
    base_url: String,
    client_id: String,
    state: String,
    verifier: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
}

#[derive(Deserialize)]
struct AreaResult {
    id: u64,
    #[serde(rename = "type")]
    message_type: String,
    success: bool,
    result: Vec<Area>,
}

#[derive(Deserialize)]
struct Area {
    area_id: String,
    name: String,
}

impl State {
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
        }
    }

    fn localized(&self, english: &str, german: &str) -> String {
        if self.german { german } else { english }.to_owned()
    }

    fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
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
    ui.on_toggle_language(move || {
        let _ = actions.send(Action::ToggleLanguage);
    });
}

pub(crate) fn handle_action(
    action: Action,
    state: &mut State,
    authorization: &mut Option<Authorization>,
) {
    match action {
        Action::WifiSelected(index) => select_wifi(state, index),
        Action::InstanceSelected(index) => select_instance(state, authorization, index),
        Action::ManualUrl => {
            state.stage = Stage::Manual;
            state.error.clear();
            state.clear_input();
            *authorization = None;
        }
        Action::Retry => discover(state),
        Action::Key(key) => {
            if state.key(&key) {
                submit(state, authorization);
            }
        }
        Action::ToggleLanguage => {
            state.german = !state.german;
            state.error.clear();
        }
    }
}

fn select_wifi(state: &mut State, index: i32) {
    let Some(index) = usize::try_from(index).ok() else {
        return;
    };
    let Some(network) = state.networks.get(index).cloned() else {
        return;
    };
    state.selected_wifi.clone_from(&network.ssid);
    state.error.clear();
    state.clear_input();
    if network.password_required {
        state.stage = Stage::Password;
    } else {
        connect_and_discover(state, "");
    }
}

fn submit(state: &mut State, authorization: &mut Option<Authorization>) {
    match state.stage {
        Stage::Password => {
            let password = state.input.clone();
            connect_and_discover(state, &password);
            if state.stage == Stage::Discovery {
                state.clear_input();
            }
        }
        Stage::Manual => {
            let endpoint = state.input.clone();
            if validate_endpoint(&endpoint).is_err() {
                state.error = state.localized(
                    "Enter a valid http:// or https:// Home Assistant address.",
                    "Gib eine gültige http://- oder https://-Adresse ein.",
                );
                return;
            }
            state.selected_instance = "Home Assistant".to_owned();
            state.selected_endpoint = endpoint;
            begin_authorization(state, authorization);
        }
        _ => {}
    }
}

fn connect_and_discover(state: &mut State, password: &str) {
    state.error.clear();
    match board::connect_wifi(&state.selected_wifi, password) {
        Ok(()) => discover(state),
        Err(_) => {
            state.stage = Stage::Password;
            state.error = state.localized(
                "That password did not connect. Check it and try again.",
                "Mit diesem Passwort konnte keine Verbindung hergestellt werden.",
            );
        }
    }
}

fn discover(state: &mut State) {
    state.stage = Stage::Discovery;
    state.error.clear();
    state.instances.clear();
    state.authorization_url.clear();
    state.login_code.clear();
    match board::discover_home_assistant() {
        Ok(instances) if instances.is_empty() => {
            state.error = state.localized(
                "No installation found. Rescan or enter its URL.",
                "Keine Installation gefunden. Suche erneut oder gib ihre URL ein.",
            );
        }
        Ok(instances) => state.instances = instances,
        Err(_) => {
            state.error = state.localized(
                "Local discovery is unavailable. Enter the URL manually.",
                "Die lokale Suche ist nicht verfügbar. Gib die URL manuell ein.",
            );
        }
    }
}

fn select_instance(state: &mut State, authorization: &mut Option<Authorization>, index: i32) {
    let Some(index) = usize::try_from(index).ok() else {
        return;
    };
    let Some(instance) = state.instances.get(index).cloned() else {
        return;
    };
    state.selected_instance.clone_from(&instance.name);
    state.selected_endpoint.clone_from(&instance.endpoint);
    begin_authorization(state, authorization);
}

fn begin_authorization(state: &mut State, authorization: &mut Option<Authorization>) {
    state.error.clear();
    state.authorization_url.clear();
    state.login_code.clear();
    match Authorization::start(&state.selected_endpoint) {
        Ok((session, url, login_code)) => {
            state.stage = Stage::Browser;
            state.authorization_url = url;
            state.login_code = login_code;
            *authorization = Some(session);
        }
        Err(_) => {
            state.error = state.localized(
                "Could not start browser sign-in. Check the address and network.",
                "Die Browser-Anmeldung konnte nicht gestartet werden. Prüfe Adresse und Netzwerk.",
            );
        }
    }
}

impl Authorization {
    fn start(endpoint: &str) -> Result<(Self, String, String), String> {
        let base = validate_endpoint(endpoint)?;
        let local_ip = callback_ipv4(&base)?;
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
                state,
                verifier,
            },
            authorization_url.into(),
            login_code,
        ))
    }
}

pub(crate) fn poll_authorization(
    authorization: &mut Option<Authorization>,
    german: bool,
) -> Result<Option<Vec<RoomItem>>, String> {
    let Some(session) = authorization.as_ref() else {
        return Ok(None);
    };
    let (mut stream, _) = match session.listener.accept() {
        Ok(accepted) => accepted,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
        Err(_) => {
            return Err(localized(
                german,
                "Browser callback failed.",
                "Der Browser-Rückruf ist fehlgeschlagen.",
            ))
        }
    };
    let code = match read_callback(session, &mut stream)? {
        Some(code) => code,
        None => return Ok(None),
    };
    let session = authorization
        .take()
        .ok_or_else(|| "Authorization state disappeared".to_owned())?;
    let token = exchange_code(&session, &code).map_err(|_| {
        localized(
            german,
            "Home Assistant rejected the browser sign-in.",
            "Home Assistant hat die Browser-Anmeldung abgelehnt.",
        )
    })?;
    let rooms = fetch_rooms(&session.base_url, &token.access_token, german).map_err(|_| {
        localized(
            german,
            "Signed in, but the room list could not be loaded.",
            "Angemeldet, aber die Raumliste konnte nicht geladen werden.",
        )
    })?;
    Ok(Some(rooms))
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
        let read = stream
            .read(&mut request[length..])
            .map_err(|_| "Could not read browser callback")?;
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
    let response = board::post_form(&endpoint, &body).map_err(|_| "Token exchange failed")?;
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

fn fetch_rooms(base_url: &str, access_token: &str, german: bool) -> Result<Vec<RoomItem>, String> {
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
    let auth = serde_json::to_string(&serde_json::json!({
        "type": "auth",
        "access_token": access_token,
    }))
    .map_err(|_| "Could not build Home Assistant authentication")?;
    let response = board::fetch_areas(websocket.as_str(), &auth)
        .map_err(|_| "Home Assistant WebSocket failed")?;
    let result: AreaResult =
        serde_json::from_slice(&response).map_err(|_| "Invalid Home Assistant area response")?;
    if result.id != 1 || result.message_type != "result" || !result.success {
        return Err("Home Assistant rejected the area request".to_owned());
    }
    Ok(result
        .result
        .into_iter()
        .filter(|area| !area.area_id.is_empty() && !area.name.is_empty())
        .map(|area| RoomItem {
            id: area.area_id.into(),
            name: area.name.into(),
            summary: if german {
                "Home-Assistant-Bereich".into()
            } else {
                "Home Assistant area".into()
            },
        })
        .collect())
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

fn callback_ipv4(endpoint: &Url) -> Result<Ipv4Addr, String> {
    let host = endpoint
        .host_str()
        .ok_or_else(|| "Missing Home Assistant host".to_owned())?;
    let port = endpoint
        .port_or_known_default()
        .ok_or_else(|| "Missing Home Assistant port".to_owned())?;
    let remote = (host, port)
        .to_socket_addrs()
        .map_err(|_| "Could not resolve Home Assistant")?
        .find(std::net::SocketAddr::is_ipv4)
        .ok_or_else(|| "No IPv4 route to Home Assistant".to_owned())?;
    let socket =
        UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|_| "Could not create route probe")?;
    socket
        .connect(remote)
        .map_err(|_| "Could not route to Home Assistant")?;
    match socket
        .local_addr()
        .map_err(|_| "Could not read local route")?
        .ip()
    {
        IpAddr::V4(address) if !address.is_unspecified() => Ok(address),
        _ => Err("No IPv4 route to Home Assistant".to_owned()),
    }
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
    ui.set_selected_wifi(state.selected_wifi.clone().into());
    ui.set_selected_instance(state.selected_instance.clone().into());
    ui.set_error_message(state.error.clone().into());
    ui.set_demo_controls(false);
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

pub(crate) fn set_error(state: &mut State, message: String) {
    state.error = message;
}

pub(crate) fn is_german(state: &State) -> bool {
    state.german
}
