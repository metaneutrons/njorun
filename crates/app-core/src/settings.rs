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

/// Settings exchanged with the HA integration; transport secrets are separate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub orientation: String,
    pub brightness: u8,
    pub theme: String,
    pub language: String,
    pub idle_timeout: u32,
    pub idle_brightness: u8,
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

impl Default for Settings {
    fn default() -> Self {
        Self {
            orientation: "portrait".into(),
            brightness: 80,
            theme: "dark".into(),
            language: "en".into(),
            idle_timeout: 300,
            idle_brightness: 10,
            screensaver: "display_off".into(),
            home_view: "rooms".into(),
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
            && (10..=86400).contains(&self.idle_timeout)
            && ["automatic", "light", "dark"].contains(&self.theme.as_str())
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
