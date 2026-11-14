//! Attribute keys consumed by the shared presentation and entity-control models.
//!
//! Transports may use this inventory to discard unrelated Home Assistant attributes before
//! constructing bounded state snapshots. `njorun_script_fields` is injected from `get_services`
//! rather than read from a state attribute.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::MAX_IDENTIFIER_CHARS;

pub const ENTITY_ATTRIBUTE_KEYS: &[&str] = &[
    "friendly_name",
    "icon",
    "njorun_script_fields",
    "njorun_script_fields_omitted",
    "unit_of_measurement",
    "device_class",
    "state_class",
    "entity_picture",
    "supported_features",
    "entity_id",
    "motion_detection",
    "current_activity",
    "available_tones",
    "activity_list",
    "supported_color_modes",
    "color_mode",
    "brightness",
    "color_temp_kelvin",
    "min_color_temp_kelvin",
    "max_color_temp_kelvin",
    "effect",
    "effect_list",
    "rgb_color",
    "rgbw_color",
    "rgbww_color",
    "hs_color",
    "xy_color",
    "percentage",
    "percentage_step",
    "preset_mode",
    "preset_modes",
    "direction",
    "oscillating",
    "hvac_mode",
    "hvac_modes",
    "hvac_action",
    "temperature",
    "temperature_unit",
    "current_temperature",
    "target_temp_high",
    "target_temp_low",
    "target_temp_step",
    "precision",
    "display_precision",
    "suggested_display_precision",
    "target_humidity",
    "target_humidity_step",
    "current_humidity",
    "humidity",
    "min_temp",
    "max_temp",
    "min_humidity",
    "max_humidity",
    "swing_mode",
    "swing_modes",
    "swing_horizontal_mode",
    "swing_horizontal_modes",
    "fan_mode",
    "fan_modes",
    "current_position",
    "current_tilt_position",
    "available_modes",
    "mode",
    "min",
    "max",
    "step",
    "current",
    "operation_mode",
    "operation_list",
    "away_mode",
    "options",
    "code_format",
    "code_arm_required",
    "battery_level",
    "pressure",
    "wind_speed",
    "wind_bearing",
    "cleaning_area",
    "message",
    "start_time",
    "end_time",
    "location",
    "event_type",
    "source_type",
    "latitude",
    "longitude",
    "gps_accuracy",
    "last_triggered",
    "next_event",
    "duration",
    "finishes_at",
    "installed_version",
    "latest_version",
    "skipped_version",
    "release_summary",
    "release_url",
    "title",
    "in_progress",
    "elevation",
    "next_rising",
    "next_setting",
    "air_quality_index",
    "particulate_matter_2_5",
    "carbon_dioxide",
    "volume_level",
    "is_volume_muted",
    "media_title",
    "media_artist",
    "media_album_name",
    "media_album_artist",
    "media_track",
    "media_content_type",
    "media_content_id",
    "media_position",
    "media_position_updated_at",
    "media_duration",
    "media_image_url",
    "media_image_hash",
    "source",
    "source_list",
    "sound_mode",
    "sound_mode_list",
    "shuffle",
    "repeat",
    "fan_speed",
    "fan_speed_list",
    "has_date",
    "has_time",
    "pattern",
];

/// Maximum per-script field descriptors accepted from Home Assistant's service registry.
pub const MAX_SCRIPT_FIELDS: usize = 16;
/// Maximum encoded bytes retained for one script's field descriptors.
pub const MAX_SCRIPT_FIELD_BYTES: usize = 4096;

#[must_use]
pub fn is_entity_attribute(key: &str) -> bool {
    ENTITY_ATTRIBUTE_KEYS.contains(&key)
}

/// Removes unknown keys and applies the shared per-entity attribute limit in priority order.
pub fn retain_entity_attributes(attributes: &mut BTreeMap<String, Value>) {
    attributes.retain(|key, _| is_entity_attribute(key));
    if attributes.len() <= crate::MAX_ATTRIBUTE_COUNT {
        return;
    }
    let mut retained = BTreeMap::new();
    for key in ENTITY_ATTRIBUTE_KEYS {
        if let Some(value) = attributes.remove(*key) {
            retained.insert((*key).to_owned(), value);
            if retained.len() == crate::MAX_ATTRIBUTE_COUNT {
                break;
            }
        }
    }
    *attributes = retained;
}

/// Selects bounded, structured script fields and returns their omitted count.
#[must_use]
pub fn bounded_script_fields(fields: &serde_json::Map<String, Value>) -> Option<(Value, usize)> {
    let mut bounded = serde_json::Map::new();
    let mut remaining_bytes = MAX_SCRIPT_FIELD_BYTES;
    for (name, descriptor) in fields.iter().take(MAX_SCRIPT_FIELDS) {
        if name.chars().count() > MAX_IDENTIFIER_CHARS || !descriptor.is_object() {
            continue;
        }
        let encoded = serde_json::to_vec(descriptor).ok()?;
        let field_bytes = encoded.len().saturating_add(name.len());
        if field_bytes > remaining_bytes {
            continue;
        }
        remaining_bytes -= field_bytes;
        bounded.insert(name.clone(), descriptor.clone());
    }
    let omitted = fields.len().saturating_sub(bounded.len());
    Some((Value::Object(bounded), omitted))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::{MAX_SCRIPT_FIELDS, bounded_script_fields};
    use serde_json::{Map, json};

    #[test]
    fn script_fields_are_structured_bounded_and_count_omissions() {
        let fields: Map<String, _> = (0..MAX_SCRIPT_FIELDS + 3)
            .map(|index| {
                (
                    format!("field_{index:02}"),
                    json!({"selector": {"text": {}}}),
                )
            })
            .collect();
        let (bounded, omitted) = bounded_script_fields(&fields).expect("valid descriptors");
        assert_eq!(bounded.as_object().unwrap().len(), MAX_SCRIPT_FIELDS);
        assert_eq!(omitted, 3);

        let oversized = Map::from_iter([("field".into(), json!({"text": "x".repeat(5000)}))]);
        let (bounded, omitted) = bounded_script_fields(&oversized).unwrap();
        assert!(bounded.as_object().unwrap().is_empty());
        assert_eq!(omitted, 1);
        let scalar = Map::from_iter([("field".into(), json!("not a descriptor"))]);
        let (bounded, omitted) = bounded_script_fields(&scalar).unwrap();
        assert!(bounded.as_object().unwrap().is_empty());
        assert_eq!(omitted, 1);
    }
}
