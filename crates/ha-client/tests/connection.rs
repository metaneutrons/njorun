use base64::{Engine, engine::general_purpose::STANDARD};
use njorun_ha_client::{
    ClientError, Connection, Credentials, ManagementSession, generate_identity, websocket_url,
};
use njorun_pairing::Responder;
use serde_json::{Value, json};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;
use tungstenite::{Message, WebSocket};

fn receive(socket: &mut WebSocket<TcpStream>) -> Value {
    let message = socket.read().expect("request");
    serde_json::from_str(message.to_text().expect("text")).expect("json")
}
fn send(socket: &mut WebSocket<TcpStream>, value: &Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .expect("send");
}
fn reply(socket: &mut WebSocket<TcpStream>, request: &Value, result: &Value) {
    send(
        socket,
        &json!({"id":request["id"],"type":"result","success":true,"result":result}),
    );
}
fn accept(listener: &TcpListener) -> WebSocket<TcpStream> {
    let (stream, _) = listener.accept().expect("accept");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("write timeout");
    tungstenite::accept(stream).expect("websocket")
}

#[test]
fn asynchronous_service_results_survive_an_interleaved_heartbeat() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener);
        send(&mut socket, &json!({"type":"auth_required"}));
        receive(&mut socket);
        send(&mut socket, &json!({"type":"auth_ok"}));
        let query = receive(&mut socket);
        assert_eq!(query["type"], "media_player/browse_media");
        let ping = receive(&mut socket);
        assert_eq!(ping["type"], "ping");
        send(
            &mut socket,
            &json!({"type":"event","id":77,"event":{"changed":true}}),
        );
        reply(
            &mut socket,
            &query,
            &json!({"children":[{"title":"Album"}]}),
        );
        send(&mut socket, &json!({"id":ping["id"],"type":"pong"}));
    });
    let mut connection =
        Connection::connect(&format!("http://{address}"), "test-token", true).unwrap();
    let id = connection
        .send_request(json!({"type":"media_player/browse_media","entity_id":"media_player.test"}))
        .unwrap();
    connection.request(json!({"type":"ping"})).unwrap();
    assert_eq!(connection.poll().unwrap().unwrap()["type"], "event");
    let result = connection.poll().unwrap().unwrap();
    assert_eq!(result["id"], id);
    assert_eq!(result["result"]["children"][0]["title"], "Album");
    server.join().unwrap();
}

#[test]
fn late_timed_out_result_does_not_break_a_later_heartbeat() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener);
        send(&mut socket, &json!({"type":"auth_required"}));
        receive(&mut socket);
        send(&mut socket, &json!({"type":"auth_ok"}));
        let expired = receive(&mut socket);
        let ping = receive(&mut socket);
        reply(&mut socket, &expired, &json!({"late":true}));
        send(&mut socket, &json!({"id":ping["id"],"type":"pong"}));
    });
    let mut connection =
        Connection::connect(&format!("http://{address}"), "test-token", true).unwrap();
    let id = connection
        .send_request(json!({"type":"media_player/browse_media","entity_id":"media_player.test"}))
        .unwrap();
    connection.forget_request(id);
    connection.request(json!({"type":"ping"})).unwrap();
    server.join().unwrap();
}

#[test]
fn endpoint_consent_and_credential_boundary() {
    assert_eq!(
        websocket_url("http://home.local:8123", false),
        Err(ClientError::HttpNotAllowed)
    );
    assert_eq!(
        websocket_url("https://home.example", false)
            .expect("url")
            .as_str(),
        "wss://home.example/api/websocket"
    );
    assert_eq!(
        websocket_url("http://127.0.0.1:8123/", true)
            .expect("url")
            .as_str(),
        "ws://127.0.0.1:8123/api/websocket"
    );
    for invalid in [
        "ftp://example.com",
        "https://secret@example.com",
        "https://example.com?token=secret",
        "https://example.com/#fragment",
        "not a url",
    ] {
        assert_eq!(
            websocket_url(invalid, true),
            Err(ClientError::InvalidEndpoint)
        );
    }
}

fn serve_management(listener: &TcpListener, ha_private: String, tablet_public: String) {
    let mut socket = accept(listener);
    send(
        &mut socket,
        &json!({"type":"auth_required","ha_version":"test"}),
    );
    assert_eq!(receive(&mut socket)["type"], "auth");
    send(&mut socket, &json!({"type":"auth_ok"}));
    let start = receive(&mut socket);
    assert_eq!(start["type"], "njorun/session_start");
    let context = [42_u8; 32];
    reply(
        &mut socket,
        &start,
        &json!({"session_id":"session","context":STANDARD.encode(context)}),
    );
    let private: [u8; 32] = STANDARD
        .decode(ha_private)
        .expect("key")
        .try_into()
        .expect("size");
    let public: [u8; 32] = STANDARD
        .decode(tablet_public)
        .expect("key")
        .try_into()
        .expect("size");
    let mut noise = Responder::new(&private, &public, &context).expect("responder");
    let handshake = receive(&mut socket);
    let request = STANDARD
        .decode(handshake["frame"].as_str().expect("frame"))
        .expect("base64");
    let response = noise.respond(&request).expect("respond");
    reply(
        &mut socket,
        &handshake,
        &json!({"frame":STANDARD.encode(response)}),
    );
    let subscribe = receive(&mut socket);
    let confirmation = STANDARD
        .decode(subscribe["frame"].as_str().expect("frame"))
        .expect("base64");
    let confirm: Value =
        serde_json::from_slice(&noise.open_frame(&confirmation).expect("decrypt")).expect("json");
    assert_eq!(
        confirm,
        json!({"type":"confirm","protocol":1,"binding_id":"binding","session_id":"session"})
    );
    // An unrelated HA event arrives before the subscription result.
    send(
        &mut socket,
        &json!({"type":"event","id":99,"event":{"marker":true}}),
    );
    reply(&mut socket, &subscribe, &Value::Null);
    let config = json!({"type":"configuration","operation_id":"op","revision":1,"settings":{"brightness":60},"expires_at":9_999_999_999_u64});
    let frame = noise
        .seal_frame(config.to_string().as_bytes())
        .expect("encrypt");
    send(
        &mut socket,
        &json!({"type":"event","id":subscribe["id"],"event":{"frame":STANDARD.encode(frame)}}),
    );
    let ack = receive(&mut socket);
    let frame = STANDARD
        .decode(ack["frame"].as_str().expect("frame"))
        .expect("base64");
    let payload: Value =
        serde_json::from_slice(&noise.open_frame(&frame).expect("ack decrypt")).expect("json");
    assert_eq!(payload["type"], "applied");
    reply(&mut socket, &ack, &Value::Null);
    let closed = json!({"type":"closed","protocol":1,"session_id":"session","reason":"expired"});
    let frame = noise
        .seal_frame(closed.to_string().as_bytes())
        .expect("close frame");
    send(
        &mut socket,
        &json!({"type":"event","id":subscribe["id"],"event":{"frame":STANDARD.encode(frame)}}),
    );
}

#[test]
fn management_session_interoperates_and_preserves_interleaved_events() {
    exercise_management(true);
}

#[test]
fn authenticated_close_ends_management_and_rejects_later_sends() {
    exercise_management(false);
}

fn exercise_management(check_replay: bool) {
    let tablet = generate_identity().expect("tablet identity");
    let ha = generate_identity().expect("HA identity");
    let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
    let endpoint = format!("http://{}", listener.local_addr().expect("address"));
    let credentials = Credentials {
        ha_url: endpoint,
        access_token: "local-test-token".into(),
        refresh_token: None,
        client_id: None,
        allow_http: true,
        tablet_id: "tablet".into(),
        binding_id: "binding".into(),
        tablet_private_key: tablet.private_key,
        ha_public_key: ha.public_key,
        capabilities: None,
    };
    let server =
        thread::spawn(move || serve_management(&listener, ha.private_key, tablet.public_key));
    let mut connection =
        Connection::connect(&credentials.ha_url, &credentials.access_token, true).expect("connect");
    let mut session =
        ManagementSession::establish(&mut connection, &credentials).expect("mutual proof");
    assert_eq!(
        connection.poll().expect("poll").expect("event")["event"]["marker"],
        true
    );
    let event = loop {
        if let Some(event) = connection.poll().expect("poll") {
            break event;
        }
    };
    let config = session.decode(&event).expect("authenticated config");
    assert_eq!(config["settings"]["brightness"], 60);
    session.send(&mut connection, &json!({"type":"applied","operation_id":"op","revision":1,"settings":config["settings"]})).expect("ack");
    if check_replay {
        // Authentication failure terminates this session; never keep decoding it.
        assert_eq!(session.decode(&event), Err(ClientError::Trust));
        assert_eq!(
            session.send(&mut connection, &json!({"type":"applied"})),
            Err(ClientError::Trust)
        );
    } else {
        let closed = connection.poll().expect("poll close").expect("close event");
        assert_eq!(session.decode(&closed), Err(ClientError::ManagementClosed));
        assert_eq!(session.decode(&event), Err(ClientError::ManagementClosed));
        assert_eq!(
            session.send(&mut connection, &json!({"type":"applied"})),
            Err(ClientError::ManagementClosed)
        );
    }
    server.join().expect("server completed");
}

#[test]
fn rejected_ha_authentication_does_not_proceed_to_management() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
    let endpoint = format!("http://{}", listener.local_addr().expect("address"));
    let server = thread::spawn(move || {
        let mut socket = accept(&listener);
        send(&mut socket, &json!({"type":"auth_required"}));
        receive(&mut socket);
        send(
            &mut socket,
            &json!({"type":"auth_invalid","message":"sensitive server detail"}),
        );
    });
    assert!(matches!(
        Connection::connect(&endpoint, "wrong", true),
        Err(ClientError::Authentication)
    ));
    server.join().expect("server");
}

#[test]
fn stored_identity_roundtrip_and_failed_write_preserve_previous_state() {
    let directory =
        std::env::temp_dir().join(format!("njorun-storage-test-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("directory");
    let path = directory.join("state.json");
    njorun_ha_client::storage::write(&path, &json!({"revision":1})).expect("write");
    let read: Value = njorun_ha_client::storage::read(&path).expect("read");
    assert_eq!(read, json!({"revision":1}));
    let oversized = "x".repeat(65537);
    assert_eq!(
        njorun_ha_client::storage::write(&path, &oversized),
        Err(ClientError::Storage)
    );
    let read: Value = njorun_ha_client::storage::read(&path).expect("read previous");
    assert_eq!(read, json!({"revision":1}));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    std::fs::remove_dir_all(directory).expect("cleanup");
}

#[test]
fn heartbeat_matches_pong_and_preserves_interleaved_events() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
    let address = listener.local_addr().expect("address");
    let server = thread::spawn(move || {
        let mut socket = accept(&listener);
        send(&mut socket, &json!({"type":"auth_required"}));
        assert_eq!(receive(&mut socket)["type"], "auth");
        send(&mut socket, &json!({"type":"auth_ok"}));
        let ping = receive(&mut socket);
        assert_eq!(ping["type"], "ping");
        send(
            &mut socket,
            &json!({"id":42,"type":"event","event":{"state":"on"}}),
        );
        send(&mut socket, &json!({"id":ping["id"],"type":"pong"}));
    });
    let mut client =
        Connection::connect(&format!("http://{address}"), "test", true).expect("connect");
    assert_eq!(
        client.request(json!({"type":"ping"})).expect("pong").1,
        Value::Null
    );
    assert_eq!(client.poll().expect("poll").expect("preserved")["id"], 42);
    server.join().expect("server");
}

#[test]
fn websocket_redirect_is_rejected_before_sending_ha_credentials() {
    use std::io::{Read, Write};
    let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
    let address = listener.local_addr().expect("address");
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept");
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        let mut request = Vec::new();
        let mut byte = [0_u8];
        while !request.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).expect("upgrade request");
            request.push(byte[0]);
            assert!(request.len() < 8192);
        }
        assert!(!String::from_utf8_lossy(&request).contains("private-test-token"));
        socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/stolen\r\nContent-Length: 0\r\n\r\n").expect("redirect");
    });
    assert!(matches!(
        Connection::connect(&format!("http://{address}"), "private-test-token", true),
        Err(ClientError::Connection)
    ));
    server.join().expect("server");
}

#[test]
fn snapshot_subscribes_before_fetch_and_preserves_interleaved_changes() {
    use njorun_ha_client::snapshot::Snapshot;
    let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
    let address = listener.local_addr().expect("address");
    let server = thread::spawn(move || {
        let mut socket = accept(&listener);
        send(&mut socket, &json!({"type":"auth_required"}));
        receive(&mut socket);
        send(&mut socket, &json!({"type":"auth_ok"}));
        let subscription = receive(&mut socket);
        assert_eq!(subscription["type"], "subscribe_events");
        assert_eq!(subscription["event_type"], "state_changed");
        reply(&mut socket, &subscription, &Value::Null);
        for command in [
            "config/area_registry/list",
            "config/device_registry/list",
            "config/entity_registry/list",
        ] {
            let request = receive(&mut socket);
            assert_eq!(request["type"], command);
            reply(&mut socket, &request, &json!([]));
        }
        let request = receive(&mut socket);
        assert_eq!(request["type"], "get_states");
        let state = json!({"entity_id":"light.one","state":"off","attributes":{},"last_updated":"2026-11-05T00:00:10Z"});
        send(
            &mut socket,
            &json!({"type":"event","id":42,"event":{"management":true}}),
        );
        let mut newer = state.clone();
        newer["state"] = "on".into();
        newer["last_updated"] = "2026-11-05T00:00:11Z".into();
        send(
            &mut socket,
            &json!({"type":"event","id":subscription["id"],"event":{"event_type":"state_changed","data":{"entity_id":"light.one","new_state":newer}}}),
        );
        reply(&mut socket, &request, &json!([state]));
    });
    let mut client =
        Connection::connect(&format!("http://{address}"), "test", true).expect("connect");
    let snapshot = Snapshot::fetch(&mut client).expect("snapshot");
    assert_eq!(snapshot.states["light.one"]["state"], "on");
    assert_eq!(snapshot.ordered_states()[0]["state"], "on");
    let unrelated = client.poll().expect("poll").expect("other subscription");
    assert_eq!(unrelated["id"], 42);
    server.join().expect("server");
}

#[test]
fn deferred_events_have_an_aggregate_byte_budget_and_release_it_on_poll() {
    for overflow in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
        let address = listener.local_addr().expect("address");
        let server = thread::spawn(move || {
            let mut socket = accept(&listener);
            send(&mut socket, &json!({"type":"auth_required"}));
            assert_eq!(receive(&mut socket)["type"], "auth");
            send(&mut socket, &json!({"type":"auth_ok"}));
            let event = json!({"id":42,"type":"event","event":"x".repeat(3 * 1024 * 1024)});
            for _ in 0..if overflow { 1 } else { 2 } {
                let request = receive(&mut socket);
                for _ in 0..if overflow { 3 } else { 2 } {
                    send(&mut socket, &event);
                }
                if !overflow {
                    reply(&mut socket, &request, &Value::Null);
                }
            }
        });
        let mut client =
            Connection::connect(&format!("http://{address}"), "test", true).expect("connect");
        if overflow {
            assert_eq!(
                client.request(json!({"type":"test"})),
                Err(ClientError::Limit)
            );
        } else {
            for _ in 0..2 {
                client
                    .request(json!({"type":"test"}))
                    .expect("bounded request");
                for _ in 0..2 {
                    assert_eq!(client.poll().expect("poll").expect("event")["id"], 42);
                }
            }
        }
        server.join().expect("server");
    }
}
