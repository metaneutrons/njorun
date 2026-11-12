//! The `network` message and its result (plan 006), against
//! protocol/fixtures/network-messages.json.

use njorun_app_core::network::{Interface, Mode, Problem};
use njorun_app_core::network_message::{
    InterfaceState, Outcome, Refusal, Rejection, State, accept_network, result_message,
};
use njorun_app_core::trial::{Proof, Reason};
use serde_json::Value;

fn fixture() -> Value {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../protocol/fixtures/network-messages.json"
    ))
    .expect("fixture");
    serde_json::from_str(&text).expect("fixture is JSON")
}

fn interfaces(fixture: &Value) -> Vec<Interface> {
    fixture["interfaces"]
        .as_array()
        .expect("interfaces")
        .iter()
        .map(|name| match name.as_str().expect("name") {
            "wifi" => Interface::Wifi,
            "ethernet" => Interface::Ethernet,
            other => panic!("unknown interface {other}"),
        })
        .collect()
}

fn gate(fixture: &Value, message: &Value) -> Result<(), Refusal> {
    accept_network(
        message,
        fixture["now"].as_u64().expect("now"),
        fixture["last_revision"].as_u64().expect("last revision"),
        &interfaces(fixture),
    )
    .map(|_| ())
}

fn state() -> State {
    State {
        wifi: Some(InterfaceState {
            mode: Mode::Dhcp,
            address: Some("192.168.1.20".parse().expect("address")),
            ssid: Some("Home".to_owned()),
            carries_session: true,
        }),
        ethernet: None,
    }
}

#[test]
fn accepted_messages_pass_the_gate() {
    let fixture = fixture();
    for case in fixture["accepted"].as_array().expect("accepted") {
        let outcome = gate(&fixture, &case["message"]);
        assert!(outcome.is_ok(), "{}: {outcome:?}", case["name"]);
    }
}

#[test]
fn refused_messages_are_refused_for_the_stated_reason_and_answered_when_they_can_be() {
    let fixture = fixture();
    for case in fixture["refused"].as_array().expect("refused") {
        let name = &case["name"];
        let refusal = gate(&fixture, &case["message"]).expect_err(&name.to_string());
        let expected = match case["rejection"].as_str().expect("rejection") {
            "malformed" => Rejection::Malformed,
            "expired" => Rejection::Expired,
            "stale" => Rejection::Stale,
            "invalid" => {
                assert!(matches!(refusal.rejection, Rejection::Invalid(_)), "{name}");
                refusal.rejection
            }
            other => panic!("unknown rejection {other}"),
        };
        assert_eq!(refusal.rejection, expected, "{name}");
        let answer = refusal.result(&state());
        assert_eq!(
            answer.is_some(),
            case["answered"].as_bool().expect("answered"),
            "{name}"
        );
        if let Some(answer) = answer {
            assert_eq!(answer["result"], "reverted", "{name}");
            assert_eq!(answer["reason"], "rejected_values", "{name}");
            assert_eq!(answer["operation_id"], case["message"]["operation_id"]);
            assert!(answer.get("proof").is_none(), "{name}");
        }
    }
}

#[test]
fn the_values_of_an_accepted_message_are_what_was_sent() {
    let fixture = fixture();
    let message = &fixture["accepted"][1]["message"];
    let request = accept_network(message, 1_000_000_000, 4, &[Interface::Wifi]).expect("accepted");
    assert_eq!(request.operation_id, "op-1");
    assert_eq!(request.revision, 5);
    let wifi = request.config.wifi.expect("wifi");
    assert_eq!(wifi.ssid, "Home");
    assert_eq!(wifi.password.as_deref(), Some("correct horse"));
    assert_eq!(wifi.ip.mode, Mode::Static);
    assert_eq!(wifi.ip.prefix, Some(24));
    assert_eq!(wifi.ip.dns.len(), 2);
    assert!(request.config.ethernet.is_none());
}

#[test]
fn a_refusal_for_values_names_the_problem_but_the_answer_does_not() {
    let fixture = fixture();
    let refused = fixture["refused"]
        .as_array()
        .expect("refused")
        .iter()
        .find(|case| case["name"] == "short password")
        .expect("case");
    let refusal = gate(&fixture, &refused["message"]).expect_err("refused");
    assert_eq!(refusal.rejection, Rejection::Invalid(Problem::Password));
    let answer = refusal.result(&state()).expect("answered");
    assert!(!answer.to_string().contains("short"));
}

#[test]
fn results_match_the_fixture_and_hold_no_password() {
    let fixture = fixture();
    let results = &fixture["results"];
    let valid = results["valid"].as_array().expect("valid");
    let built = [
        result_message("op-1", 5, Outcome::Applied(Proof::Session), &state()),
        result_message("op-1", 5, Outcome::Applied(Proof::Link), &state()),
        result_message(
            "op-1",
            5,
            Outcome::Reverted(Reason::NoAssociation),
            &state(),
        ),
        result_message("op-1", 5, Outcome::Reverted(Reason::NoAddress), &state()),
        result_message(
            "op-1",
            5,
            Outcome::Reverted(Reason::NoHomeAssistant),
            &state(),
        ),
    ];
    for (index, message) in built.iter().enumerate() {
        assert_eq!(message, &valid[index], "result {index}");
        assert!(message.to_string().len() <= 768);
    }
    let unset = result_message(
        "op-1",
        5,
        Outcome::Reverted(Reason::RejectedValues),
        &State {
            wifi: Some(InterfaceState {
                mode: Mode::Static,
                address: None,
                ssid: Some("Home".to_owned()),
                carries_session: false,
            }),
            ethernet: None,
        },
    );
    assert_eq!(unset, valid[5]);
    assert_eq!(
        result_message(
            "op-1",
            5,
            Outcome::Applied(Proof::Session),
            &State::default()
        ),
        valid[6]
    );
}
