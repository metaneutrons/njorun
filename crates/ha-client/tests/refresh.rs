use njorun_ha_client::{ClientError, Credentials};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

fn credentials(endpoint: String) -> Credentials {
    Credentials {
        ha_url: endpoint,
        access_token: "previous-test-token".into(),
        refresh_token: Some("test-refresh".into()),
        client_id: Some("http://localhost/test-client".into()),
        allow_http: true,
        tablet_id: "tablet".into(),
        binding_id: "binding".into(),
        tablet_private_key: "unchanged-private-test-key".into(),
        ha_public_key: "unchanged-public-test-key".into(),
        capabilities: None,
    }
}

fn refresh_reply(status: &str, response: &Value) -> (Credentials, Result<(), ClientError>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
    let mut credentials = credentials(format!(
        "http://{}",
        listener.local_addr().expect("address")
    ));
    let response = response.to_string();
    let status = status.to_owned();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept");
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        let mut header = Vec::new();
        let mut byte = [0];
        while !header.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).expect("header");
            header.push(byte[0]);
            assert!(header.len() < 8192);
        }
        let header = String::from_utf8(header).expect("header text");
        assert!(header.starts_with("POST /auth/token HTTP/1.1\r\n"));
        let size: usize = header
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().expect("length"))
            })
            .expect("body length");
        assert!(size < 4096);
        let mut body = vec![0; size];
        socket.read_exact(&mut body).expect("form");
        let body = String::from_utf8(body).expect("form text");
        assert!(body.contains("grant_type=refresh_token"));
        assert!(body.contains("refresh_token=test-refresh"));
        write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nLocation: http://127.0.0.1:1/no-follow\r\nConnection: close\r\n\r\n{response}", response.len()).expect("reply");
    });
    let result = credentials.refresh();
    server.join().expect("server");
    (credentials, result)
}

#[test]
fn refresh_replaces_only_the_access_token_after_valid_response() {
    let (credentials, result) =
        refresh_reply("200 OK", &json!({"access_token":"replacement-test-token"}));
    assert_eq!(result, Ok(()));
    assert_eq!(credentials.access_token, "replacement-test-token");
    assert_eq!(credentials.refresh_token.as_deref(), Some("test-refresh"));
    assert_eq!(credentials.tablet_private_key, "unchanged-private-test-key");
    assert_eq!(credentials.ha_public_key, "unchanged-public-test-key");
    assert_eq!(credentials.binding_id, "binding");
}

#[test]
fn refresh_failure_preserves_identity_and_previous_token() {
    for (status, response) in [
        ("401 Unauthorized", json!({"error":"invalid_grant"})),
        ("302 Found", json!({})),
        ("200 OK", json!({"access_token":""})),
        ("200 OK", json!({"access_token":"x".repeat(8193)})),
        ("200 OK", json!({"access_token":42})),
    ] {
        let (credentials, result) = refresh_reply(status, &response);
        assert_eq!(result, Err(ClientError::Authentication));
        assert_eq!(credentials.access_token, "previous-test-token");
        assert_eq!(credentials.binding_id, "binding");
    }
}

#[test]
fn refresh_requires_explicit_http_consent_and_complete_grant() {
    let mut credentials = credentials("http://127.0.0.1:1".into());
    credentials.allow_http = false;
    assert_eq!(credentials.refresh(), Err(ClientError::HttpNotAllowed));
    credentials.allow_http = true;
    credentials.refresh_token = None;
    assert_eq!(credentials.refresh(), Err(ClientError::Authentication));
    credentials.refresh_token = Some("test-refresh".into());
    credentials.client_id = None;
    assert_eq!(credentials.refresh(), Err(ClientError::Authentication));
}
