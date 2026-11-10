//! Host HA WebSocket and Noise management transport.
//!
//! Credentials are never included in diagnostic errors. The desktop socket adapter
//! is separate from the future ESP-IDF networking backend.

use std::collections::VecDeque;
use std::fmt;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};
use url::Url;

pub mod browser_auth;
pub mod discovery;
pub mod snapshot;
pub mod storage;

/// Maximum HA JSON message admitted by the host adapter. Embedded limits need
/// independent qualification; oversized messages produce an actionable error.
pub const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_DEFERRED_EVENTS: usize = 256;
const MAX_DEFERRED_BYTES: usize = 8 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Intentionally contains no URLs, response payloads or credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientError {
    InvalidEndpoint,
    HttpNotAllowed,
    Connection,
    Authentication,
    Timeout,
    Protocol,
    Rejected,
    Limit,
    Trust,
    Storage,
    StorageCommitUncertain,
    ManagementClosed,
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidEndpoint => "Invalid Home Assistant endpoint",
            Self::HttpNotAllowed => "HTTP requires explicit local-network consent",
            Self::Connection => "Home Assistant connection failed",
            Self::Authentication => "Home Assistant authentication failed; sign in again",
            Self::Timeout => "Home Assistant request timed out",
            Self::Protocol => "Unexpected Home Assistant protocol message",
            Self::Rejected => "Home Assistant rejected the request",
            Self::Limit => "Home Assistant data exceeds the configured host limit",
            Self::Trust => "Tablet trust verification failed; recover or enroll again",
            Self::Storage => "Could not read or persist simulator state",
            Self::StorageCommitUncertain => {
                "Simulator state was replaced but durability is unconfirmed"
            }
            Self::ManagementClosed => "Tablet management session ended",
        })
    }
}
impl std::error::Error for ClientError {}

/// Secret-bearing bootstrap input. Debug is deliberately not implemented.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    pub ha_url: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub client_id: Option<String>,
    pub allow_http: bool,
    pub tablet_id: String,
    pub binding_id: String,
    pub tablet_private_key: String,
    pub ha_public_key: String,
    /// The capabilities this tablet reports in its confirmation (decision D8); absent for a
    /// tablet that leaves them as enrolled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<serde_json::Value>,
}

impl Credentials {
    /// Refresh HA authorization without changing tablet identity or trust.
    ///
    /// # Errors
    /// Requires a refresh token/client ID and rejects redirects or invalid responses.
    pub fn refresh(&mut self) -> Result<(), ClientError> {
        websocket_url(&self.ha_url, self.allow_http)?;
        let refresh_token = self
            .refresh_token
            .as_deref()
            .ok_or(ClientError::Authentication)?;
        let client_id = self
            .client_id
            .as_deref()
            .ok_or(ClientError::Authentication)?;
        let endpoint = format!("{}/auth/token", self.ha_url.trim_end_matches('/'));
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .max_redirects(0)
            .build()
            .into();
        let response: Value = agent
            .post(&endpoint)
            .send_form([
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", client_id),
            ])
            .map_err(|_| ClientError::Authentication)?
            .body_mut()
            .read_json()
            .map_err(|_| ClientError::Authentication)?;
        let token = response["access_token"]
            .as_str()
            .filter(|value| !value.is_empty() && value.len() <= 8192)
            .ok_or(ClientError::Authentication)?;
        token.clone_into(&mut self.access_token);
        Ok(())
    }
}

/// Generated on the simulator; the private key is never sent to HA.
pub struct Identity {
    pub private_key: String,
    pub public_key: String,
}

/// Generate an X25519 identity from the operating system's random source, as pairing does.
///
/// # Errors
/// Returns `Trust` if secure key generation fails.
pub fn generate_identity() -> Result<Identity, ClientError> {
    let mut private = [0_u8; 32];
    getrandom::fill(&mut private).map_err(|_| ClientError::Trust)?;
    let public = njorun_management::public_key(&private).map_err(|_| ClientError::Trust)?;
    Ok(Identity {
        private_key: STANDARD.encode(private),
        public_key: STANDARD.encode(public),
    })
}

/// Validate and derive the API WebSocket URL without accepting embedded credentials.
///
/// # Errors
/// Rejects unsupported schemes, URL credentials, fragments and HTTP without consent.
pub fn websocket_url(endpoint: &str, allow_http: bool) -> Result<Url, ClientError> {
    let mut url = Url::parse(endpoint).map_err(|_| ClientError::InvalidEndpoint)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
        || url.host_str().is_none()
    {
        return Err(ClientError::InvalidEndpoint);
    }
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" if allow_http => "ws",
        "http" => return Err(ClientError::HttpNotAllowed),
        _ => return Err(ClientError::InvalidEndpoint),
    };
    url.set_scheme(scheme)
        .map_err(|()| ClientError::InvalidEndpoint)?;
    let path = format!("{}/api/websocket", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url)
}

// System DNS resolution is synchronous; the connection deadline starts after it.
// TCP attempts share one deadline, and TLS/HTTP handshakes use socket timeouts.
fn connect_tcp(url: &Url) -> Result<TcpStream, ClientError> {
    let addresses = url
        .socket_addrs(|| match url.scheme() {
            "wss" => Some(443),
            "ws" => Some(80),
            _ => None,
        })
        .map_err(|_| ClientError::Connection)?;
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    for address in addresses {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(ClientError::Timeout)?;
        if let Ok(stream) = TcpStream::connect_timeout(&address, remaining) {
            stream
                .set_read_timeout(Some(REQUEST_TIMEOUT))
                .map_err(|_| ClientError::Connection)?;
            stream
                .set_write_timeout(Some(REQUEST_TIMEOUT))
                .map_err(|_| ClientError::Connection)?;
            return Ok(stream);
        }
    }
    Err(ClientError::Connection)
}

/// One authenticated, bounded, ordered HA connection. Use from a worker thread.
pub struct Connection {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    deferred: VecDeque<(Value, usize)>,
    deferred_bytes: usize,
}

impl Connection {
    /// Connect using verified TLS by default and authenticate with HA.
    ///
    /// # Errors
    /// Returns a sanitized endpoint, network, TLS or authentication error.
    pub fn connect(endpoint: &str, token: &str, allow_http: bool) -> Result<Self, ClientError> {
        let url = websocket_url(endpoint, allow_http)?;
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_MESSAGE_BYTES));
        let stream = connect_tcp(&url)?;
        let (mut socket, _) =
            tungstenite::client_tls_with_config(url.as_str(), stream, Some(config), None)
                .map_err(|_| ClientError::Connection)?;
        let stream = match socket.get_mut() {
            MaybeTlsStream::Plain(stream) => stream,
            MaybeTlsStream::Rustls(stream) => &mut stream.sock,
            _ => return Err(ClientError::Connection),
        };
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .map_err(|_| ClientError::Connection)?;
        stream
            .set_write_timeout(Some(REQUEST_TIMEOUT))
            .map_err(|_| ClientError::Connection)?;
        let mut connection = Self {
            socket,
            next_id: 1,
            deferred: VecDeque::new(),
            deferred_bytes: 0,
        };
        let hello = connection.read_until(Instant::now() + REQUEST_TIMEOUT)?;
        if hello["type"] != "auth_required" {
            return Err(ClientError::Protocol);
        }
        connection.send(&json!({"type":"auth", "access_token": token}))?;
        let result = connection.read_until(Instant::now() + REQUEST_TIMEOUT)?;
        if result["type"] != "auth_ok" {
            return Err(ClientError::Authentication);
        }
        Ok(connection)
    }

    fn send(&mut self, value: &Value) -> Result<(), ClientError> {
        let text = serde_json::to_string(value).map_err(|_| ClientError::Protocol)?;
        if text.len() > MAX_MESSAGE_BYTES {
            return Err(ClientError::Limit);
        }
        self.socket
            .send(Message::Text(text.into()))
            .map_err(|_| ClientError::Connection)
    }

    fn read_one(&mut self) -> Result<Option<Value>, ClientError> {
        match self.socket.read() {
            Ok(Message::Text(text)) => {
                if text.len() > MAX_MESSAGE_BYTES {
                    return Err(ClientError::Limit);
                }
                serde_json::from_str(&text)
                    .map(Some)
                    .map_err(|_| ClientError::Protocol)
            }
            Ok(Message::Ping(_) | Message::Pong(_)) => {
                self.socket.flush().map_err(|_| ClientError::Connection)?;
                Ok(None)
            }
            Ok(Message::Close(_)) => Err(ClientError::Connection),
            Ok(_) => Err(ClientError::Protocol),
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(tungstenite::Error::Capacity(_)) => Err(ClientError::Limit),
            Err(_) => Err(ClientError::Connection),
        }
    }

    fn read_until(&mut self, deadline: Instant) -> Result<Value, ClientError> {
        while Instant::now() < deadline {
            if let Some(value) = self.read_one()? {
                return Ok(value);
            }
        }
        Err(ClientError::Timeout)
    }

    /// Execute one HA command while preserving interleaved subscription events.
    ///
    /// # Errors
    /// Rejects malformed requests, errors, timeouts and event-buffer overflow.
    pub fn request(&mut self, mut request: Value) -> Result<(u64, Value), ClientError> {
        let is_ping = request["type"] == "ping";
        let object = request.as_object_mut().ok_or(ClientError::Protocol)?;
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or(ClientError::Limit)?;
        object.insert("id".into(), id.into());
        self.send(&request)?;
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        loop {
            let message = self.read_until(deadline)?;
            if is_ping && message["id"].as_u64() == Some(id) && message["type"] == "pong" {
                return Ok((id, Value::Null));
            }
            if message["id"].as_u64() == Some(id) && message["type"] == "result" {
                if message["success"] != true {
                    return Err(ClientError::Rejected);
                }
                return Ok((id, message["result"].clone()));
            }
            if message["type"] != "event" {
                return Err(ClientError::Protocol);
            }
            let bytes = serde_json::to_vec(&message)
                .map_err(|_| ClientError::Protocol)?
                .len();
            if self.deferred.len() >= MAX_DEFERRED_EVENTS
                || bytes > MAX_DEFERRED_BYTES.saturating_sub(self.deferred_bytes)
            {
                return Err(ClientError::Limit);
            }
            self.deferred_bytes += bytes;
            self.deferred.push_back((message, bytes));
        }
    }

    fn take_deferred(&mut self, subscription: u64) -> VecDeque<Value> {
        let (matching, remaining): (VecDeque<_>, VecDeque<_>) = std::mem::take(&mut self.deferred)
            .into_iter()
            .partition(|(event, _)| event["id"].as_u64() == Some(subscription));
        self.deferred_bytes = remaining.iter().map(|(_, bytes)| bytes).sum();
        self.deferred = remaining;
        matching.into_iter().map(|(event, _)| event).collect()
    }

    /// Poll one event; a short read timeout lets the owner process UI requests.
    ///
    /// # Errors
    /// Returns sanitized transport errors. No requests are queued across reconnect.
    pub fn poll(&mut self) -> Result<Option<Value>, ClientError> {
        if let Some((event, bytes)) = self.deferred.pop_front() {
            self.deferred_bytes -= bytes;
            return Ok(Some(event));
        }
        self.read_one()
    }
}

/// Noise state bound to one HA socket and its management subscription.
pub struct ManagementSession {
    session: njorun_management::Session,
    pub subscription_id: u64,
    /// The live parts (decision D6) that HA offered; empty for an integration without D6.
    pub live: Vec<&'static str>,
    /// Whether HA offered the install protocol (decision D7) on this session.
    pub ota: bool,
}

const fn management_error(error: njorun_management::Error) -> ClientError {
    match error {
        njorun_management::Error::Protocol => ClientError::Protocol,
        njorun_management::Error::Trust => ClientError::Trust,
        njorun_management::Error::Limit => ClientError::Limit,
        njorun_management::Error::Closed => ClientError::ManagementClosed,
    }
}

impl ManagementSession {
    /// Complete device proof and send encrypted initiator confirmation.
    ///
    /// # Errors
    /// Rejects bad pins, contexts, handshake frames or HA session errors.
    pub fn establish(
        connection: &mut Connection,
        credentials: &Credentials,
    ) -> Result<Self, ClientError> {
        let (_, start) = connection.request(njorun_management::session_start_request(
            &credentials.binding_id,
        ))?;
        let private = decode_key(&credentials.tablet_private_key)?;
        let public = decode_key(&credentials.ha_public_key)?;
        let (handshake, request) =
            njorun_management::Handshake::begin(&start, &credentials.binding_id, &private, &public)
                .map_err(management_error)?;
        let (_, response) = connection.request(request)?;
        let (mut session, subscribe) = handshake
            .finish(&response, credentials.capabilities.as_ref())
            .map_err(management_error)?;
        let (subscription_id, result) = connection.request(subscribe)?;
        session.attach(subscription_id);
        let live = njorun_management::live_parts(&result).map_err(management_error)?;
        let ota = njorun_management::ota_offered(&result).map_err(management_error)?;
        Ok(Self {
            session,
            subscription_id,
            live,
            ota,
        })
    }

    /// Whether HA offered a live part (`wake`, `backlight` or `telemetry`) on this session.
    #[must_use]
    pub fn offers(&self, part: &str) -> bool {
        self.live.contains(&part)
    }

    /// Decode an authenticated management event.
    ///
    /// # Errors
    /// Rejects another subscription, malformed JSON, tampering and replay.
    pub fn decode(&mut self, event: &Value) -> Result<Value, ClientError> {
        self.session.decode(event).map_err(management_error)
    }

    /// Report applied settings after the caller has persisted and applied them.
    ///
    /// # Errors
    /// Rejects oversize data or failed session/HA requests.
    pub fn send(
        &mut self,
        connection: &mut Connection,
        payload: &Value,
    ) -> Result<(), ClientError> {
        let request = self
            .session
            .frame_request(payload)
            .map_err(management_error)?;
        connection.request(request)?;
        Ok(())
    }
}

fn decode_key(text: &str) -> Result<[u8; 32], ClientError> {
    njorun_management::decode_key(text).map_err(management_error)
}
