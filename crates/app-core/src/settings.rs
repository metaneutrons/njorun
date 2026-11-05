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
