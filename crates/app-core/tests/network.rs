//! Network settings, the link choice and the trial (plan 006), against
//! protocol/fixtures/network-cases.json for the shape.

use njorun_app_core::link::{NO_SESSION_MS, Observation, RETRY_WIRED_MS, Selector};
use njorun_app_core::network::{
    EthernetConfig, Interface, IpConfig, Mode, NetworkConfig, Problem, WifiConfig,
};
use njorun_app_core::trial::{PROOF_TIMEOUT_MS, Proof, Reason, Step, Stored, Trial};
use serde_json::Value;

fn cases() -> Vec<Value> {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../protocol/fixtures/network-cases.json"
    ))
    .expect("fixture");
    serde_json::from_str(&text).expect("fixture is JSON")
}

fn interfaces(case: &Value) -> Vec<Interface> {
    case["interfaces"]
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

/// Parsing and validation together, as the tablet does when a message arrives.
fn check(case: &Value) -> Result<(), String> {
    let config: NetworkConfig = serde_json::from_value(case["config"].clone())
        .map_err(|error| format!("parse: {error}"))?;
    config
        .validate(&interfaces(case))
        .map_err(|problem| format!("{problem:?}"))
}

#[test]
fn every_case_of_the_fixture_is_decided_as_written() {
    let cases = cases();
    assert!(cases.len() >= 40);
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let result = check(case);
        if case["valid"].as_bool().unwrap() {
            assert_eq!(result, Ok(()), "{name}");
        } else {
            let error = result.expect_err(name);
            let expected = case["problem"].as_str().unwrap();
            let got = match expected {
                "parse" => error.starts_with("parse"),
                "empty" => error == "Empty",
                "no_such_interface" => error.starts_with("NoSuchInterface"),
                other => error.eq_ignore_ascii_case(other),
            };
            assert!(got, "{name}: expected {expected}, got {error}");
        }
    }
}

#[test]
fn a_valid_configuration_survives_a_round_trip_through_json() {
    for case in cases().iter().filter(|case| case["valid"] == true) {
        let config: NetworkConfig = serde_json::from_value(case["config"].clone()).unwrap();
        let again: NetworkConfig =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(config, again, "{}", case["name"]);
    }
}

#[test]
fn the_password_is_not_in_the_debug_text() {
    let config = WifiConfig {
        ssid: "Home".into(),
        password: Some("correct horse battery".into()),
        ip: IpConfig::dhcp(),
    };
    let shown = format!("{config:?}");
    assert!(shown.contains("Home"));
    assert!(!shown.contains("correct horse"));
    assert!(shown.contains("redacted"));
}

#[test]
fn problems_read_as_sentences() {
    assert_eq!(
        Problem::NoSuchInterface(Interface::Ethernet).to_string(),
        "this board has no Ethernet"
    );
    assert!(Problem::Gateway.to_string().contains("subnet"));
}

const fn seen(wired: bool, wifi: bool, session: bool) -> Observation {
    Observation {
        wired_ready: wired,
        wifi_ready: wifi,
        session_open: session,
    }
}

#[test]
fn the_cable_wins_as_soon_as_it_has_an_address() {
    let mut selector = Selector::new();
    assert_eq!(
        selector.update(0, seen(false, true, false)),
        Some(Interface::Wifi)
    );
    assert_eq!(
        selector.update(1_000, seen(true, true, true)),
        Some(Interface::Ethernet)
    );
    assert_eq!(selector.chosen(), Some(Interface::Ethernet));
}

#[test]
fn without_any_link_nothing_is_chosen() {
    let mut selector = Selector::new();
    assert_eq!(selector.update(0, seen(false, false, false)), None);
}

#[test]
fn a_lost_cable_falls_back_to_wifi_at_once_and_returns_when_it_is_back() {
    let mut selector = Selector::new();
    selector.update(0, seen(true, true, true));
    assert_eq!(
        selector.update(1_000, seen(false, true, false)),
        Some(Interface::Wifi)
    );
    assert_eq!(
        selector.update(2_000, seen(true, true, false)),
        Some(Interface::Ethernet)
    );
}

#[test]
fn a_cable_that_is_up_but_reaches_nothing_hands_over_after_a_minute() {
    let mut selector = Selector::new();
    assert_eq!(
        selector.update(0, seen(true, true, false)),
        Some(Interface::Ethernet)
    );
    assert_eq!(
        selector.update(NO_SESSION_MS - 1, seen(true, true, false)),
        Some(Interface::Ethernet)
    );
    assert_eq!(
        selector.update(NO_SESSION_MS, seen(true, true, false)),
        Some(Interface::Wifi)
    );
}

#[test]
fn the_cable_is_tried_again_after_the_wait_and_gets_a_full_minute() {
    let mut selector = Selector::new();
    selector.update(0, seen(true, true, false));
    let handed_over = NO_SESSION_MS;
    assert_eq!(
        selector.update(handed_over, seen(true, true, false)),
        Some(Interface::Wifi)
    );
    // The session now runs over Wi-Fi.
    assert_eq!(
        selector.update(handed_over + RETRY_WIRED_MS - 1, seen(true, true, true)),
        Some(Interface::Wifi)
    );
    let retry = handed_over + RETRY_WIRED_MS;
    assert_eq!(
        selector.update(retry, seen(true, true, true)),
        Some(Interface::Ethernet)
    );
    // The probe has no session: it keeps the cable for the full minute, then hands over again.
    assert_eq!(
        selector.update(retry + NO_SESSION_MS - 1, seen(true, true, false)),
        Some(Interface::Ethernet)
    );
    assert_eq!(
        selector.update(retry + NO_SESSION_MS, seen(true, true, false)),
        Some(Interface::Wifi)
    );
}

#[test]
fn a_cable_that_carries_a_session_is_kept() {
    let mut selector = Selector::new();
    for step in 0..20 {
        assert_eq!(
            selector.update(step * NO_SESSION_MS, seen(true, true, true)),
            Some(Interface::Ethernet)
        );
    }
}

#[test]
fn the_cable_stays_when_there_is_nothing_to_fall_back_to() {
    let mut selector = Selector::new();
    for step in 0..5 {
        assert_eq!(
            selector.update(step * NO_SESSION_MS, seen(true, false, false)),
            Some(Interface::Ethernet)
        );
    }
}

#[test]
fn a_cable_that_returns_is_not_held_back_by_an_old_hand_over() {
    let mut selector = Selector::new();
    selector.update(0, seen(true, true, false));
    selector.update(NO_SESSION_MS, seen(true, true, false));
    assert_eq!(selector.chosen(), Some(Interface::Wifi));
    selector.update(NO_SESSION_MS + 1_000, seen(false, true, true));
    assert_eq!(
        selector.update(NO_SESSION_MS + 2_000, seen(true, true, true)),
        Some(Interface::Ethernet)
    );
}

#[test]
fn a_full_proof_commits_when_the_session_opens() {
    let mut trial = Trial::start(1_000, Proof::Session);
    assert_eq!(trial.joined(), Step::Continue);
    assert_eq!(trial.address(), Step::Continue);
    assert_eq!(trial.session_opened(), Step::Commit(Proof::Session));
    // Nothing after the end changes the outcome.
    assert_eq!(trial.tick(1_000 + PROOF_TIMEOUT_MS), Step::Continue);
}

#[test]
fn a_session_before_an_address_does_not_count() {
    let mut trial = Trial::start(0, Proof::Session);
    assert_eq!(trial.session_opened(), Step::Continue);
    trial.joined();
    assert_eq!(trial.session_opened(), Step::Continue);
}

#[test]
fn the_weaker_proof_needs_the_tcp_connection_and_not_a_session() {
    let mut trial = Trial::start(0, Proof::Link);
    trial.joined();
    trial.address();
    assert_eq!(trial.session_opened(), Step::Continue);
    assert_eq!(trial.reached(), Step::Commit(Proof::Link));
    let mut full = Trial::start(0, Proof::Session);
    full.joined();
    full.address();
    assert_eq!(full.reached(), Step::Continue);
}

#[test]
fn the_reason_at_the_deadline_is_where_the_attempt_stopped() {
    let end = PROOF_TIMEOUT_MS;
    let mut joining = Trial::start(0, Proof::Session);
    assert_eq!(joining.tick(end - 1), Step::Continue);
    assert_eq!(joining.tick(end), Step::Revert(Reason::NoAssociation));
    let mut addressing = Trial::start(0, Proof::Session);
    addressing.joined();
    assert_eq!(addressing.tick(end), Step::Revert(Reason::NoAddress));
    let mut waiting = Trial::start(0, Proof::Session);
    waiting.joined();
    waiting.address();
    assert_eq!(waiting.tick(end), Step::Revert(Reason::NoHomeAssistant));
}

#[test]
fn a_failure_from_the_driver_reverts_at_once_and_only_once() {
    let mut trial = Trial::start(0, Proof::Session);
    assert_eq!(
        trial.failed(Reason::NoAssociation),
        Step::Revert(Reason::NoAssociation)
    );
    assert_eq!(trial.failed(Reason::NoAddress), Step::Continue);
    assert_eq!(trial.tick(PROOF_TIMEOUT_MS), Step::Continue);
}

#[test]
fn the_words_of_the_result_are_the_ones_of_the_plan() {
    assert_eq!(Proof::Session.wire(), "session");
    assert_eq!(Proof::Link.wire(), "link");
    assert_eq!(Reason::NoAssociation.wire(), "no_association");
    assert_eq!(Reason::NoAddress.wire(), "no_address");
    assert_eq!(Reason::NoHomeAssistant.wire(), "no_home_assistant");
    assert_eq!(Reason::RejectedValues.wire(), "rejected_values");
}

fn wifi(ssid: &str) -> WifiConfig {
    WifiConfig {
        ssid: ssid.into(),
        password: Some("correct horse".into()),
        ip: IpConfig::dhcp(),
    }
}

#[test]
fn the_stored_values_change_only_when_the_candidate_is_committed() {
    let old = NetworkConfig {
        wifi: Some(wifi("Old")),
        ethernet: None,
    };
    let new = NetworkConfig {
        wifi: Some(wifi("New")),
        ethernet: None,
    };
    let mut stored = Stored::new(old.clone());
    stored.begin(&new);
    assert!(stored.trying());
    assert_eq!(stored.in_use(), &new);
    assert_eq!(
        stored.persisted(),
        &old,
        "a reset now ends on the old values"
    );
    stored.abandon();
    assert!(!stored.trying());
    assert_eq!(stored.in_use(), &old);
    stored.begin(&new);
    assert_eq!(stored.commit(), &new);
    assert_eq!(stored.persisted(), &new);
    assert!(!stored.trying());
}

#[test]
fn an_interface_the_candidate_leaves_out_keeps_its_values() {
    let mut stored = Stored::new(NetworkConfig {
        wifi: Some(wifi("Home")),
        ethernet: Some(EthernetConfig {
            ip: IpConfig {
                mode: Mode::Static,
                address: Some("192.168.1.50".parse().unwrap()),
                prefix: Some(24),
                gateway: Some("192.168.1.1".parse().unwrap()),
                dns: Vec::new(),
            },
        }),
    });
    stored.begin(&NetworkConfig {
        wifi: Some(wifi("Other")),
        ethernet: None,
    });
    let used = stored.in_use();
    assert_eq!(used.wifi.as_ref().unwrap().ssid, "Other");
    assert!(used.ethernet.is_some());
}
