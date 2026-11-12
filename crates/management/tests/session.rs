//! The tablet side of the management session against a responder playing Home Assistant.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use njorun_management::{
    Announcement, Error, Handshake, Session, announce_request, comparison_code, decode_key,
    encode_key, live_parts, network_offered, ota_offered, parse_announce, public_key,
    session_start_request,
};
use njorun_pairing::Responder;
use serde_json::{Value, json};

const TABLET_PRIVATE: [u8; 32] = [11; 32];
const HA_PRIVATE: [u8; 32] = [22; 32];
const CONTEXT: [u8; 32] = [33; 32];
const SUBSCRIPTION: u64 = 7;

struct FakeHa {
    responder: Responder,
}

impl FakeHa {
    fn new() -> Self {
        let tablet_public = public_key(&TABLET_PRIVATE).expect("tablet key");
        Self {
            responder: Responder::new(&HA_PRIVATE, &tablet_public, &CONTEXT).expect("responder"),
        }
    }

    fn start_result() -> Value {
        json!({"session_id": "session-1", "context": encode_key(&CONTEXT), "expires_at": 1})
    }

    fn handshake(&mut self, request: &Value) -> Value {
        assert_eq!(request["type"], "njorun/session_handshake");
        assert_eq!(request["session_id"], "session-1");
        let first = STANDARD
            .decode(request["frame"].as_str().expect("frame"))
            .expect("base64");
        assert_eq!(first.len(), 96);
        let response = self
            .responder
            .respond(&first)
            .expect("tablet proves its key");
        json!({"frame": STANDARD.encode(response)})
    }

    fn open(&mut self, request: &Value) -> Value {
        let frame = STANDARD
            .decode(request["frame"].as_str().expect("frame"))
            .expect("base64");
        serde_json::from_slice(&self.responder.open_frame(&frame).expect("authentic"))
            .expect("json")
    }

    fn event(&mut self, message: &Value) -> Value {
        let frame = self
            .responder
            .seal_frame(message.to_string().as_bytes())
            .expect("seal");
        json!({"id": SUBSCRIPTION, "type": "event", "event": {"frame": STANDARD.encode(frame)}})
    }
}

fn establish(ha: &mut FakeHa) -> Session {
    let ha_public = public_key(&HA_PRIVATE).expect("ha key");
    let (handshake, request) = Handshake::begin(
        &FakeHa::start_result(),
        "binding-1",
        &TABLET_PRIVATE,
        &ha_public,
    )
    .expect("begin");
    let result = ha.handshake(&request);
    let (mut session, subscribe) = handshake.finish(&result, None).expect("finish");
    assert_eq!(subscribe["type"], "njorun/session_subscribe");
    assert_eq!(subscribe["session_id"], "session-1");
    assert_eq!(
        ha.open(&subscribe),
        json!({"type": "confirm", "protocol": 1, "binding_id": "binding-1", "session_id": "session-1"})
    );
    session.attach(SUBSCRIPTION);
    session
}

#[test]
fn requests_have_the_documented_shape() {
    assert_eq!(
        session_start_request("binding-1"),
        json!({"type": "njorun/session_start", "protocol": 1, "binding_id": "binding-1"})
    );
    let key = [5_u8; 32];
    assert_eq!(decode_key(&encode_key(&key)), Ok(key));
    assert_eq!(decode_key("AAAA"), Err(Error::Trust));
    assert_eq!(decode_key(&"A".repeat(45)), Err(Error::Trust));
}

#[test]
fn configuration_flows_in_and_acknowledgements_flow_out() {
    let mut ha = FakeHa::new();
    let mut session = establish(&mut ha);
    assert_eq!(session.session_id(), "session-1");
    assert_eq!(session.subscription_id(), Some(SUBSCRIPTION));

    let configuration = json!({"type": "configuration", "operation_id": "op", "revision": 4,
                               "settings": {"theme": "dark"}, "expires_at": 2});
    let event = ha.event(&configuration);
    assert_eq!(session.decode(&event), Ok(configuration));

    let applied = json!({"type": "applied", "operation_id": "op", "revision": 4,
                         "settings": {"theme": "dark"}});
    let request = session.frame_request(&applied).expect("frame");
    assert_eq!(request["type"], "njorun/session_frame");
    assert_eq!(ha.open(&request), applied);
}

#[test]
fn foreign_tampered_and_replayed_events_are_refused() {
    let mut ha = FakeHa::new();
    let mut session = establish(&mut ha);
    let event = ha.event(&json!({"type": "configuration"}));
    let mut other = event.clone();
    other["id"] = json!(SUBSCRIPTION + 1);
    assert_eq!(session.decode(&other), Err(Error::Protocol));
    let mut result = event.clone();
    result["type"] = json!("result");
    assert_eq!(session.decode(&result), Err(Error::Protocol));
    let mut huge = event.clone();
    huge["event"]["frame"] = json!("A".repeat(5501));
    assert_eq!(session.decode(&huge), Err(Error::Limit));

    assert!(session.decode(&event).is_ok());
    assert_eq!(session.decode(&event), Err(Error::Trust), "replay");
    let next = ha.event(&json!({"type": "configuration"}));
    assert_eq!(
        session.decode(&next),
        Err(Error::Trust),
        "failure is terminal"
    );
    assert_eq!(
        session.frame_request(&json!({"type": "applied"})),
        Err(Error::Trust)
    );
}

#[test]
fn a_close_from_home_assistant_ends_the_session() {
    let mut ha = FakeHa::new();
    let mut session = establish(&mut ha);
    let malformed = ha.event(&json!({"type": "closed", "protocol": 1,
                                     "session_id": "session-1", "reason": "bored"}));
    assert_eq!(session.decode(&malformed), Err(Error::Protocol));
    let closed = ha.event(&json!({"type": "closed", "protocol": 1,
                                  "session_id": "session-1", "reason": "revoked"}));
    assert_eq!(session.decode(&closed), Err(Error::Closed));
    assert_eq!(session.frame_request(&json!({})), Err(Error::Closed));
    assert_eq!(
        Error::Closed.to_string(),
        "management session closed by Home Assistant"
    );
}

#[test]
fn a_wrong_pin_or_bad_start_result_fails_early() {
    let ha_public = public_key(&HA_PRIVATE).expect("ha key");
    for start in [
        json!({"context": encode_key(&CONTEXT)}),
        json!({"session_id": "", "context": encode_key(&CONTEXT)}),
        json!({"session_id": "x".repeat(129), "context": encode_key(&CONTEXT)}),
        json!({"session_id": "a\nb", "context": encode_key(&CONTEXT)}),
        json!({"session_id": "s"}),
    ] {
        assert_eq!(
            Handshake::begin(&start, "b", &TABLET_PRIVATE, &ha_public).err(),
            Some(Error::Protocol),
            "{start}"
        );
    }
    let short_context = json!({"session_id": "s", "context": "AAAA"});
    assert_eq!(
        Handshake::begin(&short_context, "b", &TABLET_PRIVATE, &ha_public).err(),
        Some(Error::Trust)
    );

    // The tablet pins another HA key: HA cannot answer, and a forged answer is refused.
    let mut ha = FakeHa::new();
    let wrong_pin = public_key(&[44; 32]).expect("key");
    let (handshake, request) =
        Handshake::begin(&FakeHa::start_result(), "b", &TABLET_PRIVATE, &wrong_pin).expect("begin");
    let first = STANDARD
        .decode(request["frame"].as_str().expect("frame"))
        .expect("base64");
    assert!(ha.responder.respond(&first).is_err());
    let forged = json!({"frame": STANDARD.encode([0_u8; 48])});
    assert_eq!(handshake.finish(&forged, None).err(), Some(Error::Trust));
}

#[test]
fn comparison_codes_match_the_shared_fixture() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../protocol/fixtures/comparison-code.json"
    ))
    .expect("fixture");
    for case in fixture["cases"].as_array().expect("cases") {
        let key = decode_key(case["tablet_public_key"].as_str().expect("key")).expect("32 bytes");
        assert_eq!(comparison_code(&key), case["code"].as_str().expect("code"));
    }
}

#[test]
fn announcements_are_built_and_read() {
    let tablet_public = public_key(&TABLET_PRIVATE).expect("key");
    let capabilities = json!({"orientations": ["portrait"], "brightness": true});
    let request = announce_request("njorun-1", "Kitchen", &tablet_public, &capabilities);
    assert_eq!(
        request,
        json!({"type": "njorun/announce", "protocol": 1, "tablet_id": "njorun-1",
               "name": "Kitchen", "tablet_public_key": encode_key(&tablet_public),
               "capabilities": capabilities})
    );
    let code = comparison_code(&tablet_public);
    assert_eq!(
        parse_announce(&json!({"status": "pending", "code": &code}), &tablet_public),
        Ok(Announcement::Pending { code: code.clone() })
    );
    let wrong = if code == "000000" { "000001" } else { "000000" };
    assert_eq!(
        parse_announce(&json!({"status": "pending", "code": wrong}), &tablet_public),
        Err(Error::Protocol),
        "a code that is not the tablet's own is refused"
    );
    let ha_public = public_key(&HA_PRIVATE).expect("key");
    assert_eq!(
        parse_announce(
            &json!({"status": "approved", "binding_id": "b1", "ha_public_key": encode_key(&ha_public)}),
            &tablet_public
        ),
        Ok(Announcement::Approved {
            binding_id: "b1".into(),
            ha_public
        })
    );
    assert_eq!(
        parse_announce(
            &json!({"status": "approved", "binding_id": "b1", "ha_public_key": "AAAA"}),
            &tablet_public
        ),
        Err(Error::Trust)
    );
    assert_eq!(
        parse_announce(&json!({"status": "conflict"}), &tablet_public),
        Ok(Announcement::Conflict)
    );
    assert_eq!(
        parse_announce(&json!({"status": "maybe"}), &tablet_public),
        Err(Error::Protocol)
    );
}

#[test]
fn live_messages_travel_both_ways_within_the_frame_limit() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../protocol/fixtures/live-messages.json"
    ))
    .expect("fixture");
    let mut ha = FakeHa::new();
    let mut session = establish(&mut ha);

    for command in fixture["command"]["valid"].as_array().expect("commands") {
        let event = ha.event(command);
        assert_eq!(session.decode(&event).as_ref(), Ok(command));
    }
    for kind in ["command_result", "telemetry"] {
        for message in fixture[kind]["valid"].as_array().expect("messages") {
            let request = session.frame_request(message).expect("frame");
            assert_eq!(request["type"], "njorun/session_frame");
            assert_eq!(&ha.open(&request), message);
        }
    }
}

#[test]
fn live_parts_come_from_the_subscribe_result() {
    assert_eq!(
        live_parts(&json!({"session_id": "s"})),
        Ok(vec![]),
        "an older integration"
    );
    assert_eq!(
        live_parts(&json!({"live": ["telemetry", "wake", "wake", "future"]})),
        Ok(vec!["telemetry", "wake"])
    );
    assert_eq!(live_parts(&json!({"live": "wake"})), Err(Error::Protocol));
    assert_eq!(live_parts(&json!({"live": [1]})), Err(Error::Protocol));
    assert_eq!(
        live_parts(&json!({"live": vec!["wake"; 9]})),
        Err(Error::Protocol)
    );
}

#[test]
fn install_messages_travel_both_ways_within_the_frame_limit() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../protocol/fixtures/install-messages.json"
    ))
    .expect("fixture");
    let mut ha = FakeHa::new();
    let mut session = establish(&mut ha);

    for install in fixture["install"]["valid"].as_array().expect("installs") {
        let event = ha.event(install);
        assert_eq!(session.decode(&event).as_ref(), Ok(install));
    }
    for kind in ["install_result", "install_progress", "install_outcome"] {
        for message in fixture[kind]["valid"].as_array().expect("messages") {
            let request = session.frame_request(message).expect("frame");
            assert_eq!(request["type"], "njorun/session_frame");
            assert_eq!(&ha.open(&request), message);
        }
    }
}

#[test]
fn network_comes_from_the_subscribe_result() {
    assert_eq!(network_offered(&json!({"session_id": "s"})), Ok(false));
    assert_eq!(network_offered(&json!({"network": true})), Ok(true));
    assert_eq!(network_offered(&json!({"network": false})), Ok(false));
    assert_eq!(
        network_offered(&json!({"network": "yes"})),
        Err(Error::Protocol)
    );
    assert_eq!(
        network_offered(&json!({"network": null})),
        Err(Error::Protocol)
    );
}

#[test]
fn network_messages_travel_both_ways_within_the_frame_limit() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../protocol/fixtures/network-messages.json"
    ))
    .expect("fixture");
    let mut ha = FakeHa::new();
    let mut session = establish(&mut ha);

    for case in fixture["accepted"].as_array().expect("accepted") {
        let event = ha.event(&case["message"]);
        assert_eq!(session.decode(&event).as_ref(), Ok(&case["message"]));
    }
    for message in fixture["results"]["valid"].as_array().expect("results") {
        let request = session.frame_request(message).expect("frame");
        assert_eq!(request["type"], "njorun/session_frame");
        assert_eq!(&ha.open(&request), message);
    }
}

#[test]
fn ota_comes_from_the_subscribe_result() {
    assert_eq!(ota_offered(&json!({"session_id": "s"})), Ok(false));
    assert_eq!(ota_offered(&json!({"ota": true})), Ok(true));
    assert_eq!(ota_offered(&json!({"ota": false})), Ok(false));
    assert_eq!(ota_offered(&json!({"ota": "yes"})), Err(Error::Protocol));
    assert_eq!(ota_offered(&json!({"ota": null})), Err(Error::Protocol));
}

#[test]
fn the_confirmation_carries_the_tablet_capabilities_from_the_shared_fixture() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../protocol/fixtures/confirm-messages.json"
    ))
    .expect("fixture");
    let base = fixture["confirm"]["base"].clone();
    for capabilities in fixture["confirm"]["capabilities"]["valid"]
        .as_array()
        .expect("valid")
    {
        let mut ha = FakeHa::new();
        let ha_public = public_key(&HA_PRIVATE).expect("ha key");
        let (handshake, request) = Handshake::begin(
            &FakeHa::start_result(),
            "binding-1",
            &TABLET_PRIVATE,
            &ha_public,
        )
        .expect("begin");
        let result = ha.handshake(&request);
        let (_, subscribe) = handshake
            .finish(&result, Some(capabilities))
            .expect("finish");
        let mut expected = base.clone();
        expected["capabilities"] = capabilities.clone();
        assert_eq!(ha.open(&subscribe), expected);
    }
}
