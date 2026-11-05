//! Subscribe-before-snapshot synchronization for the standard HA dashboard API.

use crate::{ClientError, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Raw bounded registry snapshot and timestamp-ordered entity state cache.
#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub areas: Vec<Value>,
    pub devices: Vec<Value>,
    pub entities: Vec<Value>,
    pub states: BTreeMap<String, Value>,
    #[serde(skip)]
    pub state_subscription: u64,
    pub state_order: Vec<String>,
    /// Deletion versions prevent delayed events from resurrecting removed entities.
    #[serde(default)]
    pub tombstones: BTreeMap<String, String>,
}

impl Snapshot {
    /// Subscribe first so mutations during registry/snapshot retrieval are retained.
    ///
    /// # Errors
    /// Rejects request failures, malformed rows and oversized installations.
    pub fn fetch(connection: &mut Connection) -> Result<Self, ClientError> {
        let (state_subscription, _) =
            connection.request(json!({"type":"subscribe_events","event_type":"state_changed"}))?;
        let (_, areas) = connection.request(json!({"type":"config/area_registry/list"}))?;
        let (_, devices) = connection.request(json!({"type":"config/device_registry/list"}))?;
        let (_, entities) = connection.request(json!({"type":"config/entity_registry/list"}))?;
        let (_, states) = connection.request(json!({"type":"get_states"}))?;
        let states = rows(states, 4096)?;
        let mut state_order = Vec::with_capacity(states.len());
        let mut indexed = BTreeMap::new();
        for state in states {
            let id = state["entity_id"]
                .as_str()
                .ok_or(ClientError::Protocol)?
                .to_owned();
            timestamp(&state)?;
            if indexed.contains_key(&id) {
                return Err(ClientError::Protocol);
            }
            state_order.push(id.clone());
            indexed.insert(id, state);
        }
        let mut snapshot = Self {
            areas: rows(areas, 128)?,
            devices: rows(devices, 1024)?,
            entities: rows(entities, 4096)?,
            states: indexed,
            state_order,
            state_subscription,
            tombstones: BTreeMap::new(),
        };
        for event in connection.take_deferred(state_subscription) {
            snapshot.apply_event(&event)?;
        }
        Ok(snapshot)
    }

    /// Apply one state event only if it is newer than the cache. Older buffered
    /// events cannot overwrite a more recent snapshot. Returns whether it changed.
    ///
    /// # Errors
    /// Rejects malformed timestamps, identities and state-count overflow.
    pub fn apply_event(&mut self, event: &Value) -> Result<bool, ClientError> {
        if event["type"] != "event" || event["id"].as_u64() != Some(self.state_subscription) {
            return Ok(false);
        }
        let data = &event["event"]["data"];
        let id = data["entity_id"].as_str().ok_or(ClientError::Protocol)?;
        let state = &data["new_state"];
        if state.is_null() {
            // A deletion event can also have been queued before a newer snapshot.
            let removed_at = event["event"]["time_fired"]
                .as_str()
                .ok_or(ClientError::Protocol)?;
            let removed_text = removed_at.to_owned();
            let removed_at =
                OffsetDateTime::parse(removed_at, &Rfc3339).map_err(|_| ClientError::Protocol)?;
            if self.deleted_after(id, removed_at)? {
                return Ok(false);
            }
            if let Some(current) = self.states.get(id)
                && timestamp(current)? > removed_at
            {
                return Ok(false);
            }
            if self.tombstones.len() >= 4096 && !self.tombstones.contains_key(id) {
                return Err(ClientError::Limit);
            }
            self.tombstones.insert(id.to_owned(), removed_text);
            self.state_order.retain(|known| known != id);
            self.states.remove(id);
            // A new tombstone changes the cache even if no visible entity existed.
            return Ok(true);
        }
        if state["entity_id"] != id {
            return Err(ClientError::Protocol);
        }
        let updated_at = timestamp(state)?;
        if self.deleted_after(id, updated_at)? {
            return Ok(false);
        }
        if let Some(current) = self.states.get(id) {
            if timestamp(current)? >= updated_at {
                return Ok(false);
            }
        } else {
            if self.states.len() >= 4096 {
                return Err(ClientError::Limit);
            }
            self.state_order.push(id.to_owned());
        }
        self.tombstones.remove(id);
        self.states.insert(id.to_owned(), state.clone());
        Ok(true)
    }

    fn deleted_after(&self, id: &str, at: OffsetDateTime) -> Result<bool, ClientError> {
        self.tombstones.get(id).map_or(Ok(false), |deleted| {
            OffsetDateTime::parse(deleted, &Rfc3339)
                .map(|version| version >= at)
                .map_err(|_| ClientError::Protocol)
        })
    }

    /// Preserve the original HA state order for the core's fallback ordering.
    #[must_use]
    pub fn ordered_states(&self) -> Vec<Value> {
        self.state_order
            .iter()
            .filter_map(|id| self.states.get(id).cloned())
            .collect()
    }
}

fn rows(value: Value, maximum: usize) -> Result<Vec<Value>, ClientError> {
    let Value::Array(values) = value else {
        return Err(ClientError::Protocol);
    };
    if values.len() > maximum {
        return Err(ClientError::Limit);
    }
    Ok(values)
}

fn timestamp(state: &Value) -> Result<OffsetDateTime, ClientError> {
    let text = state["last_updated"]
        .as_str()
        .ok_or(ClientError::Protocol)?;
    OffsetDateTime::parse(text, &Rfc3339).map_err(|_| ClientError::Protocol)
}
