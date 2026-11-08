//! Versioned desired settings. HA is the only preference writer.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
            theme: "automatic".into(),
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
    /// Validate limits independently of HA, before persistence or UI application.
    #[must_use]
    pub fn is_valid(&self, orientations: &[&str]) -> bool {
        orientations.contains(&self.orientation.as_str())
            && ["portrait", "landscape"].contains(&self.orientation.as_str())
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

const ORIENTATIONS: [&str; 2] = ["portrait", "landscape"];

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
    let candidate = applied
        .candidate(revision, settings, &ORIENTATIONS)
        .ok_or(Rejection::Stale)?;
    Ok(Configuration {
        operation_id: operation_id.to_owned(),
        candidate,
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
