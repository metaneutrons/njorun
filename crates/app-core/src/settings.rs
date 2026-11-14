//! Versioned desired settings. HA is the only preference writer.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{DashboardOrder, RoomId};

/// Orientations the tablet can show, as named by `Orientation::key`.
pub const ORIENTATIONS: [&str; 4] = [
    "portrait",
    "landscape",
    "portrait_flipped",
    "landscape_flipped",
];

/// The smallest, the largest and the default size of the interface, in percent.
pub const MIN_UI_SCALE: u8 = 75;
pub const MAX_UI_SCALE: u8 = 200;
pub const DEFAULT_UI_SCALE: u8 = 100;
pub const DEFAULT_CAMERA_MOTION_SENSITIVITY: u8 = 80;

const MAX_ENTITY_NAME_ENTRIES: usize = 256;
const MAX_ROOM_ALIAS_ENTRIES: usize = crate::MAX_AREA_COUNT;

const fn default_ui_scale() -> u8 {
    DEFAULT_UI_SCALE
}

const fn default_camera_motion_enabled() -> bool {
    true
}

const fn default_camera_motion_sensitivity() -> u8 {
    DEFAULT_CAMERA_MOTION_SENSITIVITY
}

fn default_color_scheme() -> String {
    "njorun".into()
}

fn valid_label(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= crate::MAX_NAME_CHARS
        && !value.chars().any(char::is_control)
}

fn valid_name_map(map: &BTreeMap<String, String>, maximum: usize) -> bool {
    map.len() <= maximum
        && map
            .iter()
            .all(|(key, value)| valid_id(key) && valid_label(value))
}

fn valid_icon_map(map: &BTreeMap<String, String>) -> bool {
    map.len() <= MAX_ROOM_ALIAS_ENTRIES
        && map.iter().all(|(key, value)| {
            valid_id(key)
                && [
                    "home", "sofa", "kitchen", "bed", "desk", "bath", "garage", "garden",
                ]
                .contains(&value.as_str())
        })
}

/// Settings exchanged with the HA integration; transport secrets are separate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Settings {
    pub orientation: String,
    pub brightness: u8,
    pub theme: String,
    /// The curated paired light/dark palette selected in Home Assistant.
    pub color_scheme: String,
    /// Per-mode palettes. Legacy settings are normalized to their paired palette on load.
    pub light_color_scheme: String,
    pub dark_color_scheme: String,
    pub increased_contrast: bool,
    pub reduced_motion: bool,
    /// Whether the camera may power up while the display is idle or manually off.
    pub camera_motion_enabled: bool,
    /// Camera motion sensitivity from 1 (least sensitive) through 100 (most sensitive).
    pub camera_motion_sensitivity: u8,
    pub entity_names: BTreeMap<String, String>,
    pub room_names: BTreeMap<String, String>,
    pub room_icons: BTreeMap<String, String>,
    pub language: String,
    pub idle_timeout: u32,
    pub idle_brightness: u8,
    /// Size of everything on the dashboard in percent, 75 to 200. A record stored before this
    /// setting existed has 100.
    pub ui_scale: u8,
    pub screensaver: String,
    pub home_view: String,
    pub details: String,
    pub update_policy: String,
    pub update_time: String,
    pub update_channel: String,
    pub favorites: Vec<String>,
    pub room_order: Vec<String>,
    pub entity_order: BTreeMap<String, Vec<String>>,
    pub excluded_entities: Vec<String>,
}

/// Tracks whether the wire carried a value without making `null` equivalent to omission.
#[derive(Debug, Default)]
enum Explicit<T> {
    #[default]
    Missing,
    Present(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Explicit<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Self::Present)
    }
}

/// Deserialize through a wire type so omitted legacy contrast can be distinguished from an
/// explicit `false`. Keeping this representation private preserves the public Settings shape.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsWire {
    orientation: String,
    brightness: u8,
    theme: String,
    #[serde(default = "default_color_scheme")]
    color_scheme: String,
    #[serde(default)]
    light_color_scheme: Explicit<String>,
    #[serde(default)]
    dark_color_scheme: Explicit<String>,
    #[serde(default)]
    increased_contrast: Explicit<bool>,
    #[serde(default)]
    reduced_motion: bool,
    #[serde(default = "default_camera_motion_enabled")]
    camera_motion_enabled: bool,
    #[serde(default = "default_camera_motion_sensitivity")]
    camera_motion_sensitivity: u8,
    #[serde(default)]
    entity_names: BTreeMap<String, String>,
    #[serde(default)]
    room_names: BTreeMap<String, String>,
    #[serde(default)]
    room_icons: BTreeMap<String, String>,
    language: String,
    idle_timeout: u32,
    idle_brightness: u8,
    #[serde(default = "default_ui_scale")]
    ui_scale: u8,
    screensaver: String,
    home_view: String,
    details: String,
    update_policy: String,
    update_time: String,
    update_channel: String,
    favorites: Vec<String>,
    room_order: Vec<String>,
    entity_order: BTreeMap<String, Vec<String>>,
    excluded_entities: Vec<String>,
}

impl<'de> Deserialize<'de> for Settings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = SettingsWire::deserialize(deserializer)?;
        let increased_contrast = match wire.increased_contrast {
            Explicit::Present(value) => value,
            Explicit::Missing => wire.color_scheme == "high_contrast",
        };
        let fallback_palette = if wire.color_scheme == "high_contrast" {
            "njorun"
        } else {
            &wire.color_scheme
        };
        let light_color_scheme = match wire.light_color_scheme {
            Explicit::Present(value) => value,
            Explicit::Missing => fallback_palette.to_owned(),
        };
        let dark_color_scheme = match wire.dark_color_scheme {
            Explicit::Present(value) => value,
            Explicit::Missing => fallback_palette.to_owned(),
        };

        Ok(Self {
            orientation: wire.orientation,
            brightness: wire.brightness,
            theme: wire.theme,
            color_scheme: wire.color_scheme,
            light_color_scheme,
            dark_color_scheme,
            increased_contrast,
            reduced_motion: wire.reduced_motion,
            camera_motion_enabled: wire.camera_motion_enabled,
            camera_motion_sensitivity: wire.camera_motion_sensitivity,
            entity_names: wire.entity_names,
            room_names: wire.room_names,
            room_icons: wire.room_icons,
            language: wire.language,
            idle_timeout: wire.idle_timeout,
            idle_brightness: wire.idle_brightness,
            ui_scale: wire.ui_scale,
            screensaver: wire.screensaver,
            home_view: wire.home_view,
            details: wire.details,
            update_policy: wire.update_policy,
            update_time: wire.update_time,
            update_channel: wire.update_channel,
            favorites: wire.favorites,
            room_order: wire.room_order,
            entity_order: wire.entity_order,
            excluded_entities: wire.excluded_entities,
        })
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            orientation: "portrait".into(),
            brightness: 80,
            theme: "dark".into(),
            color_scheme: default_color_scheme(),
            light_color_scheme: default_color_scheme(),
            dark_color_scheme: default_color_scheme(),
            increased_contrast: false,
            reduced_motion: false,
            camera_motion_enabled: true,
            camera_motion_sensitivity: DEFAULT_CAMERA_MOTION_SENSITIVITY,
            entity_names: BTreeMap::new(),
            room_names: BTreeMap::new(),
            room_icons: BTreeMap::new(),
            language: "en".into(),
            idle_timeout: 300,
            idle_brightness: 10,
            ui_scale: DEFAULT_UI_SCALE,
            screensaver: "display_off".into(),
            home_view: "mixed".into(),
            details: "adaptive".into(),
            update_policy: "automatic".into(),
            update_time: "02:00".into(),
            update_channel: "stable".into(),
            favorites: Vec::new(),
            room_order: Vec::new(),
            entity_order: BTreeMap::new(),
            excluded_entities: Vec::new(),
        }
    }
}

impl Settings {
    /// The palette used for this mode, with a fallback for manually constructed legacy values.
    #[must_use]
    pub fn effective_color_scheme(&self, dark: bool) -> &str {
        let selected = if dark {
            &self.dark_color_scheme
        } else {
            &self.light_color_scheme
        };
        if !selected.is_empty() {
            selected
        } else if self.color_scheme == "high_contrast" {
            "njorun"
        } else {
            &self.color_scheme
        }
    }

    /// Contrast preference after legacy high-contrast records have been normalized on load.
    #[must_use]
    pub const fn effective_increased_contrast(&self) -> bool {
        self.increased_contrast
    }

    /// Room icon by stable room ID, with a name-based suggestion for unconfigured rooms.
    #[must_use]
    pub fn room_icon(&self, room_id: &str, room_name: &str) -> &'static str {
        if let Some(icon) = self.room_icons.get(room_id) {
            return match icon.as_str() {
                "sofa" => "sofa",
                "kitchen" => "kitchen",
                "bed" => "bed",
                "desk" => "desk",
                "bath" => "bath",
                "garage" => "garage",
                "garden" => "garden",
                _ => "home",
            };
        }
        let alias = self.room_names.get(room_id).map_or("", String::as_str);
        let name = format!("{room_id} {room_name} {alias}").to_lowercase();
        if name.contains("kitchen") || name.contains("küche") || name.contains("kueche") {
            "kitchen"
        } else if name.contains("bedroom") || name.contains("bed room") || name.contains("schlaf") {
            "bed"
        } else if name.contains("bath") || name.contains("bad") {
            "bath"
        } else if name.contains("garage") {
            "garage"
        } else if name.contains("garden") || name.contains("garten") || name.contains("outdoor") {
            "garden"
        } else if name.contains("office")
            || name.contains("study")
            || name.contains("büro")
            || name.contains("buero")
            || name.contains("arbeitszimmer")
        {
            "desk"
        } else if name.contains("living") || name.contains("wohnzimmer") || name.contains("lounge")
        {
            "sofa"
        } else {
            "home"
        }
    }

    /// Display name for a room ID; absent aliases preserve Home Assistant's name.
    #[must_use]
    pub fn room_name<'a>(&'a self, room_id: &str, fallback: &'a str) -> &'a str {
        self.room_names
            .get(room_id)
            .map_or(fallback, String::as_str)
    }

    /// Display name for an entity; absent aliases preserve its current Home Assistant name.
    #[must_use]
    pub fn entity_name<'a>(&'a self, entity_id: &str, fallback: &'a str) -> &'a str {
        self.entity_names
            .get(entity_id)
            .map_or(fallback, String::as_str)
    }

    /// Room and tile order for the dashboard; `unassigned` names the unassigned room.
    #[must_use]
    pub fn dashboard_order(&self) -> DashboardOrder {
        DashboardOrder {
            area_ids: Some(self.room_order.clone()),
            entity_ids_by_room: self
                .entity_order
                .iter()
                .map(|(room, values)| {
                    (
                        if room == "unassigned" {
                            RoomId::Unassigned
                        } else {
                            RoomId::Area(room.clone())
                        },
                        values.clone(),
                    )
                })
                .collect(),
        }
    }

    /// Whether the owner excluded this entity from the panel.
    #[must_use]
    pub fn excludes(&self, entity_id: &str) -> bool {
        self.excluded_entities.iter().any(|id| id == entity_id)
    }

    /// Whether `other` changes what the dashboard lists: rooms, tiles, their order or
    /// exclusions. Language, theme, brightness and the like do not.
    #[must_use]
    pub fn layout_differs(&self, other: &Self) -> bool {
        self.room_order != other.room_order
            || self.entity_order != other.entity_order
            || self.excluded_entities != other.excluded_entities
    }

    /// Validate limits independently of HA, before persistence or UI application.
    #[must_use]
    pub fn is_valid(&self, orientations: &[&str]) -> bool {
        orientations.contains(&self.orientation.as_str())
            && ORIENTATIONS.contains(&self.orientation.as_str())
            && (1..=100).contains(&self.brightness)
            && self.idle_brightness <= 100
            && (1..=100).contains(&self.camera_motion_sensitivity)
            && (MIN_UI_SCALE..=MAX_UI_SCALE).contains(&self.ui_scale)
            && (10..=86400).contains(&self.idle_timeout)
            && ["automatic", "light", "dark"].contains(&self.theme.as_str())
            && ["njorun", "graphite", "warm", "high_contrast", "porcelain"]
                .contains(&self.color_scheme.as_str())
            && ["njorun", "graphite", "warm", "porcelain"]
                .contains(&self.light_color_scheme.as_str())
            && ["njorun", "graphite", "warm", "porcelain"]
                .contains(&self.dark_color_scheme.as_str())
            && ["en", "de"].contains(&self.language.as_str())
            && ["display_off", "clock", "disabled"].contains(&self.screensaver.as_str())
            && ["rooms", "favorites", "mixed"].contains(&self.home_view.as_str())
            && ["adaptive", "fullscreen"].contains(&self.details.as_str())
            && ["automatic", "manual"].contains(&self.update_policy.as_str())
            && ["stable", "beta"].contains(&self.update_channel.as_str())
            && valid_time(&self.update_time)
            && valid_selection(&self.favorites)
            && self.room_order.len() <= crate::MAX_AREA_COUNT
            && valid_selection(&self.room_order)
            && valid_selection(&self.excluded_entities)
            && self.entity_order.len() <= crate::MAX_AREA_COUNT + 1
            && self
                .entity_order
                .iter()
                .all(|(room, values)| valid_id(room) && valid_selection(values))
            && valid_name_map(&self.entity_names, MAX_ENTITY_NAME_ENTRIES)
            && valid_name_map(&self.room_names, MAX_ROOM_ALIAS_ENTRIES)
            && valid_icon_map(&self.room_icons)
            && serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= 3072)
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.chars().count() <= 128 && !value.chars().any(char::is_control)
}

fn valid_selection(values: &[String]) -> bool {
    values.len() <= 256
        && values
            .iter()
            .enumerate()
            .all(|(index, value)| valid_id(value) && !values[..index].contains(value))
}

fn valid_time(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 5
        && bytes[2] == b':'
        && bytes[..2].iter().all(u8::is_ascii_digit)
        && bytes[3..].iter().all(u8::is_ascii_digit)
        && value[..2].parse::<u8>().is_ok_and(|hour| hour < 24)
        && value[3..].parse::<u8>().is_ok_and(|minute| minute < 60)
}

/// Persisted applied settings. Revision never means applied until the caller has
/// committed the new record and updated its runtime/UI successfully.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedSettings {
    pub revision: u64,
    pub settings: Settings,
}

impl AppliedSettings {
    /// Build a validated candidate without mutating the currently applied record.
    /// Equal revisions are idempotent only when they carry identical settings.
    #[must_use]
    pub fn candidate(
        &self,
        revision: u64,
        settings: Settings,
        orientations: &[&str],
    ) -> Option<Self> {
        if revision < self.revision
            || !settings.is_valid(orientations)
            || (revision == self.revision && settings != self.settings)
        {
            return None;
        }
        Some(Self { revision, settings })
    }
}

/// A configuration the integration sent that the tablet may apply: the operation to
/// acknowledge and the validated candidate record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Configuration {
    pub operation_id: String,
    pub candidate: AppliedSettings,
    /// The installation-wide remote logging sink: empty means off. `None` when the message
    /// does not carry one (an older integration), which leaves the stored sink as it is.
    pub log_sink: Option<String>,
}

/// Why a configuration message was not accepted. Nothing is persisted or applied then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// Not a `configuration` message, or a field has the wrong type.
    Malformed,
    /// The operation expired before it arrived.
    Expired,
    /// Older than what is applied, or the same revision with other values.
    Stale,
    /// A value is outside what the tablet supports.
    Invalid,
}

impl Rejection {
    /// The word in the `rejected` message.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::Expired => "expired",
            Self::Stale => "stale",
            Self::Invalid => "invalid",
        }
    }
}

/// The reason in a `rejected` message when the tablet could not store a valid configuration.
pub const STORAGE_REFUSAL: &str = "storage";

/// The `rejected` message that answers a configuration the tablet did not apply (plan 006, O4).
///
/// `None` when the message does not carry a readable `operation_id` and `revision`, because
/// without them Home Assistant could not match the answer. Only sent on a session that
/// negotiated it.
#[must_use]
pub fn rejected_message(message: &serde_json::Value, reason: &str) -> Option<serde_json::Value> {
    let operation_id = message["operation_id"]
        .as_str()
        .filter(|id| valid_id(id) && id.len() <= 128)?;
    let revision = message["revision"].as_u64()?;
    Some(serde_json::json!({
        "type": "rejected",
        "operation_id": operation_id,
        "revision": revision,
        "reason": reason,
    }))
}

/// Checks a decrypted `configuration` message against the applied record.
///
/// `now` is the current Unix time in seconds. An equal revision with identical settings is
/// accepted again (HA resends after a reconnect) so the acknowledgement can be repeated.
///
/// # Errors
/// See [`Rejection`].
pub fn accept_configuration(
    applied: &AppliedSettings,
    message: &serde_json::Value,
    now: u64,
) -> Result<Configuration, Rejection> {
    let operation_id = message["operation_id"]
        .as_str()
        .filter(|id| valid_id(id) && id.len() <= 128)
        .ok_or(Rejection::Malformed)?;
    if message["type"] != "configuration" {
        return Err(Rejection::Malformed);
    }
    let expires_at = message["expires_at"].as_u64().ok_or(Rejection::Malformed)?;
    if expires_at <= now {
        return Err(Rejection::Expired);
    }
    let revision = message["revision"].as_u64().ok_or(Rejection::Malformed)?;
    let settings: Settings =
        serde_json::from_value(message["settings"].clone()).map_err(|_| Rejection::Malformed)?;
    if !settings.is_valid(&ORIENTATIONS) {
        return Err(Rejection::Invalid);
    }
    let log_sink = match message.get("log_sink") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => {
            let text = value.as_str().ok_or(Rejection::Malformed)?;
            crate::logging::parse_sink(text).ok_or(Rejection::Invalid)?;
            Some(text.trim().to_owned())
        }
    };
    let candidate = applied
        .candidate(revision, settings, &ORIENTATIONS)
        .ok_or(Rejection::Stale)?;
    Ok(Configuration {
        operation_id: operation_id.to_owned(),
        candidate,
        log_sink,
    })
}

#[cfg(test)]
mod configuration_tests {
    use serde_json::{Value, json};

    use super::{AppliedSettings, Rejection, Settings, accept_configuration};

    const NOW: u64 = 1_800_000_000;

    fn message(revision: u64, settings: &Settings) -> Value {
        json!({"type": "configuration", "operation_id": "op-1", "revision": revision,
               "settings": settings, "expires_at": NOW + 60})
    }

    fn dark() -> Settings {
        Settings {
            theme: "dark".into(),
            ..Settings::default()
        }
    }

    #[test]
    fn a_fresh_valid_configuration_becomes_the_candidate() {
        let applied = AppliedSettings::default();
        let accepted = accept_configuration(&applied, &message(4, &dark()), NOW).expect("accepted");
        assert_eq!(accepted.operation_id, "op-1");
        assert_eq!(accepted.candidate.revision, 4);
        assert_eq!(accepted.candidate.settings.theme, "dark");
    }

    #[test]
    fn the_log_sink_travels_with_the_configuration() {
        let applied = AppliedSettings::default();
        let mut with = message(2, &dark());
        assert_eq!(
            accept_configuration(&applied, &with, NOW)
                .expect("accepted")
                .log_sink,
            None,
            "an older integration sends none"
        );
        with["log_sink"] = json!(" https://loki.example ");
        assert_eq!(
            accept_configuration(&applied, &with, NOW)
                .expect("accepted")
                .log_sink,
            Some("https://loki.example".to_owned())
        );
        with["log_sink"] = json!("");
        assert_eq!(
            accept_configuration(&applied, &with, NOW)
                .expect("accepted")
                .log_sink,
            Some(String::new()),
            "empty switches logging off"
        );
        with["log_sink"] = json!(7);
        assert_eq!(
            accept_configuration(&applied, &with, NOW),
            Err(Rejection::Malformed)
        );
        with["log_sink"] = json!("http://user:pw@host");
        assert_eq!(
            accept_configuration(&applied, &with, NOW),
            Err(Rejection::Invalid)
        );
    }

    #[test]
    fn a_resent_revision_is_accepted_only_with_identical_settings() {
        let applied = AppliedSettings {
            revision: 4,
            settings: dark(),
        };
        assert!(accept_configuration(&applied, &message(4, &dark()), NOW).is_ok());
        let other = Settings {
            theme: "light".into(),
            ..Settings::default()
        };
        assert_eq!(
            accept_configuration(&applied, &message(4, &other), NOW),
            Err(Rejection::Stale)
        );
        assert_eq!(
            accept_configuration(&applied, &message(3, &dark()), NOW),
            Err(Rejection::Stale)
        );
    }

    #[test]
    fn malformed_expired_and_unsupported_configurations_are_refused() {
        let applied = AppliedSettings::default();
        let good = message(1, &dark());
        let mut cases: Vec<(Value, Rejection)> = Vec::new();
        for (key, value, why) in [
            ("type", json!("applied"), Rejection::Malformed),
            ("operation_id", json!(""), Rejection::Malformed),
            ("operation_id", json!("x".repeat(129)), Rejection::Malformed),
            ("operation_id", json!("a\nb"), Rejection::Malformed),
            ("revision", json!("one"), Rejection::Malformed),
            ("expires_at", json!(NOW), Rejection::Expired),
            ("expires_at", json!(null), Rejection::Malformed),
            ("settings", json!({"theme": "dark"}), Rejection::Malformed),
        ] {
            let mut changed = good.clone();
            changed[key] = value;
            cases.push((changed, why));
        }
        let mut unsupported = good.clone();
        unsupported["settings"]["orientation"] = json!("upside_down");
        cases.push((unsupported, Rejection::Invalid));
        let mut out_of_range = good;
        out_of_range["settings"]["brightness"] = json!(0);
        cases.push((out_of_range, Rejection::Invalid));
        for (case, expected) in cases {
            assert_eq!(
                accept_configuration(&applied, &case, NOW),
                Err(expected),
                "{case}"
            );
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use std::collections::BTreeMap;

    use super::Settings;
    use crate::RoomId;

    #[test]
    fn order_and_exclusions_come_from_the_settings() {
        let settings = Settings {
            room_order: vec!["kitchen".into(), "hall".into()],
            entity_order: BTreeMap::from([
                ("kitchen".into(), vec!["light.a".into()]),
                ("unassigned".into(), vec!["switch.b".into()]),
            ]),
            excluded_entities: vec!["light.hidden".into()],
            ..Settings::default()
        };
        let order = settings.dashboard_order();
        assert_eq!(order.area_ids, Some(vec!["kitchen".into(), "hall".into()]));
        assert_eq!(
            order
                .entity_ids_by_room
                .get(&RoomId::Area("kitchen".into())),
            Some(&vec!["light.a".to_owned()])
        );
        assert_eq!(
            order.entity_ids_by_room.get(&RoomId::Unassigned),
            Some(&vec!["switch.b".to_owned()])
        );
        assert!(settings.excludes("light.hidden"));
        assert!(!settings.excludes("light.shown"));
    }

    #[test]
    fn only_layout_changes_need_a_dashboard_rebuild() {
        let base = Settings::default();
        let theme = Settings {
            theme: "dark".into(),
            brightness: 30,
            ..Settings::default()
        };
        assert!(!base.layout_differs(&theme));
        for changed in [
            Settings {
                room_order: vec!["a".into()],
                ..Settings::default()
            },
            Settings {
                excluded_entities: vec!["light.a".into()],
                ..Settings::default()
            },
            Settings {
                entity_order: BTreeMap::from([("a".into(), vec!["light.a".into()])]),
                ..Settings::default()
            },
        ] {
            assert!(base.layout_differs(&changed));
        }
    }

    #[test]
    fn all_four_orientations_are_valid_and_others_are_not() {
        for orientation in [
            "portrait",
            "landscape",
            "portrait_flipped",
            "landscape_flipped",
        ] {
            let settings = Settings {
                orientation: orientation.into(),
                ..Settings::default()
            };
            assert!(settings.is_valid(&super::ORIENTATIONS), "{orientation}");
        }
        let settings = Settings {
            orientation: "diagonal".into(),
            ..Settings::default()
        };
        assert!(!settings.is_valid(&super::ORIENTATIONS));
    }
}

#[cfg(test)]
mod refusal_tests {
    use serde_json::json;

    use super::Rejection;

    fn message(revision: u64) -> serde_json::Value {
        json!({"type": "configuration", "operation_id": "op-1", "revision": revision})
    }

    #[test]
    fn a_refusal_is_answered_only_when_the_message_can_be_matched() {
        let good = message(3);
        let answer = super::rejected_message(&good, Rejection::Stale.wire()).unwrap();
        assert_eq!(
            answer,
            json!({"type": "rejected", "operation_id": "op-1", "revision": 3, "reason": "stale"})
        );
        let mut no_revision = good.clone();
        no_revision["revision"] = json!("three");
        assert!(super::rejected_message(&no_revision, "malformed").is_none());
        let mut no_id = good;
        no_id["operation_id"] = json!("");
        assert!(super::rejected_message(&no_id, "malformed").is_none());
    }

    #[test]
    fn the_words_of_a_refusal_are_the_documented_ones() {
        let words = [
            Rejection::Malformed.wire(),
            Rejection::Expired.wire(),
            Rejection::Stale.wire(),
            Rejection::Invalid.wire(),
            super::STORAGE_REFUSAL,
        ];
        assert_eq!(
            words,
            ["malformed", "expired", "stale", "invalid", "storage"]
        );
    }
}
