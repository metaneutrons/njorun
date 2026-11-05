//! Standard Home Assistant authorization-code login with S256 PKCE.
//!
//! The callback listener is intended for a phone browser on the same trusted
//! local network. The authorization code is protected by PKCE and accepted
//! only with the unpredictable state generated for this session.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::{ClientError, websocket_url};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CALLBACK_REQUEST_BYTES: usize = 8192;
const MAX_TOKEN_BYTES: usize = 8192;

/// Public values required to render the browser-login screen.
pub struct BrowserAuthorization {
    authorization_url: String,
    login_code: String,
    base_url: String,
    client_id: String,
    state: String,
    verifier: String,
    listener: TcpListener,
}

/// Tokens returned by Home Assistant. Debug is deliberately not implemented.
pub struct AuthorizationTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub client_id: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
}

impl BrowserAuthorization {
    /// Create a bounded callback listener and the standard HA authorization URL.
    ///
    /// # Errors
    /// Rejects invalid endpoints, unavailable local routes and failed secure
    /// random generation.
    pub fn start(endpoint: &str, allow_http: bool) -> Result<Self, ClientError> {
        websocket_url(endpoint, allow_http)?;
        let base = Url::parse(endpoint).map_err(|_| ClientError::InvalidEndpoint)?;
        let local_ip = callback_ipv4(&base)?;
        let listener =
            TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|_| ClientError::Connection)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| ClientError::Connection)?;
        let port = listener
            .local_addr()
            .map_err(|_| ClientError::Connection)?
            .port();
        let callback_origin = format!("http://{local_ip}:{port}");
        let client_id = format!("{callback_origin}/client");
        let redirect_uri = format!("{callback_origin}/callback");
        let state = random_url_token()?;
        let verifier = random_url_token()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut authorization = Url::parse(&format!(
            "{}/auth/authorize",
            endpoint.trim_end_matches('/')
        ))
        .map_err(|_| ClientError::InvalidEndpoint)?;
        authorization
            .query_pairs_mut()
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        let login_code = state.chars().take(4).collect::<String>().to_uppercase()
            + "-"
            + &state
                .chars()
                .skip(4)
                .take(4)
                .collect::<String>()
                .to_uppercase();
        Ok(Self {
            authorization_url: authorization.into(),
            login_code,
            base_url: endpoint.trim_end_matches('/').to_owned(),
            client_id,
            state,
            verifier,
            listener,
        })
    }

    /// URL encoded into the on-screen QR code.
    #[must_use]
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }

    /// Short, non-secret session identifier shown on the panel.
    #[must_use]
    pub fn login_code(&self) -> &str {
        &self.login_code
    }

    /// Wait for a matching callback and exchange its one-time code for tokens.
    ///
    /// # Errors
    /// Returns a sanitized timeout, callback or token-exchange error. Callback
    /// query strings and credentials are never included in errors.
    pub fn wait_and_exchange(self, timeout: Duration) -> Result<AuthorizationTokens, ClientError> {
        let code = self.wait_for_code(timeout)?;
        let endpoint = format!("{}/auth/token", self.base_url);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .max_redirects(0)
            .build()
            .into();
        let response: TokenResponse = agent
            .post(&endpoint)
            .send_form([
                ("grant_type", "authorization_code"),
                ("client_id", self.client_id.as_str()),
                ("code", code.as_str()),
                ("code_verifier", self.verifier.as_str()),
            ])
            .map_err(|_| ClientError::Authentication)?
            .body_mut()
            .read_json()
            .map_err(|_| ClientError::Authentication)?;
        if response.access_token.is_empty()
            || response.access_token.len() > MAX_TOKEN_BYTES
            || response.refresh_token.is_empty()
            || response.refresh_token.len() > MAX_TOKEN_BYTES
        {
            return Err(ClientError::Authentication);
        }
        Ok(AuthorizationTokens {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            client_id: self.client_id,
        })
    }

    fn wait_for_code(&self, timeout: Duration) -> Result<String, ClientError> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    if let Some(code) = self.handle_callback(&mut stream)? {
                        return Ok(code);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(ClientError::Timeout);
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => return Err(ClientError::Connection),
            }
        }
    }

    fn handle_callback(&self, stream: &mut TcpStream) -> Result<Option<String>, ClientError> {
        stream
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .map_err(|_| ClientError::Connection)?;
        stream
            .set_write_timeout(Some(REQUEST_TIMEOUT))
            .map_err(|_| ClientError::Connection)?;
        let mut request = [0_u8; MAX_CALLBACK_REQUEST_BYTES];
        let read = stream
            .read(&mut request)
            .map_err(|_| ClientError::Connection)?;
        let line = std::str::from_utf8(&request[..read])
            .ok()
            .and_then(|text| text.lines().next());
        let target = line.and_then(|line| {
            let mut parts = line.split_whitespace();
            match (parts.next(), parts.next(), parts.next(), parts.next()) {
                (Some("GET"), Some(target), Some(_), None) => Some(target),
                _ => None,
            }
        });
        let callback =
            target.and_then(|target| Url::parse(&format!("http://callback.invalid{target}")).ok());
        let valid_path = callback
            .as_ref()
            .is_some_and(|url| url.path() == "/callback");
        let mut code = None;
        let mut state = None;
        let mut duplicates = false;
        if let Some(callback) = callback {
            for (name, value) in callback.query_pairs() {
                match name.as_ref() {
                    "code" => duplicates |= code.replace(value.into_owned()).is_some(),
                    "state" => duplicates |= state.replace(value.into_owned()).is_some(),
                    _ => {}
                }
            }
        }
        let accepted = valid_path
            && !duplicates
            && state.as_deref() == Some(self.state.as_str())
            && code
                .as_ref()
                .is_some_and(|value| !value.is_empty() && value.len() <= 8192);
        write_callback_response(stream, accepted)?;
        Ok(if accepted { code } else { None })
    }
}

fn random_url_token() -> Result<String, ClientError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| ClientError::Authentication)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn callback_ipv4(endpoint: &Url) -> Result<Ipv4Addr, ClientError> {
    let host = endpoint.host_str().ok_or(ClientError::InvalidEndpoint)?;
    let port = endpoint
        .port_or_known_default()
        .ok_or(ClientError::InvalidEndpoint)?;
    let remote = (host, port)
        .to_socket_addrs()
        .map_err(|_| ClientError::Connection)?
        .find(std::net::SocketAddr::is_ipv4)
        .ok_or(ClientError::Connection)?;
    if remote.ip().is_loopback() {
        return Ok(Ipv4Addr::LOCALHOST);
    }
    let socket =
        UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|_| ClientError::Connection)?;
    socket
        .connect(remote)
        .map_err(|_| ClientError::Connection)?;
    match socket
        .local_addr()
        .map_err(|_| ClientError::Connection)?
        .ip()
    {
        IpAddr::V4(address) if !address.is_unspecified() => Ok(address),
        _ => Err(ClientError::Connection),
    }
}

fn write_callback_response(stream: &mut TcpStream, accepted: bool) -> Result<(), ClientError> {
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
    .map_err(|_| ClientError::Connection)
}

#[cfg(test)]
mod tests {
    use super::BrowserAuthorization;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;
    use url::Url;

    fn request(url: &str) -> String {
        let parsed = Url::parse(url).expect("callback URL");
        let address = format!(
            "{}:{}",
            parsed.host_str().expect("callback host"),
            parsed.port().expect("callback port")
        );
        let mut stream = TcpStream::connect(address).expect("connect callback");
        write!(
            stream,
            "GET {}?{} HTTP/1.1\r\nHost: callback\r\nConnection: close\r\n\r\n",
            parsed.path(),
            parsed.query().unwrap_or_default()
        )
        .expect("write callback");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read callback");
        response
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2048];
        loop {
            let bytes = stream.read(&mut buffer).expect("read token request");
            assert_ne!(bytes, 0, "token request ended before its body");
            request.extend_from_slice(&buffer[..bytes]);
            let Some(header_end) = request.windows(4).position(|value| value == b"\r\n\r\n") else {
                continue;
            };
            let body_start = header_end + 4;
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .expect("content length");
            if request.len() >= body_start + length {
                return String::from_utf8(request).expect("UTF-8 token request");
            }
        }
    }

    #[test]
    fn matching_state_and_pkce_exchange_are_required() {
        let token_server = TcpListener::bind("127.0.0.1:0").expect("token listener");
        let endpoint = format!(
            "http://{}",
            token_server.local_addr().expect("token address")
        );
        let session = BrowserAuthorization::start(&endpoint, true).expect("start authorization");
        let authorization = Url::parse(session.authorization_url()).expect("authorization URL");
        let redirect = authorization
            .query_pairs()
            .find_map(|(name, value)| (name == "redirect_uri").then(|| value.into_owned()))
            .expect("redirect URI");
        let state = session.state.clone();
        let client_id = session.client_id.clone();
        let verifier = session.verifier.clone();
        let worker = thread::spawn(move || session.wait_and_exchange(Duration::from_secs(3)));

        let wrong = format!("{redirect}?code=one-time-code&state=wrong");
        assert!(request(&wrong).starts_with("HTTP/1.1 400"));
        let correct = format!("{redirect}?code=one-time-code&state={state}");
        assert!(request(&correct).starts_with("HTTP/1.1 200"));

        let (mut token_stream, _) = token_server.accept().expect("accept token exchange");
        token_stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("token read timeout");
        let request = read_http_request(&mut token_stream);
        let form = request
            .split_once("\r\n\r\n")
            .map(|(_, body)| {
                url::form_urlencoded::parse(body.as_bytes())
                    .into_owned()
                    .collect::<std::collections::BTreeMap<_, _>>()
            })
            .expect("token form");
        assert_eq!(
            form.get("grant_type").map(String::as_str),
            Some("authorization_code")
        );
        assert_eq!(form.get("code").map(String::as_str), Some("one-time-code"));
        assert_eq!(form.get("client_id"), Some(&client_id));
        assert_eq!(form.get("code_verifier"), Some(&verifier));
        let response = r#"{"access_token":"access","refresh_token":"refresh"}"#;
        write!(
            token_stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
            response.len()
        )
        .expect("write token response");
        let tokens = worker
            .join()
            .expect("authorization worker")
            .expect("tokens");
        assert_eq!(tokens.access_token, "access");
        assert_eq!(tokens.refresh_token, "refresh");
        assert_eq!(tokens.client_id, client_id);
    }
}
