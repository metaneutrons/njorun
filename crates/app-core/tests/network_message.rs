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

mod deciding {
    use super::*;
    use njorun_app_core::network::{IpConfig, NetworkConfig, WifiConfig};
    use njorun_app_core::network_message::{Pending, Plan, Request, plan};

    fn wifi(ssid: &str, password: Option<&str>, ip: IpConfig) -> WifiConfig {
        WifiConfig {
            ssid: ssid.to_owned(),
            password: password.map(str::to_owned),
            ip,
        }
    }

    fn request(revision: u64, wifi: WifiConfig) -> Request {
        Request {
            operation_id: "op".to_owned(),
            revision,
            config: NetworkConfig {
                wifi: Some(wifi),
                ethernet: None,
            },
        }
    }

    const fn current(wifi: WifiConfig) -> NetworkConfig {
        NetworkConfig {
            wifi: Some(wifi),
            ethernet: None,
        }
    }

    fn fixed() -> IpConfig {
        IpConfig::from_text("192.168.1.50", "24", "192.168.1.1", "192.168.1.1").expect("static")
    }

    #[test]
    fn other_values_are_tried() {
        let stored = current(wifi("Home", Some("correct horse"), IpConfig::dhcp()));
        let new = request(5, wifi("Home", Some("another one"), IpConfig::dhcp()));
        assert_eq!(plan(&new, 4, &stored), Plan::Try);
        let new = request(5, wifi("Home", Some("correct horse"), fixed()));
        assert_eq!(plan(&new, 4, &stored), Plan::Try);
        assert_eq!(plan(&new, 0, &NetworkConfig::default()), Plan::Try);
    }

    #[test]
    fn the_values_in_use_are_answered_without_a_trial() {
        let stored = current(wifi("Home", Some("correct horse"), fixed()));
        let same = request(5, wifi("Home", Some("correct horse"), fixed()));
        assert_eq!(plan(&same, 4, &stored), Plan::AlreadyApplied);
        assert_eq!(plan(&same, 5, &stored), Plan::AlreadyApplied);
    }

    #[test]
    fn an_open_network_is_the_same_with_or_without_an_empty_password() {
        let stored = current(wifi("Guest", None, IpConfig::dhcp()));
        let same = request(2, wifi("Guest", Some(""), IpConfig::dhcp()));
        assert_eq!(plan(&same, 1, &stored), Plan::AlreadyApplied);
    }

    #[test]
    fn a_finished_revision_with_other_values_is_stale() {
        let stored = current(wifi("Home", Some("correct horse"), IpConfig::dhcp()));
        let other = request(4, wifi("Home", Some("another one"), IpConfig::dhcp()));
        assert_eq!(plan(&other, 4, &stored), Plan::Stale);
    }

    #[test]
    fn pending_results_survive_storage_and_nothing_else_is_read() {
        for outcome in [
            Outcome::Applied(Proof::Session),
            Outcome::Applied(Proof::Link),
            Outcome::Reverted(Reason::NoAssociation),
            Outcome::Reverted(Reason::NoAddress),
            Outcome::Reverted(Reason::NoHomeAssistant),
            Outcome::Reverted(Reason::RejectedValues),
        ] {
            let pending = Pending {
                operation_id: "op-1".to_owned(),
                revision: 7,
                outcome,
            };
            assert_eq!(Pending::decode(&pending.encode()), Some(pending));
        }
        for text in [
            "",
            "not json",
            "[]",
            r#"{"operation_id":"op","revision":1,"result":"applied"}"#,
            r#"{"operation_id":"op","revision":1,"result":"applied","proof":"guess"}"#,
            r#"{"operation_id":"op","revision":1,"result":"reverted","reason":"bad"}"#,
            r#"{"operation_id":"op","revision":0,"result":"applied","proof":"session"}"#,
            r#"{"operation_id":"","revision":1,"result":"applied","proof":"session"}"#,
            r#"{"operation_id":"op","revision":1,"result":"applied","proof":"session","reason":"no_address"}"#,
        ] {
            assert_eq!(Pending::decode(text), None, "{text}");
        }
    }
}
