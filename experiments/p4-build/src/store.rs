//! Persistent setup state: Wi-Fi credentials, the Home Assistant session, the language and
//! the tablet's management identity. Values live in plaintext NVS because flash encryption
//! and secure boot are deliberately not enabled; see docs/device-persistence.md.

use njorun_app_core::identity::{Stored, TabletIdentity, RANDOM_BYTES};
use njorun_app_core::network::{IpConfig, Mode};
use njorun_app_core::ota::PendingInstall;
use njorun_app_core::problems::{
    valid_base_url, valid_client_id, valid_refresh_token, SetupProblem,
};
use njorun_app_core::settings::AppliedSettings;

use crate::board;

const WIFI_SSID: &str = "wifi_ssid";
const WIFI_PASSWORD: &str = "wifi_pass";
/// The IPv4 setup of the Wi-Fi as JSON (plan 006); absent means DHCP.
const WIFI_IP: &str = "wifi_ip";
const HA_URL: &str = "ha_url";
const HA_CLIENT_ID: &str = "ha_client";
const HA_REFRESH_TOKEN: &str = "ha_refresh";
const LANGUAGE: &str = "lang";
const ORIENTATION: &str = "orient";
const THEME: &str = "theme";
const DAYLIGHT: &str = "daylight";
const SETTINGS: &str = "settings";
const LOG_SINK: &str = "log_sink";
const TABLET_ID: &str = "tablet_id";
const TABLET_KEY: &str = "tablet_key";
const MGMT_BINDING: &str = "mgmt_binding";
const MGMT_HA_KEY: &str = "mgmt_ha_key";
const OTA_PENDING: &str = "ota_pending";

#[derive(Clone)]
pub(crate) struct WifiCredentials {
    pub ssid: String,
    pub password: String,
    pub ip: IpConfig,
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
    let wifi_ip = read(WIFI_IP);
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
            ip: saved_ip(wifi_ip.as_deref()),
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

/// The saved IP setup. A value that does not parse or does not validate is not trusted: a tablet
/// with a damaged static setup must still be able to join a network, so it falls back to DHCP.
fn saved_ip(text: Option<&str>) -> IpConfig {
    let Some(text) = text else {
        return IpConfig::dhcp();
    };
    match serde_json::from_str::<IpConfig>(text) {
        Ok(config) if config.validate().is_ok() => config,
        _ => {
            log::warn!(target: "store", "the saved IP setup is not valid; using DHCP");
            IpConfig::dhcp()
        }
    }
}

pub(crate) fn save_wifi(ssid: &str, password: &str, ip: &IpConfig) {
    set(WIFI_SSID, ssid);
    set(WIFI_PASSWORD, password);
    // DHCP is the absence of the key, written as an empty value.
    let text = if ip.mode == Mode::Dhcp {
        String::new()
    } else {
        serde_json::to_string(ip).unwrap_or_default()
    };
    set(WIFI_IP, &text);
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

/// Whether the sun was up when Home Assistant last said so (`up`/`down`), so the automatic
/// theme is right from the first screen after a restart.
pub(crate) fn daylight() -> Option<bool> {
    match board::store_get(DAYLIGHT).ok().flatten().as_deref() {
        Some("up") => Some(true),
        Some("down") => Some(false),
        _ => None,
    }
}

pub(crate) fn save_daylight(up: bool) {
    if daylight() != Some(up) {
        set(DAYLIGHT, if up { "up" } else { "down" });
    }
}

/// The settings Home Assistant last sent and the tablet applied, with their revision.
pub(crate) fn applied_settings() -> Option<AppliedSettings> {
    let text = board::store_get(SETTINGS).ok().flatten()?;
    serde_json::from_str(&text).ok()
}

/// Stores the applied settings; `false` when they could not be written, in which case the
/// caller must not acknowledge them.
pub(crate) fn save_applied_settings(applied: &AppliedSettings) -> bool {
    let Ok(text) = serde_json::to_string(applied) else {
        return false;
    };
    if let Err(code) = board::store_set(SETTINGS, &text) {
        log::error!(target: "store", "could not save the applied settings: error {code}");
        return false;
    }
    board::store_get(SETTINGS).ok().flatten().as_deref() == Some(text.as_str())
}

/// The remote logging sink Home Assistant last sent: empty for off, `None` when never sent.
pub(crate) fn log_sink() -> Option<String> {
    board::store_get(LOG_SINK).ok().flatten()
}

/// Keeps the sink across restarts, so logging works before Home Assistant answers.
pub(crate) fn save_log_sink(sink: &str) {
    if log_sink().as_deref() != Some(sink) {
        set(LOG_SINK, sink);
    }
}

pub(crate) fn save_language(german: bool) {
    set(LANGUAGE, if german { "de" } else { "en" });
}

/// The tablet's management identity, created on first use. `None` when storage cannot be read
/// or written: an identity that is not saved would change on the next boot, so it is not used.
pub(crate) fn identity() -> Option<TabletIdentity> {
    let read = |key: &str| {
        board::store_get(key).map_err(|code| {
            log::error!(target: "store", "saved {key} could not be read: error {code}");
        })
    };
    let (tablet_id, key) = (read(TABLET_ID).ok()?, read(TABLET_KEY).ok()?);
    match TabletIdentity::classify(tablet_id.as_deref(), key.as_deref()) {
        Stored::Valid(identity) => return Some(identity),
        Stored::Missing => log::info!(target: "store", "creating the tablet identity"),
        Stored::Damaged => log::warn!(
            target: "store",
            "saved tablet identity is damaged; creating a new one (pair the tablet again)"
        ),
    }
    let mut random = [0_u8; RANDOM_BYTES];
    if let Err(code) = board::fill_random(&mut random) {
        log::error!(target: "store", "random number generator failed: error {code}");
        return None;
    }
    if !TabletIdentity::random_is_usable(&random) {
        log::error!(target: "store", "random number generator returned no entropy");
        return None;
    }
    let identity = TabletIdentity::from_random(&random);
    random.fill(0);
    let saved = board::store_set(TABLET_KEY, &identity.encoded_key())
        .and_then(|()| board::store_set(TABLET_ID, identity.tablet_id()));
    if let Err(code) = saved {
        log::error!(target: "store", "could not save the tablet identity: error {code}");
        return None;
    }
    let read_back = (read(TABLET_ID).ok()?, read(TABLET_KEY).ok()?);
    match TabletIdentity::classify(read_back.0.as_deref(), read_back.1.as_deref()) {
        Stored::Valid(stored) if stored == identity => Some(identity),
        _ => {
            log::error!(target: "store", "the saved tablet identity does not read back");
            None
        }
    }
}

/// The approved binding with the Njörun integration and the HA key it pins.
pub(crate) fn binding() -> Option<(String, [u8; 32])> {
    let binding = board::store_get(MGMT_BINDING).ok().flatten()?;
    let key = board::store_get(MGMT_HA_KEY).ok().flatten()?;
    let key = njorun_management::decode_key(&key).ok()?;
    (!binding.is_empty()).then_some((binding, key))
}

pub(crate) fn save_binding(binding_id: &str, ha_public: &[u8; 32]) {
    set(MGMT_HA_KEY, &njorun_management::encode_key(ha_public));
    set(MGMT_BINDING, binding_id);
}

pub(crate) fn forget_binding() {
    set(MGMT_BINDING, "");
    set(MGMT_HA_KEY, "");
}

pub(crate) fn forget_all() {
    if let Err(code) = board::store_erase() {
        log::warn!(target: "store", "could not erase saved setup: error {code}");
    }
}

/// The install that switched the boot slot and still has to report its outcome.
pub(crate) fn pending_install() -> Option<PendingInstall> {
    let text = board::store_get(OTA_PENDING).ok().flatten()?;
    let pending = PendingInstall::decode(&text);
    if pending.is_none() && !text.is_empty() {
        log::warn!(target: "store", "the stored install record is damaged; dropped");
        clear_pending_install();
    }
    pending
}

/// Stores the record before the slot is switched. `false` when it could not be written.
pub(crate) fn save_pending_install(pending: &PendingInstall) -> bool {
    board::store_set(OTA_PENDING, &pending.encode())
        .map_err(
            |code| log::warn!(target: "store", "could not save the install record: error {code}"),
        )
        .is_ok()
}

pub(crate) fn clear_pending_install() {
    set(OTA_PENDING, "");
}
