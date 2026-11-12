//! The announcement over HTTP and the grant (plan 004), against the shared test vector.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use njorun_management::pairing::{
    Grant, Reply, announce_body, display_code, hostname, open_grant, parse_reply, tablet_name,
};
use njorun_management::{Error, comparison_code, encode_key, public_key};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn fixture() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../protocol/fixtures/grant.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json")
}

fn seed_key(fixture: &Value, field: &str) -> [u8; 32] {
    let digest = Sha256::digest(fixture[field].as_str().expect("seed").as_bytes());
    digest.into()
}

struct Case {
    private: [u8; 32],
    public: [u8; 32],
    binding: String,
    grant: String,
    plaintext: Value,
}

fn case() -> Case {
    let fixture = fixture();
    let private = seed_key(&fixture, "tablet_private_key_seed");
    let public = public_key(&private).expect("public key");
    assert_eq!(
        encode_key(&public),
        fixture["tablet_public_key"].as_str().unwrap(),
        "the seed gives the published public key"
    );
    Case {
        private,
        public,
        binding: fixture["binding_id"].as_str().unwrap().to_owned(),
        grant: fixture["grant"].as_str().unwrap().to_owned(),
        plaintext: serde_json::from_str(fixture["plaintext"].as_str().unwrap()).unwrap(),
    }
}

fn approved(case: &Case, grant: Option<&str>) -> Vec<u8> {
    let mut reply = json!({
        "status": "approved",
        "binding_id": case.binding,
        "ha_public_key": STANDARD.encode([5_u8; 32]),
    });
    if let Some(grant) = grant {
        reply["grant"] = json!(grant);
    }
    reply.to_string().into_bytes()
}

#[test]
fn the_fixture_grant_opens_with_the_tablet_key() {
    let case = case();
    let grant = open_grant(&case.private, &case.binding, &case.grant).expect("opens");
    assert_eq!(grant.refresh_token, case.plaintext["refresh_token"]);
    assert_eq!(grant.client_id, case.plaintext["client_id"]);
}

#[test]
fn a_grant_does_not_open_for_another_key_binding_or_after_tampering() {
    let case = case();
    assert_eq!(
        open_grant(&[9; 32], &case.binding, &case.grant),
        Err(Error::Trust)
    );
    assert_eq!(
        open_grant(&case.private, "another-binding", &case.grant),
        Err(Error::Trust)
    );
    let mut bytes = STANDARD.decode(&case.grant).unwrap();
    bytes[40] ^= 1;
    assert_eq!(
        open_grant(&case.private, &case.binding, &STANDARD.encode(bytes)),
        Err(Error::Trust)
    );
    assert_eq!(
        open_grant(&case.private, &case.binding, "not base64!"),
        Err(Error::Protocol)
    );
}

#[test]
fn a_grant_is_never_printed_with_its_token() {
    let grant = Grant {
        refresh_token: "secret-token".to_owned(),
        client_id: "https://example.org/client".to_owned(),
    };
    let shown = format!("{grant:?}");
    assert!(!shown.contains("secret-token"));
    assert!(shown.contains("redacted"));
}

#[test]
fn the_body_carries_the_announcement_and_only_asks_for_a_grant_when_needed() {
    let case = case();
    let capabilities = json!({"orientations": ["portrait"], "brightness": true});
    let plain: Value = serde_json::from_str(&announce_body(
        "njorun-1",
        "Kitchen",
        &case.public,
        &capabilities,
        false,
        None,
    ))
    .unwrap();
    assert_eq!(plain["protocol"], 1);
    assert_eq!(plain["tablet_id"], "njorun-1");
    assert_eq!(plain["name"], "Kitchen");
    assert_eq!(plain["tablet_public_key"], encode_key(&case.public));
    assert_eq!(plain["capabilities"], capabilities);
    assert!(plain.get("need_grant").is_none());
    assert!(plain.get("type").is_none(), "no WebSocket command type");
    let needing: Value = serde_json::from_str(&announce_body(
        "njorun-1",
        "Kitchen",
        &case.public,
        &capabilities,
        true,
        Some("light"),
    ))
    .unwrap();
    assert_eq!(needing["need_grant"], true);
    assert_eq!(needing["theme"], "light");
    assert!(plain.get("theme").is_none());
}

#[test]
fn pending_shows_the_code_that_belongs_to_the_tablet_key() {
    let case = case();
    let own = comparison_code(&case.public);
    let body = json!({"status": "pending", "code": own}).to_string();
    assert_eq!(
        parse_reply(200, body.as_bytes(), &case.public, &case.private),
        Ok(Reply::Pending { code: own.clone() })
    );
    let tampered = json!({"status": "pending", "code": "000000"}).to_string();
    assert_ne!(own, "000000");
    assert_eq!(
        parse_reply(200, tampered.as_bytes(), &case.public, &case.private),
        Err(Error::Protocol)
    );
}

#[test]
fn approved_with_a_grant_gives_the_access() {
    let case = case();
    let reply = parse_reply(
        200,
        &approved(&case, Some(&case.grant)),
        &case.public,
        &case.private,
    );
    match reply {
        Ok(Reply::Approved {
            binding_id,
            ha_public,
            grant: Some(grant),
        }) => {
            assert_eq!(binding_id, case.binding);
            assert_eq!(ha_public, [5; 32]);
            assert_eq!(grant.refresh_token, case.plaintext["refresh_token"]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn approved_without_a_grant_says_so_and_a_broken_grant_is_an_error() {
    let case = case();
    assert!(matches!(
        parse_reply(200, &approved(&case, None), &case.public, &case.private),
        Ok(Reply::Approved { grant: None, .. })
    ));
    assert_eq!(
        parse_reply(
            200,
            &approved(&case, Some("AAAA")),
            &case.public,
            &case.private
        ),
        Err(Error::Trust)
    );
    let mut wrong = approved(&case, None);
    let mut value: Value = serde_json::from_slice(&wrong).unwrap();
    value["grant"] = json!(7);
    wrong = value.to_string().into_bytes();
    assert_eq!(
        parse_reply(200, &wrong, &case.public, &case.private),
        Err(Error::Protocol)
    );
}

#[test]
fn the_statuses_of_the_endpoint_are_told_apart() {
    let case = case();
    let read = |status: u16, body: &[u8]| parse_reply(status, body, &case.public, &case.private);
    assert_eq!(read(404, b""), Ok(Reply::NotInstalled));
    assert_eq!(read(400, br#"{"error":"invalid"}"#), Ok(Reply::Rejected));
    assert_eq!(
        read(503, br#"{"error":"too_many_waiting"}"#),
        Ok(Reply::Busy)
    );
    assert_eq!(
        read(429, br#"{"error":"rate_limited"}"#),
        Ok(Reply::RateLimited)
    );
    assert_eq!(read(200, br#"{"status":"conflict"}"#), Ok(Reply::Conflict));
    assert_eq!(read(500, b"oops"), Err(Error::Protocol));
    assert_eq!(read(200, b"not json"), Err(Error::Protocol));
    assert_eq!(read(200, br#"{"status":"nonsense"}"#), Err(Error::Protocol));
}

#[test]
fn the_code_is_shown_in_two_groups() {
    assert_eq!(display_code("123456"), "123 456");
    assert_eq!(display_code("12"), "12");
}

#[test]
fn the_tablet_proposes_a_name_without_the_brand() {
    assert_eq!(
        tablet_name("njorun-f3e28163bf3adb69164fece1a551b586"),
        "D1001 f3e281"
    );
    assert_eq!(
        tablet_name("short"),
        "D1001 short",
        "an ID without a suffix is used as it is"
    );
    assert!(!tablet_name("njorun-f3e28163bf3adb69164fece1a551b586").contains("Njörun"));
}

#[test]
fn the_dhcp_host_name_is_the_model_and_the_end_of_the_id() {
    assert_eq!(
        hostname("njorun-f3e28163bf3adb69164fece1a551b586"),
        "d1001-f3e281"
    );
    assert_eq!(hostname("njorun-F3E281ABC"), "d1001-f3e281");
    assert_eq!(hostname("x"), "d1001-x");
    assert_eq!(hostname(""), "d1001");
    assert_eq!(hostname("!!!"), "d1001");
    for id in ["njorun-f3e28163bf3adb69", "short", "a b c d e f g h i"] {
        let name = hostname(id);
        assert!(name.len() <= 31, "{name}");
        assert!(name.starts_with('d'));
        assert!(
            name.bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        );
    }
}
