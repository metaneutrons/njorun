//! Persistent setup state: Wi-Fi credentials, the Home Assistant session and
//! the language. Values live in plaintext NVS because flash encryption and
//! secure boot are deliberately not enabled; see docs/protocols/003-device-persistence.md.

use njorun_app_core::problems::{
    valid_base_url, valid_client_id, valid_refresh_token, SetupProblem,
};

use crate::board;

const WIFI_SSID: &str = "wifi_ssid";
const WIFI_PASSWORD: &str = "wifi_pass";
const HA_URL: &str = "ha_url";
const HA_CLIENT_ID: &str = "ha_client";
const HA_REFRESH_TOKEN: &str = "ha_refresh";
const LANGUAGE: &str = "lang";
const ORIENTATION: &str = "orient";
const THEME: &str = "theme";

#[derive(Clone)]
pub(crate) struct WifiCredentials {
    pub ssid: String,
    pub password: String,
}

#[derive(Clone)]
pub(crate) struct HomeAssistantSession {
    pub url: String,
    pub client_id: String,
    pub refresh_token: String,
}

#[derive(Default)]
pub(crate) struct Saved {
    pub wifi: Option<WifiCredentials>,
    pub home_assistant: Option<HomeAssistantSession>,
    pub german: Option<bool>,
    /// Why the saved setup cannot be used, if it cannot.
    pub problem: Option<SetupProblem>,
}

/// Reads the saved setup and checks it, so damaged storage or values are reported instead of
/// failing later as an unexplained connection error.
pub(crate) fn load() -> Saved {
    let mut unreadable = false;
    let mut read = |key: &str| {
        board::store_get(key)
            .map_err(|code| {
                log::warn!(target: "store", "saved {key} could not be read: error {code}");
                unreadable = true;
            })
            .ok()
            .flatten()
            .filter(|value| !value.is_empty())
    };
    let ssid = read(WIFI_SSID);
    let password = read(WIFI_PASSWORD);
    let url = read(HA_URL);
    let client_id = read(HA_CLIENT_ID);
    let refresh_token = read(HA_REFRESH_TOKEN);
    let language = read(LANGUAGE);
    let german = language.as_deref().map(|value| value == "de");

    let wifi_valid = ssid
        .as_ref()
        .is_some_and(|ssid| ssid.len() <= 32 && !ssid.contains('\0'))
        && password
            .as_ref()
            .is_none_or(|password| password.len() <= 64);
    let ha_present = [&url, &client_id, &refresh_token]
        .iter()
        .filter(|value| value.is_some())
        .count();
    let ha_valid = ha_present == 3
        && url.as_deref().is_some_and(valid_base_url)
        && client_id.as_deref().is_some_and(valid_client_id)
        && refresh_token.as_deref().is_some_and(valid_refresh_token);

    let problem = if unreadable {
        Some(SetupProblem::StorageUnreadable)
    } else if (ssid.is_some() && !wifi_valid) || (ha_present == 3 && !ha_valid) {
        Some(SetupProblem::Invalid)
    } else if (ha_present > 0 && ha_present < 3) || (ha_present == 3 && ssid.is_none()) {
        Some(SetupProblem::Incomplete)
    } else {
        None
    };
    if let Some(problem) = problem {
        log::warn!(target: "store", "saved setup is unusable: {problem:?}");
    }
    Saved {
        wifi: (problem.is_none() && wifi_valid).then(|| WifiCredentials {
            ssid: ssid.unwrap_or_default(),
            password: password.unwrap_or_default(),
        }),
        home_assistant: (problem.is_none() && ha_valid).then(|| HomeAssistantSession {
            url: url.unwrap_or_default(),
            client_id: client_id.unwrap_or_default(),
            refresh_token: refresh_token.unwrap_or_default(),
        }),
        german,
        problem,
    }
}

fn set(key: &str, value: &str) {
    if let Err(code) = board::store_set(key, value) {
        log::warn!(target: "store", "could not save {key}: error {code}");
    }
}

pub(crate) fn save_wifi(ssid: &str, password: &str) {
    set(WIFI_SSID, ssid);
    set(WIFI_PASSWORD, password);
}

pub(crate) fn save_home_assistant(session: &HomeAssistantSession) {
    set(HA_URL, &session.url);
    set(HA_CLIENT_ID, &session.client_id);
    set(HA_REFRESH_TOKEN, &session.refresh_token);
}

pub(crate) fn clear_home_assistant() {
    for key in [HA_URL, HA_CLIENT_ID, HA_REFRESH_TOKEN] {
        set(key, "");
    }
}

/// The panel orientation key (see `Orientation::key`); `None` when never chosen.
pub(crate) fn orientation() -> Option<String> {
    board::store_get(ORIENTATION).ok().flatten()
}

pub(crate) fn save_orientation(key: &str) {
    set(ORIENTATION, key);
}

/// `light`, `dark` or `automatic`; `None` when never chosen.
pub(crate) fn theme() -> Option<String> {
    board::store_get(THEME).ok().flatten()
}

pub(crate) fn save_theme(theme: &str) {
    set(THEME, theme);
}

pub(crate) fn save_language(german: bool) {
    set(LANGUAGE, if german { "de" } else { "en" });
}

pub(crate) fn forget_all() {
    if let Err(code) = board::store_erase() {
        log::warn!(target: "store", "could not erase saved setup: error {code}");
    }
}
