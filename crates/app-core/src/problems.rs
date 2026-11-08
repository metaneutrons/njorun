//! Why the panel cannot reach Home Assistant or use its saved setup, with the text shown to
//! the user. The texts name the likely cause so a network fault is not mistaken for a broken
//! panel.

/// Why a connection to Home Assistant failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionProblem {
    /// The host name did not resolve.
    NameNotFound,
    /// No answer within the timeout: usually a firewall or routing rule.
    Timeout,
    /// The host answered but nothing listens on the port.
    Refused,
    /// No route to the host or network.
    Unreachable,
    /// The TLS handshake or the certificate check failed.
    Certificate,
    /// The panel clock is not set yet, so certificates cannot be checked.
    ClockNotSet,
    /// Home Assistant refused the saved sign-in.
    SignInRejected,
    /// Home Assistant answered, but not as expected.
    BadResponse,
    /// Anything else.
    Other,
}

impl ConnectionProblem {
    /// Maps a firmware error code (see `njorun_d1001.h`) to a problem.
    #[must_use]
    pub const fn from_code(code: i32) -> Self {
        match code {
            0x6E01 => Self::NameNotFound,
            0x6E02 => Self::Timeout,
            0x6E03 => Self::Refused,
            0x6E04 => Self::Unreachable,
            0x6E05 => Self::Certificate,
            0x103 => Self::ClockNotSet,
            0x10D => Self::SignInRejected,
            0x108 => Self::BadResponse,
            _ => Self::Other,
        }
    }

    /// One sentence for the user, naming `host`.
    #[must_use]
    pub fn message(self, host: &str, german: bool) -> String {
        match (self, german) {
            (Self::NameNotFound, false) => {
                format!("{host} cannot be found. Check the address and the network's DNS.")
            }
            (Self::NameNotFound, true) => {
                format!("{host} wurde nicht gefunden. Prüfe die Adresse und das DNS des Netzwerks.")
            }
            (Self::Timeout, false) => format!(
                "{host} does not answer. The panel's network may be blocked from reaching it (firewall or routing)."
            ),
            (Self::Timeout, true) => format!(
                "{host} antwortet nicht. Vermutlich blockiert eine Firewall oder Routing-Regel den Weg aus dem Netz des Panels."
            ),
            (Self::Refused, false) => {
                format!("{host} refused the connection. Check that Home Assistant runs on this port.")
            }
            (Self::Refused, true) => format!(
                "{host} lehnt die Verbindung ab. Prüfe, ob Home Assistant auf diesem Port läuft."
            ),
            (Self::Unreachable, false) => {
                format!("There is no route from the panel's network to {host}.")
            }
            (Self::Unreachable, true) => {
                format!("Aus dem Netz des Panels führt kein Weg zu {host}.")
            }
            (Self::Certificate, false) => {
                format!("The secure connection to {host} failed. Check its certificate.")
            }
            (Self::Certificate, true) => format!(
                "Die sichere Verbindung zu {host} ist fehlgeschlagen. Prüfe das Zertifikat."
            ),
            (Self::ClockNotSet, false) => {
                "The panel's clock is not set yet (NTP), so it cannot check certificates.".to_owned()
            }
            (Self::ClockNotSet, true) => {
                "Die Uhr des Panels ist noch nicht gestellt (NTP), daher können keine Zertifikate geprüft werden.".to_owned()
            }
            (Self::SignInRejected, false) => {
                format!("{host} no longer accepts this panel's sign-in. Sign in again.")
            }
            (Self::SignInRejected, true) => {
                format!("{host} akzeptiert die Anmeldung dieses Panels nicht mehr. Melde dich erneut an.")
            }
            (Self::BadResponse, false) => {
                format!("{host} answered unexpectedly. Is this the Home Assistant address?")
            }
            (Self::BadResponse, true) => {
                format!("{host} antwortet unerwartet. Ist das die Adresse von Home Assistant?")
            }
            (Self::Other, false) => format!("{host} cannot be reached."),
            (Self::Other, true) => format!("{host} ist nicht erreichbar."),
        }
    }
}

/// Why the saved setup cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupProblem {
    /// The settings storage could not be read.
    StorageUnreadable,
    /// Values are present but not valid, for example a malformed address.
    Invalid,
    /// Only part of the setup is saved.
    Incomplete,
}

impl SetupProblem {
    #[must_use]
    pub const fn message(self, german: bool) -> &'static str {
        match (self, german) {
            (Self::StorageUnreadable, false) => {
                "The saved setup could not be read. Set up the panel again."
            }
            (Self::StorageUnreadable, true) => {
                "Die gespeicherte Einrichtung konnte nicht gelesen werden. Richte das Panel neu ein."
            }
            (Self::Invalid, false) => "The saved setup is damaged. Set up the panel again.",
            (Self::Invalid, true) => {
                "Die gespeicherte Einrichtung ist beschädigt. Richte das Panel neu ein."
            }
            (Self::Incomplete, false) => {
                "The saved setup is incomplete. Finish setting up the panel."
            }
            (Self::Incomplete, true) => {
                "Die gespeicherte Einrichtung ist unvollständig. Schließe die Einrichtung ab."
            }
        }
    }
}

/// Checks a saved Home Assistant base URL: http or https, a host, nothing after the path.
#[must_use]
pub fn valid_base_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !host.is_empty()
        && !host.contains('@')
        && host.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'[' | b']')
        })
        && !rest.contains(['?', '#', ' '])
        && url.len() <= 512
}

/// Checks a saved OAuth client id, which this panel creates as `http://<ipv4>:<port>/client`.
#[must_use]
pub fn valid_client_id(client_id: &str) -> bool {
    client_id
        .strip_prefix("http://")
        .and_then(|rest| rest.strip_suffix("/client"))
        .is_some_and(|authority| {
            !authority.is_empty()
                && authority
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b':'))
        })
}

/// Checks a saved refresh token: printable ASCII of a plausible length.
#[must_use]
pub fn valid_refresh_token(token: &str) -> bool {
    (16..=1024).contains(&token.len()) && token.bytes().all(|byte| byte.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::{
        ConnectionProblem, SetupProblem, valid_base_url, valid_client_id, valid_refresh_token,
    };

    #[test]
    fn codes_map_to_problems() {
        assert_eq!(
            ConnectionProblem::from_code(0x6E02),
            ConnectionProblem::Timeout
        );
        assert_eq!(
            ConnectionProblem::from_code(0x10D),
            ConnectionProblem::SignInRejected
        );
        assert_eq!(ConnectionProblem::from_code(-1), ConnectionProblem::Other);
    }

    #[test]
    fn messages_name_the_host_and_the_cause() {
        let text = ConnectionProblem::Timeout.message("home.example", true);
        assert!(text.starts_with("home.example antwortet nicht"));
        assert!(text.contains("Firewall"));
        assert!(
            ConnectionProblem::NameNotFound
                .message("x", false)
                .contains("cannot be found")
        );
        for problem in [
            SetupProblem::StorageUnreadable,
            SetupProblem::Invalid,
            SetupProblem::Incomplete,
        ] {
            assert_ne!(problem.message(true), problem.message(false));
        }
    }

    #[test]
    fn every_code_and_problem_has_a_message_in_both_languages() {
        let codes = [
            (0x6E01, ConnectionProblem::NameNotFound),
            (0x6E02, ConnectionProblem::Timeout),
            (0x6E03, ConnectionProblem::Refused),
            (0x6E04, ConnectionProblem::Unreachable),
            (0x6E05, ConnectionProblem::Certificate),
            (0x103, ConnectionProblem::ClockNotSet),
            (0x10D, ConnectionProblem::SignInRejected),
            (0x108, ConnectionProblem::BadResponse),
            (0x101, ConnectionProblem::Other),
        ];
        let mut seen = Vec::new();
        for (code, problem) in codes {
            assert_eq!(ConnectionProblem::from_code(code), problem);
            let english = problem.message("ha.test", false);
            let german = problem.message("ha.test", true);
            assert_ne!(english, german, "{problem:?}");
            if problem != ConnectionProblem::ClockNotSet {
                assert!(english.contains("ha.test"), "{problem:?}");
                assert!(german.contains("ha.test"), "{problem:?}");
            }
            assert!(!seen.contains(&english), "{problem:?} repeats a message");
            seen.push(english);
        }
    }

    #[test]
    fn saved_values_are_validated() {
        assert!(valid_base_url("https://home.schmieder.eu"));
        assert!(valid_base_url("http://192.168.2.5:8123"));
        assert!(valid_base_url("https://ha.example/base"));
        assert!(!valid_base_url("ftp://home"));
        assert!(!valid_base_url("https://"));
        assert!(!valid_base_url("https://user@home"));
        assert!(!valid_base_url("https://home?x=1"));
        assert!(!valid_base_url("https://ho\u{fffd}me"));
        assert!(valid_client_id("http://192.168.17.18:62992/client"));
        assert!(!valid_client_id("http://192.168.17.18:62992/"));
        assert!(!valid_client_id("https://evil.example/client"));
        assert!(valid_refresh_token(&"a".repeat(128)));
        assert!(!valid_refresh_token("short"));
        assert!(!valid_refresh_token(&format!("{}\u{0}", "a".repeat(40))));
    }
}
