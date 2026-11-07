//! Persistent setup state: Wi-Fi credentials, the Home Assistant session and
//! the language. Values live in plaintext NVS because flash encryption and
//! secure boot are deliberately not enabled; see docs/protocols/003-device-persistence.md.

use crate::board;

const WIFI_SSID: &str = "wifi_ssid";
const WIFI_PASSWORD: &str = "wifi_pass";
const HA_URL: &str = "ha_url";
const HA_CLIENT_ID: &str = "ha_client";
const HA_REFRESH_TOKEN: &str = "ha_refresh";
const LANGUAGE: &str = "lang";

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
}

pub(crate) fn load() -> Saved {
    let wifi = board::store_get(WIFI_SSID)
        .filter(|ssid| !ssid.is_empty())
        .map(|ssid| WifiCredentials {
            ssid,
            password: board::store_get(WIFI_PASSWORD).unwrap_or_default(),
        });
    let home_assistant = match (
        board::store_get(HA_URL),
        board::store_get(HA_CLIENT_ID),
        board::store_get(HA_REFRESH_TOKEN),
    ) {
        (Some(url), Some(client_id), Some(refresh_token))
            if !url.is_empty() && !client_id.is_empty() && !refresh_token.is_empty() =>
        {
            Some(HomeAssistantSession {
                url,
                client_id,
                refresh_token,
            })
        }
        _ => None,
    };
    let german = board::store_get(LANGUAGE).map(|value| value == "de");
    Saved {
        wifi,
        home_assistant,
        german,
    }
}

fn set(key: &str, value: &str) {
    if let Err(code) = board::store_set(key, value) {
        println!("Njörun D1001 could not save {key}: error {code}");
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

pub(crate) fn save_language(german: bool) {
    set(LANGUAGE, if german { "de" } else { "en" });
}

pub(crate) fn forget_all() {
    if let Err(code) = board::store_erase() {
        println!("Njörun D1001 could not erase saved setup: error {code}");
    }
}
