use njorun_pairing::{Initiator, MAX_FRAME_PLAINTEXT, PairingError, Responder, public_key};

mod tests {
    use super::{Initiator, MAX_FRAME_PLAINTEXT, PairingError, Responder, public_key};

    const TABLET_PRIVATE: [u8; 32] = [
        143, 222, 84, 178, 84, 233, 29, 254, 100, 111, 154, 206, 119, 204, 25, 168, 139, 112, 200,
        71, 241, 74, 186, 163, 33, 161, 139, 16, 72, 233, 128, 70,
    ];
    const TABLET_PUBLIC: [u8; 32] = [
        205, 183, 195, 253, 52, 136, 168, 56, 108, 171, 162, 182, 61, 53, 23, 161, 243, 160, 199,
        103, 59, 93, 51, 240, 244, 187, 96, 221, 38, 125, 234, 80,
    ];
    const HA_PRIVATE: [u8; 32] = [
        98, 18, 17, 22, 119, 189, 78, 253, 242, 5, 223, 54, 13, 21, 68, 10, 32, 163, 200, 232, 192,
        161, 76, 95, 166, 67, 154, 105, 38, 61, 66, 211,
    ];
    const HA_PUBLIC: [u8; 32] = [
        85, 147, 247, 64, 78, 135, 36, 47, 46, 246, 175, 171, 178, 127, 116, 211, 31, 1, 88, 160,
        164, 181, 1, 228, 34, 232, 95, 132, 109, 188, 129, 122,
    ];
    const OTHER_TABLET_PUBLIC: [u8; 32] = [
        173, 255, 196, 135, 10, 54, 158, 103, 185, 109, 252, 158, 35, 86, 164, 64, 57, 74, 224, 37,
        46, 97, 80, 168, 75, 149, 149, 114, 64, 85, 125, 109,
    ];
    const OTHER_HA_PUBLIC: [u8; 32] = [
        154, 112, 60, 84, 99, 235, 59, 98, 158, 190, 120, 15, 15, 214, 251, 60, 77, 138, 104, 175,
        94, 157, 176, 212, 150, 164, 14, 174, 51, 169, 38, 74,
    ];

    #[test]
    fn public_keys_derive_from_static_private_keys() {
        assert_eq!(public_key(&TABLET_PRIVATE), Ok(TABLET_PUBLIC));
        assert_eq!(public_key(&HA_PRIVATE), Ok(HA_PUBLIC));
        assert_ne!(public_key(&[7; 32]), public_key(&[8; 32]));
    }

    const fn context(value: u8) -> [u8; 32] {
        [value; 32]
    }

    fn endpoints(context_value: u8) -> (Initiator, Responder) {
        let pairing_session_context = context(context_value);
        let initiator = Initiator::new(&TABLET_PRIVATE, &HA_PUBLIC, &pairing_session_context)
            .expect("initiator should initialize with test fixtures");
        let responder = Responder::new(&HA_PRIVATE, &TABLET_PUBLIC, &pairing_session_context)
            .expect("responder should initialize with test fixtures");
        (initiator, responder)
    }

    fn established_endpoints(context_value: u8) -> (Initiator, Responder) {
        let (mut initiator, mut responder) = endpoints(context_value);
        let request = initiator.start().expect("initiator should create request");
        let response = responder
            .respond(&request)
            .expect("responder should authenticate tablet");
        initiator
            .finish(&response)
            .expect("initiator should authenticate Home Assistant");
        (initiator, responder)
    }

    #[test]
    fn handshake_and_configuration_frames_round_trip() {
        let (mut initiator, mut responder) = endpoints(7);
        assert_eq!(
            initiator.seal_frame(b"configuration"),
            Err(PairingError::WrongPhase)
        );
        assert_eq!(
            responder.seal_frame(b"configuration"),
            Err(PairingError::WrongPhase)
        );

        let request = initiator.start().expect("initiator should create request");
        assert_eq!(
            initiator.seal_frame(b"configuration"),
            Err(PairingError::WrongPhase)
        );
        let response = responder
            .respond(&request)
            .expect("responder should authenticate tablet");
        assert!(responder.is_established());
        assert_eq!(initiator.open_frame(&[]), Err(PairingError::WrongPhase));
        initiator
            .finish(&response)
            .expect("initiator should authenticate Home Assistant");
        assert!(initiator.is_established());

        let configuration = br#"{"brightness":42,"enabled":true}"#;
        let encrypted = initiator
            .seal_frame(configuration)
            .expect("initiator should encrypt configuration");
        assert_eq!(
            responder
                .open_frame(&encrypted)
                .expect("responder should decrypt configuration"),
            configuration
        );

        let acknowledgement = responder
            .seal_frame(b"applied")
            .expect("responder should encrypt acknowledgement");
        assert_eq!(
            initiator
                .open_frame(&acknowledgement)
                .expect("initiator should decrypt acknowledgement"),
            b"applied"
        );
    }

    #[test]
    fn pinned_wrong_ha_key_prevents_handshake() {
        let pairing_session_context = context(3);
        let mut initiator =
            Initiator::new(&TABLET_PRIVATE, &OTHER_HA_PUBLIC, &pairing_session_context)
                .expect("initiator should initialize with test fixtures");
        let mut responder = Responder::new(&HA_PRIVATE, &TABLET_PUBLIC, &pairing_session_context)
            .expect("responder should initialize with test fixtures");

        let request = initiator.start().expect("initiator should create request");
        assert_eq!(
            responder.respond(&request),
            Err(PairingError::HandshakeFailed)
        );
        assert_eq!(responder.seal_frame(b"blocked"), Err(PairingError::Closed));
    }

    #[test]
    fn malformed_handshake_response_lengths_close_the_initiator() {
        for malformed_length in [0, 47, 49] {
            let (mut initiator, mut responder) = endpoints(20);
            let request = initiator.start().expect("initiator should create request");
            let response = responder
                .respond(&request)
                .expect("responder should authenticate tablet");
            let mut malformed_response = response;
            malformed_response.resize(malformed_length, 0);

            assert_eq!(
                initiator.finish(&malformed_response),
                Err(PairingError::HandshakeFailed)
            );
            assert_eq!(initiator.seal_frame(b"blocked"), Err(PairingError::Closed));
        }
    }

    #[test]
    fn responder_rejects_a_wrong_tablet_public_key_before_replying() {
        let pairing_session_context = context(4);
        let mut initiator = Initiator::new(&TABLET_PRIVATE, &HA_PUBLIC, &pairing_session_context)
            .expect("initiator should initialize with test fixtures");
        let mut responder =
            Responder::new(&HA_PRIVATE, &OTHER_TABLET_PUBLIC, &pairing_session_context)
                .expect("responder should initialize with test fixtures");

        let request = initiator.start().expect("initiator should create request");
        assert_eq!(responder.respond(&request), Err(PairingError::PeerMismatch));
        assert!(!responder.is_established());
        assert_eq!(responder.seal_frame(b"blocked"), Err(PairingError::Closed));
    }

    #[test]
    fn repeated_responder_handshake_call_preserves_established_state() {
        let (mut initiator, mut responder) = endpoints(21);
        let request = initiator.start().expect("initiator should create request");
        let response = responder
            .respond(&request)
            .expect("responder should authenticate tablet");
        initiator
            .finish(&response)
            .expect("initiator should authenticate Home Assistant");

        assert_eq!(responder.respond(&request), Err(PairingError::WrongPhase));
        let frame = responder
            .seal_frame(b"still established")
            .expect("phase rejection should preserve the responder session");
        assert_eq!(
            initiator
                .open_frame(&frame)
                .expect("initiator should authenticate the response frame"),
            b"still established"
        );
    }

    #[test]
    fn mismatched_prologue_context_fails_the_handshake() {
        let mut initiator = Initiator::new(&TABLET_PRIVATE, &HA_PUBLIC, &context(5))
            .expect("initiator should initialize with test fixtures");
        let mut responder = Responder::new(&HA_PRIVATE, &TABLET_PUBLIC, &context(6))
            .expect("responder should initialize with test fixtures");

        let request = initiator.start().expect("initiator should create request");
        assert_eq!(
            responder.respond(&request),
            Err(PairingError::HandshakeFailed)
        );
        assert!(!responder.is_established());
    }

    #[test]
    fn tampered_handshake_request_closes_the_responder() {
        let (mut initiator, mut responder) = endpoints(8);
        let mut request = initiator.start().expect("initiator should create request");
        request[0] ^= 1;

        assert_eq!(
            responder.respond(&request),
            Err(PairingError::HandshakeFailed)
        );
        assert_eq!(responder.seal_frame(b"blocked"), Err(PairingError::Closed));
    }

    #[test]
    fn tampered_handshake_response_closes_the_initiator() {
        let (mut initiator, mut responder) = endpoints(9);
        let request = initiator.start().expect("initiator should create request");
        let mut response = responder
            .respond(&request)
            .expect("responder should authenticate tablet");
        response[0] ^= 1;

        assert_eq!(
            initiator.finish(&response),
            Err(PairingError::HandshakeFailed)
        );
        assert_eq!(initiator.seal_frame(b"blocked"), Err(PairingError::Closed));
    }

    #[test]
    fn reconnect_uses_saved_static_keys_with_a_new_session_context() {
        let (mut first_initiator, mut first_responder) = established_endpoints(11);
        let first_frame = first_initiator
            .seal_frame(b"first session")
            .expect("first session should seal a frame");
        assert_eq!(
            first_responder
                .open_frame(&first_frame)
                .expect("first session frame should open"),
            b"first session"
        );

        let (mut second_initiator, mut second_responder) = established_endpoints(12);
        let second_frame = second_initiator
            .seal_frame(b"second session")
            .expect("second session should seal a frame");
        assert_eq!(
            second_responder
                .open_frame(&second_frame)
                .expect("second session frame should open"),
            b"second session"
        );
    }

    #[test]
    fn corrupted_frame_closes_the_receiving_session() {
        let (mut initiator, mut responder) = established_endpoints(13);
        let mut ciphertext = initiator
            .seal_frame(b"configuration")
            .expect("initiator should seal a frame");
        let last_byte = ciphertext
            .last_mut()
            .expect("encrypted empty or non-empty messages include an authentication tag");
        *last_byte ^= 1;

        assert_eq!(
            responder.open_frame(&ciphertext),
            Err(PairingError::FrameAuthenticationFailed)
        );
        assert_eq!(
            responder.open_frame(b"a valid later frame"),
            Err(PairingError::Closed)
        );
    }

    #[test]
    fn replayed_frame_closes_the_receiving_session() {
        let (mut initiator, mut responder) = established_endpoints(14);
        let ciphertext = initiator
            .seal_frame(b"configuration")
            .expect("initiator should seal a frame");
        assert_eq!(
            responder
                .open_frame(&ciphertext)
                .expect("first frame should authenticate"),
            b"configuration"
        );
        assert_eq!(
            responder.open_frame(&ciphertext),
            Err(PairingError::FrameAuthenticationFailed)
        );
        assert_eq!(responder.seal_frame(b"response"), Err(PairingError::Closed));
    }

    #[test]
    fn truncated_ciphertext_closes_the_receiving_session() {
        let (mut initiator, mut responder) = established_endpoints(18);
        let ciphertext = initiator
            .seal_frame(b"configuration")
            .expect("initiator should seal a frame");

        assert_eq!(
            responder.open_frame(&ciphertext[..ciphertext.len() - 1]),
            Err(PairingError::FrameAuthenticationFailed)
        );
        assert_eq!(responder.seal_frame(b"blocked"), Err(PairingError::Closed));
    }

    #[test]
    fn replay_from_another_connection_with_same_static_pins_fails() {
        let (mut first_initiator, _first_responder) = established_endpoints(19);
        let (mut second_initiator, mut second_responder) = established_endpoints(19);
        let first_session_frame = first_initiator
            .seal_frame(b"first connection")
            .expect("first session should seal a frame");
        let second_session_frame = second_initiator
            .seal_frame(b"second connection")
            .expect("second session should seal a frame");
        assert_ne!(first_session_frame, second_session_frame);
        assert_eq!(
            second_responder.open_frame(&first_session_frame),
            Err(PairingError::FrameAuthenticationFailed)
        );
    }

    #[test]
    fn frame_size_boundaries_are_enforced_without_consuming_a_nonce() {
        let (mut initiator, mut responder) = established_endpoints(15);
        let oversized_plaintext = vec![0_u8; MAX_FRAME_PLAINTEXT + 1];
        assert_eq!(
            initiator.seal_frame(&oversized_plaintext),
            Err(PairingError::FrameTooLarge)
        );

        let empty_frame = initiator
            .seal_frame(&[])
            .expect("empty plaintext is a valid frame");
        assert_eq!(
            responder
                .open_frame(&empty_frame)
                .expect("empty frame should authenticate"),
            b""
        );

        let maximum_plaintext = vec![42_u8; MAX_FRAME_PLAINTEXT];
        let maximum_frame = initiator
            .seal_frame(&maximum_plaintext)
            .expect("maximum plaintext should fit");
        let mut oversized_ciphertext = maximum_frame.clone();
        oversized_ciphertext.push(0);
        assert_eq!(
            responder.open_frame(&oversized_ciphertext),
            Err(PairingError::FrameTooLarge)
        );
        assert_eq!(
            responder
                .open_frame(&maximum_frame)
                .expect("maximum frame should authenticate after size rejection"),
            maximum_plaintext
        );
    }

    #[test]
    fn wrong_handshake_phase_does_not_enable_transport() {
        let (mut initiator, mut responder) = endpoints(16);
        assert_eq!(initiator.finish(&[]), Err(PairingError::WrongPhase));
        assert_eq!(responder.respond(&[]), Err(PairingError::HandshakeFailed));
        assert_eq!(responder.seal_frame(b"blocked"), Err(PairingError::Closed));

        let (mut initiator, _responder) = endpoints(17);
        let request = initiator.start().expect("initiator should create request");
        assert_eq!(initiator.start(), Err(PairingError::WrongPhase));
        assert_eq!(
            initiator.seal_frame(&request),
            Err(PairingError::WrongPhase)
        );
    }

    #[test]
    fn pairing_errors_have_fixed_non_secret_display_messages() {
        let expected_messages = [
            (
                PairingError::CryptoUnavailable,
                "Noise cryptography is unavailable",
            ),
            (
                PairingError::WrongPhase,
                "operation is invalid in the current handshake phase",
            ),
            (
                PairingError::PeerMismatch,
                "peer static key does not match the expected key",
            ),
            (PairingError::HandshakeFailed, "Noise handshake failed"),
            (
                PairingError::FrameTooLarge,
                "transport frame exceeds the maximum size",
            ),
            (
                PairingError::FrameAuthenticationFailed,
                "transport frame authentication failed",
            ),
            (
                PairingError::FrameProtectionFailed,
                "transport frame encryption failed",
            ),
            (PairingError::Closed, "pairing session is closed"),
        ];

        for (error, expected_message) in expected_messages {
            assert_eq!(error.to_string(), expected_message);
        }
    }
}
