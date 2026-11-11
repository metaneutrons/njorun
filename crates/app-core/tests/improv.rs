//! Improv Serial against the frames Vellum's client wrote (protocol/fixtures/improv-frames.json).

use njorun_app_core::improv::{
    Accepted, Command, ErrorCode, Frame, Parser, Settings, State, check_settings, command_code,
    device_info_frame, encode_frame, error_frame, frame_type, parse_command, redirect_frame,
    result_frame, scan_frames, state_frame,
};
use serde_json::Value;

fn fixture() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../protocol/fixtures/improv-frames.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json")
}

fn from_hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex"))
        .collect()
}

fn one_frame(bytes: &[u8]) -> Frame {
    let mut frames = Parser::new().push(bytes);
    assert_eq!(frames.len(), 1, "exactly one frame");
    frames.remove(0)
}

fn command_of(item: &Value) -> Command {
    let frame = one_frame(&from_hex(item["frame"].as_str().unwrap()));
    parse_command(&frame).expect("a valid command")
}

fn settings(item: &Value) -> Settings {
    let expected = &item["command"];
    Settings {
        ssid: expected["ssid"].as_str().unwrap().to_owned(),
        password: expected["password"].as_str().unwrap().to_owned(),
        url: expected["url"].as_str().map(str::to_owned),
        voucher: expected["voucher"].as_str().map(str::to_owned),
        ntp: expected["ntp"].as_str().map(str::to_owned),
        utc: expected["utc"].as_i64(),
    }
}

#[test]
fn every_command_vellum_wrote_is_read_the_same_way() {
    for item in fixture()["host_to_device"].as_array().unwrap() {
        let name = item["name"].as_str().unwrap();
        let expected = &item["command"];
        let command = command_of(item);
        match expected["kind"].as_str().unwrap() {
            "get_state" => assert_eq!(command, Command::GetState, "{name}"),
            "scan_wifi" => assert_eq!(command, Command::ScanWifi, "{name}"),
            "get_device_info" => assert_eq!(command, Command::GetDeviceInfo, "{name}"),
            "unknown" => assert_eq!(
                command,
                Command::Unknown(u8::try_from(expected["cmd"].as_u64().unwrap()).unwrap()),
                "{name}"
            ),
            "wifi_settings" => assert_eq!(command, Command::WifiSettings(settings(item)), "{name}"),
            other => panic!("unknown kind {other}"),
        }
    }
}

#[test]
fn every_frame_the_device_sends_is_what_vellum_expects() {
    for item in fixture()["device_to_host"].as_array().unwrap() {
        let name = item["name"].as_str().unwrap();
        let wanted = from_hex(item["frame"].as_str().unwrap());
        let built = match item["kind"].as_str().unwrap() {
            "state" => {
                let state = match item["state"].as_u64().unwrap() {
                    2 => State::Ready,
                    3 => State::Provisioning,
                    4 => State::Provisioned,
                    other => panic!("state {other}"),
                };
                state_frame(state)
            }
            "error" => {
                let error = match item["error"].as_u64().unwrap() {
                    3 => ErrorCode::UnableToConnect,
                    5 => ErrorCode::AuthRequired,
                    other => panic!("error {other}"),
                };
                error_frame(error)
            }
            "result" => {
                let strings: Vec<&str> = item["strings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|text| text.as_str().unwrap())
                    .collect();
                result_frame(
                    u8::try_from(item["cmd"].as_u64().unwrap()).unwrap(),
                    &strings,
                )
                .expect("fits")
            }
            other => panic!("kind {other}"),
        };
        assert_eq!(built, wanted, "{name}");
    }
}

#[test]
fn the_answers_of_the_firmware_are_built_from_those_pieces() {
    assert_eq!(
        device_info_frame("0.2.0-d1001"),
        result_frame(
            command_code::GET_DEVICE_INFO,
            &["Njörun", "0.2.0-d1001", "ESP32-P4", "reTerminal D1001"]
        )
        .unwrap()
    );
    let frames = scan_frames(&[("Home Wi-Fi", -52, true), ("Guest", -71, false)]);
    assert_eq!(frames.len(), 3, "two networks and the end");
    assert_eq!(
        frames[0],
        result_frame(4, &["Home Wi-Fi", "-52", "YES"]).unwrap()
    );
    assert_eq!(frames[1], result_frame(4, &["Guest", "-71", "NO"]).unwrap());
    assert_eq!(frames[2], result_frame(4, &[]).unwrap());
    assert_eq!(
        redirect_frame("https://home.example.org"),
        result_frame(1, &["https://home.example.org"]).unwrap()
    );
}

#[test]
fn the_parser_finds_frames_in_console_noise_and_in_pieces() {
    let items = fixture();
    let get_state = from_hex(items["host_to_device"][0]["frame"].as_str().unwrap());
    let mut stream = b"I (4711) wifi: scan done\r\nIMPR".to_vec();
    stream.extend_from_slice(&get_state);
    stream.extend_from_slice(b"\r\nmore text");
    let mut parser = Parser::new();
    let mut found = Vec::new();
    for chunk in stream.chunks(3) {
        found.extend(parser.push(chunk));
    }
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].frame_type, frame_type::RPC_COMMAND);
    assert_eq!(parse_command(&found[0]), Ok(Command::GetState));
}

#[test]
fn a_frame_with_a_wrong_sum_is_skipped_and_the_next_one_is_found() {
    let items = fixture();
    let good = from_hex(items["host_to_device"][1]["frame"].as_str().unwrap());
    let mut bad = from_hex(items["host_to_device"][0]["frame"].as_str().unwrap());
    *bad.last_mut().unwrap() ^= 0xff;
    let mut stream = bad;
    stream.extend_from_slice(&good);
    let frames = Parser::new().push(&stream);
    assert_eq!(frames.len(), 1);
    assert_eq!(parse_command(&frames[0]), Ok(Command::ScanWifi));
}

#[test]
fn two_frames_in_one_chunk_and_the_magic_in_text_do_not_confuse_the_parser() {
    let items = fixture();
    let first = from_hex(items["host_to_device"][0]["frame"].as_str().unwrap());
    let second = from_hex(items["host_to_device"][2]["frame"].as_str().unwrap());
    let mut stream = b"IMPROV is a word\n".to_vec();
    stream.extend_from_slice(&first);
    stream.extend_from_slice(&second);
    let frames = Parser::new().push(&stream);
    assert_eq!(frames.len(), 2);
}

#[test]
fn endless_noise_does_not_grow_the_buffer_without_bound() {
    let mut parser = Parser::new();
    for _ in 0..1000 {
        assert_eq!(parser.push(&[b'x'; 64]), Vec::<Frame>::new());
    }
    // A header that announces a long frame and never finishes cannot hold the parser either.
    let mut parser = Parser::new();
    parser.push(b"IMPROV\x01\x03\xff");
    for _ in 0..1000 {
        parser.push(&[0_u8; 64]);
    }
    let items = fixture();
    let frame = from_hex(items["host_to_device"][0]["frame"].as_str().unwrap());
    // The stuck header costs at most the frame it announced, then the parser finds the next one.
    let mut found = Vec::new();
    for _ in 0..4 {
        found.extend(parser.push(&frame));
    }
    assert_ne!(found, Vec::<Frame>::new());
}

#[test]
fn malformed_commands_are_invalid_rpc() {
    let not_a_command = Frame {
        frame_type: frame_type::CURRENT_STATE,
        payload: vec![2],
    };
    assert_eq!(parse_command(&not_a_command), Err(ErrorCode::InvalidRpc));
    let wrong_length = Frame {
        frame_type: frame_type::RPC_COMMAND,
        payload: vec![1, 5, 1],
    };
    assert_eq!(parse_command(&wrong_length), Err(ErrorCode::InvalidRpc));
    let empty = Frame {
        frame_type: frame_type::RPC_COMMAND,
        payload: vec![],
    };
    assert_eq!(parse_command(&empty), Err(ErrorCode::InvalidRpc));

    let settings = |data: &[u8]| {
        let mut payload = vec![
            command_code::WIFI_SETTINGS,
            u8::try_from(data.len()).unwrap(),
        ];
        payload.extend_from_slice(data);
        parse_command(&Frame {
            frame_type: frame_type::RPC_COMMAND,
            payload,
        })
    };
    assert_eq!(settings(&[]), Err(ErrorCode::InvalidRpc), "no SSID");
    assert_eq!(
        settings(&[1, b'a']),
        Err(ErrorCode::InvalidRpc),
        "no password length"
    );
    assert_eq!(settings(&[0, 0]), Err(ErrorCode::InvalidRpc), "empty SSID");
    assert_eq!(
        settings(&[5, b'a']),
        Err(ErrorCode::InvalidRpc),
        "SSID cut off"
    );
    assert_eq!(
        settings(&[1, b'a', 1, b'b', 9, b'x']),
        Err(ErrorCode::InvalidRpc),
        "URL cut off"
    );
    assert_eq!(
        settings(&[1, 0xff, 0]),
        Err(ErrorCode::InvalidRpc),
        "SSID is not UTF-8"
    );
    let mut long_ssid = vec![33];
    long_ssid.extend_from_slice(&[b'a'; 33]);
    long_ssid.push(0);
    assert_eq!(
        settings(&long_ssid),
        Err(ErrorCode::InvalidRpc),
        "SSID over 32 bytes"
    );
    assert!(settings(&[1, b'a', 0]).is_ok(), "an open network");
}

#[test]
fn the_time_must_be_digits_in_the_accepted_range() {
    let with_time = |text: &str| {
        let mut data = vec![1, b'a', 0, 0, 0, 0];
        data.push(u8::try_from(text.len()).unwrap());
        data.extend_from_slice(text.as_bytes());
        let mut payload = vec![
            command_code::WIFI_SETTINGS,
            u8::try_from(data.len()).unwrap(),
        ];
        payload.extend_from_slice(&data);
        parse_command(&Frame {
            frame_type: frame_type::RPC_COMMAND,
            payload,
        })
    };
    assert!(with_time("1790000000").is_ok());
    assert!(with_time("1704067200").is_ok());
    assert!(with_time("4102444799").is_ok());
    for bad in [
        "1704067199",
        "4102444800",
        "",
        "12ab",
        "-5",
        "99999999999999999999",
    ] {
        assert_eq!(with_time(bad), Err(ErrorCode::InvalidRpc), "{bad:?}");
    }
}

fn parsed(name: &str) -> Settings {
    let items = fixture();
    let item = items["host_to_device"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == name)
        .unwrap()
        .clone();
    match command_of(&item) {
        Command::WifiSettings(settings) => settings,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_profile_is_taken_while_the_tablet_is_not_paired() {
    let accepted = check_settings(&parsed("wifi_url_and_time"), false).unwrap();
    assert_eq!(
        accepted,
        Accepted {
            ssid: "MyNet".into(),
            password: "s3cret!!".into(),
            url: Some("https://home.example.org".into()),
            utc: Some(1_790_000_000),
        }
    );
    assert!(check_settings(&parsed("wifi_minimal"), false).is_ok());
    assert!(check_settings(&parsed("wifi_open_utf8"), false).is_ok());
}

#[test]
fn a_paired_tablet_takes_no_profile() {
    assert_eq!(
        check_settings(&parsed("wifi_with_url"), true),
        Err(ErrorCode::AuthRequired)
    );
}

#[test]
fn a_voucher_or_an_ntp_server_is_refused_until_a_later_slice_takes_them() {
    assert_eq!(
        check_settings(&parsed("wifi_voucher_and_ntp"), false),
        Err(ErrorCode::InvalidRpc)
    );
    // The placeholders that Vellum's client writes before a time are empty strings.
    assert!(check_settings(&parsed("wifi_url_and_time"), false).is_ok());
}

#[test]
fn an_address_that_is_no_base_url_is_refused() {
    for bad in [
        "ftp://h",
        "home",
        "https://",
        "https://user@h",
        "https://h/?x=1",
    ] {
        let mut profile = parsed("wifi_with_url");
        profile.url = Some(bad.to_owned());
        assert_eq!(
            check_settings(&profile, false),
            Err(ErrorCode::InvalidRpc),
            "{bad}"
        );
    }
}

#[test]
fn a_frame_cannot_carry_more_than_255_bytes() {
    assert!(encode_frame(frame_type::RPC_RESULT, &[0; 255]).is_some());
    assert!(encode_frame(frame_type::RPC_RESULT, &[0; 256]).is_none());
    assert!(result_frame(4, &[&"x".repeat(256)]).is_none());
}
