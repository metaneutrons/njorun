//! Persistent Home Assistant session: registry/state snapshot, live `state_changed`
//! updates and service calls over one authenticated WebSocket.
//!
//! All socket work runs on one worker thread. Large JSON replies are parsed element by
//! element and filtered while parsing, so memory use follows the dashboard size instead of
//! the size of the whole installation.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::marker::PhantomData;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::Duration;

use njorun_app_core::presentation::parse_utc_offset;
use njorun_app_core::{
    AreaRegistryEntry, DeviceRegistryEntry, EntityRegistryEntry, HaEntityState,
    HomeAssistantService, MAX_ENTITY_COUNT,
};
use serde::de::{DeserializeOwned, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

use crate::board;
use crate::commissioning::{refresh_access_token, websocket_url, RestoreError};
use crate::store::HomeAssistantSession;

const SNAPSHOT_BUFFER_BYTES: usize = 6 * 1024 * 1024;
const EVENT_BUFFER_BYTES: usize = 100 * 1024;
const SMALL_BUFFER_BYTES: usize = 8 * 1024;
const REQUEST_TIMEOUT_MS: u32 = 30_000;
const SERVICE_TIMEOUT_MS: u32 = 15_000;
const KEPT_ATTRIBUTES: [&str; 5] = [
    "friendly_name",
    "icon",
    "unit_of_measurement",
    "device_class",
    "brightness",
];
const MAX_NAME_CHARS: usize = 128;
const MAX_STATE_CHARS: usize = 256;
const MAX_ICON_CHARS: usize = 64;

pub(crate) struct Snapshot {
    pub areas: Vec<AreaRegistryEntry>,
    pub devices: Vec<DeviceRegistryEntry>,
    pub entities: Vec<EntityRegistryEntry>,
    pub states: Vec<HaEntityState>,
}

pub(crate) enum Event {
    Snapshot(Box<Snapshot>),
    State(HaEntityState),
    ServiceResult {
        entity_id: String,
        result: Result<(), String>,
    },
    Offline,
    Rejected,
    /// Home Assistant's current UTC offset in seconds, for the local wall clock.
    UtcOffset(i32),
}

pub(crate) enum Command {
    CallService {
        entity_id: String,
        service: HomeAssistantService,
    },
}

enum Outcome {
    Rejected,
    Lost(String),
    Resync,
    Stopped,
}

pub(crate) fn spawn(
    session: HomeAssistantSession,
    access_token: Option<String>,
    commands: Receiver<Command>,
    events: Sender<Event>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("ha-session".to_owned())
        .stack_size(24 * 1024)
        .spawn(move || run(&session, access_token, &commands, &events))
        .map(|_| ())
}

fn run(
    session: &HomeAssistantSession,
    mut access_token: Option<String>,
    commands: &Receiver<Command>,
    events: &Sender<Event>,
) {
    let mut backoff = Duration::from_secs(3);
    loop {
        match connect_and_serve(session, access_token.take(), commands, events) {
            Outcome::Rejected => {
                let _ = events.send(Event::Rejected);
                return;
            }
            Outcome::Stopped => return,
            Outcome::Resync => {
                println!("Njörun D1001 HA session resync");
                backoff = Duration::from_secs(3);
            }
            Outcome::Lost(reason) => {
                println!("Njörun D1001 HA session lost: {reason}");
                if events.send(Event::Offline).is_err() {
                    return;
                }
                board::ha_close();
                fail_commands_for(backoff, commands, events);
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

fn fail_commands_for(wait: Duration, commands: &Receiver<Command>, events: &Sender<Event>) {
    let deadline = std::time::Instant::now() + wait;
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        match commands.recv_timeout(left.min(Duration::from_millis(500))) {
            Ok(Command::CallService { entity_id, .. }) => {
                let _ = events.send(Event::ServiceResult {
                    entity_id,
                    result: Err("Home Assistant is offline".to_owned()),
                });
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn connect_and_serve(
    session: &HomeAssistantSession,
    access_token: Option<String>,
    commands: &Receiver<Command>,
    events: &Sender<Event>,
) -> Outcome {
    let access_token = match access_token {
        Some(token) => token,
        None => match refresh_access_token(session) {
            Ok(token) => token,
            Err(RestoreError::HomeAssistantRejected) => return Outcome::Rejected,
            Err(_) => return Outcome::Lost("token refresh failed".to_owned()),
        },
    };
    let Ok(url) = websocket_url(&session.url) else {
        return Outcome::Lost("invalid Home Assistant URL".to_owned());
    };
    let auth = json!({"type": "auth", "access_token": access_token}).to_string();
    if let Err(code) = board::ha_open(&url, &auth) {
        return Outcome::Lost(format!("WebSocket open failed: {code}"));
    }
    let (free, largest) = board::internal_heap();
    println!("Njörun D1001 HA session open, internal heap free {free} largest {largest}");

    let mut next_id = 1_u64;
    let (snapshot, wanted) = match fetch_snapshot(&mut next_id) {
        Ok(result) => result,
        Err(reason) => return Outcome::Lost(reason),
    };
    let mut newest: HashMap<String, String> = snapshot
        .states
        .iter()
        .filter_map(|state| Some((state.entity_id.clone(), state.last_updated.clone()?)))
        .collect();
    println!(
        "Njörun D1001 snapshot: {} areas, {} devices, {} entities, {} states",
        snapshot.areas.len(),
        snapshot.devices.len(),
        snapshot.entities.len(),
        snapshot.states.len()
    );
    if events.send(Event::Snapshot(Box::new(snapshot))).is_err() {
        return Outcome::Stopped;
    }

    let clock_id = subscribe_clock(&mut next_id);
    let mut event_buffer = vec![0_u8; EVENT_BUFFER_BYTES];
    let mut backlog = true;
    loop {
        match commands.try_recv() {
            Ok(Command::CallService { entity_id, service }) => {
                let result = call_service(&mut next_id, &entity_id, service);
                if events
                    .send(Event::ServiceResult { entity_id, result })
                    .is_err()
                {
                    return Outcome::Stopped;
                }
                continue;
            }
            Err(TryRecvError::Disconnected) => return Outcome::Stopped,
            Err(TryRecvError::Empty) => {}
        }
        match board::ha_poll_event(&mut event_buffer, if backlog { 0 } else { 200 }) {
            Ok(Some(length)) => {
                if let Some(offset) =
                    clock_id.and_then(|id| parse_clock_event(&event_buffer[..length], id))
                {
                    if events.send(Event::UtcOffset(offset)).is_err() {
                        return Outcome::Stopped;
                    }
                    continue;
                }
                if let Some(state) = parse_state_event(&event_buffer[..length], &wanted) {
                    let stale = backlog
                        && matches!(
                            (newest.get(&state.entity_id), &state.last_updated),
                            (Some(known), Some(current)) if current <= known
                        );
                    if !stale && events.send(Event::State(state)).is_err() {
                        return Outcome::Stopped;
                    }
                }
            }
            Ok(None) => {
                if backlog {
                    backlog = false;
                    newest = HashMap::new();
                }
                if board::ha_events_lost() {
                    board::ha_close();
                    return Outcome::Resync;
                }
            }
            Err(code) => return Outcome::Lost(format!("event stream ended: {code}")),
        }
    }
}

fn fetch_snapshot(next_id: &mut u64) -> Result<(Snapshot, HashSet<String>), String> {
    let mut buffer = vec![0_u8; SNAPSHOT_BUFFER_BYTES];
    let id = bump(next_id);
    let reply = request(
        &json!({"id": id, "type": "subscribe_events", "event_type": "state_changed"}).to_string(),
        &mut buffer,
        REQUEST_TIMEOUT_MS,
    )?;
    ensure_success(reply)?;

    let areas: Vec<AreaRegistryEntry> =
        list(next_id, "config/area_registry/list", &mut buffer, |_| true)?;
    let devices: Vec<DeviceRegistryEntry> =
        list(next_id, "config/device_registry/list", &mut buffer, |_| {
            true
        })?;
    let mut entities: Vec<EntityRegistryEntry> = list(
        next_id,
        "config/entity_registry/list",
        &mut buffer,
        |entity: &EntityRegistryEntry| {
            domain_rank(&entity.entity_id).is_some()
                && entity.disabled_by.is_none()
                && entity.hidden_by.is_none()
                && entity.entity_category.as_deref() != Some("diagnostic")
        },
    )?;
    cap_entities(&mut entities);
    let wanted: HashSet<String> = entities.iter().map(|e| e.entity_id.clone()).collect();
    let states: Vec<LeanState> = list(next_id, "get_states", &mut buffer, |state: &LeanState| {
        wanted.contains(&state.entity_id)
    })?;
    let states = states.into_iter().map(LeanState::into_state).collect();
    Ok((
        Snapshot {
            areas,
            devices,
            entities,
            states,
        },
        wanted,
    ))
}

/// Actuators first: the core refuses more than `MAX_ENTITY_COUNT` eligible entities.
fn cap_entities(entities: &mut Vec<EntityRegistryEntry>) {
    if entities.len() <= MAX_ENTITY_COUNT {
        return;
    }
    let before = entities.len();
    let mut keep = vec![true; entities.len()];
    let mut excess = entities.len() - MAX_ENTITY_COUNT;
    for rank in [3_u8, 2] {
        for (index, entity) in entities.iter().enumerate().rev() {
            if excess == 0 {
                break;
            }
            if keep[index] && domain_rank(&entity.entity_id) == Some(rank) {
                keep[index] = false;
                excess -= 1;
            }
        }
    }
    let mut index = 0;
    entities.retain(|_| {
        index += 1;
        keep[index - 1]
    });
    println!(
        "Njörun D1001 showing {} of {before} supported entities (sensors dropped first)",
        entities.len()
    );
}

/// 0 light/switch, 1 scene, 2 binary sensor, 3 sensor; `None` for unsupported domains.
fn domain_rank(entity_id: &str) -> Option<u8> {
    match entity_id.split_once('.')?.0 {
        "light" | "switch" => Some(0),
        "scene" => Some(1),
        "binary_sensor" => Some(2),
        "sensor" => Some(3),
        _ => None,
    }
}

fn bump(next_id: &mut u64) -> u64 {
    let id = *next_id;
    *next_id += 1;
    id
}

fn request<'a>(message: &str, buffer: &'a mut [u8], timeout_ms: u32) -> Result<&'a [u8], String> {
    let length = board::ha_request(message, buffer, timeout_ms)
        .map_err(|code| format!("request failed: {code}"))?;
    Ok(&buffer[..length])
}

fn ensure_success(reply: &[u8]) -> Result<(), String> {
    let head = &reply[..reply.len().min(200)];
    if find(head, b"\"success\":true").is_some() {
        Ok(())
    } else {
        Err(error_message(reply))
    }
}

fn error_message(reply: &[u8]) -> String {
    serde_json::from_slice::<Value>(reply)
        .ok()
        .and_then(|value| value["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "Home Assistant reported an error".to_owned())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn list<T: DeserializeOwned>(
    next_id: &mut u64,
    kind: &str,
    buffer: &mut [u8],
    keep: impl FnMut(&T) -> bool,
) -> Result<Vec<T>, String> {
    let id = bump(next_id);
    let reply = request(
        &json!({"id": id, "type": kind}).to_string(),
        buffer,
        REQUEST_TIMEOUT_MS,
    )?;
    parse_result(reply, keep).map_err(|error| format!("{kind}: {error}"))
}

struct Filtered<T, F> {
    keep: F,
    marker: PhantomData<T>,
}

impl<'de, T: Deserialize<'de>, F: FnMut(&T) -> bool> Visitor<'de> for Filtered<T, F> {
    type Value = Vec<T>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a list")
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<Vec<T>, A::Error> {
        let mut kept = Vec::new();
        while let Some(item) = seq.next_element::<T>()? {
            if (self.keep)(&item) {
                kept.push(item);
            }
        }
        Ok(kept)
    }
}

/// Parses the `result` array of a successful reply, dropping rows `keep` rejects as it goes.
fn parse_result<T: DeserializeOwned>(
    reply: &[u8],
    keep: impl FnMut(&T) -> bool,
) -> Result<Vec<T>, String> {
    let head = &reply[..reply.len().min(200)];
    if find(head, b"\"success\":true").is_none() {
        return Err(error_message(reply));
    }
    let start = find(head, b"\"result\":").ok_or("reply has no result")? + b"\"result\":".len();
    let mut deserializer = serde_json::Deserializer::from_slice(&reply[start..]);
    deserializer
        .deserialize_seq(Filtered {
            keep,
            marker: PhantomData,
        })
        .map_err(|error| error.to_string())
}

struct Attributes(BTreeMap<String, Value>);

impl<'de> Deserialize<'de> for Attributes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Keep;
        impl<'de> Visitor<'de> for Keep {
            type Value = Attributes;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an attribute map")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Attributes, A::Error> {
                let mut kept = BTreeMap::new();
                while let Some(key) = map.next_key::<String>()? {
                    if KEPT_ATTRIBUTES.contains(&key.as_str()) {
                        kept.insert(key, map.next_value::<Value>()?);
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(Attributes(kept))
            }

            fn visit_unit<E>(self) -> Result<Attributes, E> {
                Ok(Attributes(BTreeMap::new()))
            }
        }
        deserializer.deserialize_map(Keep)
    }
}

#[derive(Deserialize)]
struct LeanState {
    entity_id: String,
    state: String,
    #[serde(default = "empty_attributes")]
    attributes: Attributes,
    #[serde(default)]
    last_updated: Option<String>,
}

fn empty_attributes() -> Attributes {
    Attributes(BTreeMap::new())
}

impl LeanState {
    /// Clamps values to the core's bounds so one odd entity cannot reject the snapshot.
    fn into_state(mut self) -> HaEntityState {
        truncate(&mut self.state, MAX_STATE_CHARS);
        let attributes = &mut self.attributes.0;
        if let Some(Value::String(name)) = attributes.get_mut("friendly_name") {
            truncate(name, MAX_NAME_CHARS);
            if name.is_empty() {
                attributes.remove("friendly_name");
            }
        }
        if attributes
            .get("icon")
            .and_then(Value::as_str)
            .is_some_and(|icon| icon.chars().count() > MAX_ICON_CHARS)
        {
            attributes.remove("icon");
        }
        HaEntityState {
            entity_id: self.entity_id,
            state: self.state,
            attributes: self.attributes.0,
            last_updated: self.last_updated.filter(|value| value.len() <= 64),
        }
    }
}

fn truncate(value: &mut String, max_chars: usize) {
    if let Some((index, _)) = value.char_indices().nth(max_chars) {
        value.truncate(index);
    }
}

#[derive(Deserialize)]
struct EventEnvelope {
    event: EventBody,
}

#[derive(Deserialize)]
struct EventBody {
    data: EventData,
}

#[derive(Deserialize)]
struct EventData {
    #[serde(default)]
    new_state: Option<LeanState>,
}

fn parse_state_event(message: &[u8], wanted: &HashSet<String>) -> Option<HaEntityState> {
    // Cheap pre-filter: most events concern entities the dashboard does not show.
    let marker = b"\"entity_id\":\"";
    let start = find(message, marker)? + marker.len();
    let end = start + message[start..].iter().position(|byte| *byte == b'"')?;
    let entity_id = std::str::from_utf8(&message[start..end]).ok()?;
    if !wanted.contains(entity_id) {
        return None;
    }
    let envelope: EventEnvelope = serde_json::from_slice(message).ok()?;
    let state = envelope.event.data.new_state?.into_state();
    (state.entity_id == entity_id).then_some(state)
}

/// Home Assistant re-renders `now()` every minute, so this subscription also follows
/// daylight-saving changes without a time zone database on the device.
fn subscribe_clock(next_id: &mut u64) -> Option<u64> {
    let id = bump(next_id);
    let mut buffer = vec![0_u8; SMALL_BUFFER_BYTES];
    let reply = request(
        &json!({"id": id, "type": "render_template", "template": "{{ now().isoformat() }}"})
            .to_string(),
        &mut buffer,
        SERVICE_TIMEOUT_MS,
    )
    .and_then(ensure_success);
    match reply {
        Ok(()) => Some(id),
        Err(error) => {
            println!("Njörun D1001 clock subscription failed: {error}");
            None
        }
    }
}

fn parse_clock_event(message: &[u8], id: u64) -> Option<i32> {
    let prefix = format!("{{\"id\":{id},\"type\":\"event\"");
    if !message.starts_with(prefix.as_bytes()) {
        return None;
    }
    let marker = b"\"result\":\"";
    let start = find(message, marker)? + marker.len();
    let end = start + message[start..].iter().position(|byte| *byte == b'"')?;
    parse_utc_offset(std::str::from_utf8(&message[start..end]).ok()?)
}

fn call_service(
    next_id: &mut u64,
    entity_id: &str,
    service: HomeAssistantService,
) -> Result<(), String> {
    let (domain, name) = match service {
        HomeAssistantService::LightToggle => ("light", "toggle"),
        HomeAssistantService::SwitchToggle => ("switch", "toggle"),
        HomeAssistantService::SceneTurnOn => ("scene", "turn_on"),
    };
    let id = bump(next_id);
    let mut buffer = vec![0_u8; SMALL_BUFFER_BYTES];
    let reply = request(
        &json!({
            "id": id,
            "type": "call_service",
            "domain": domain,
            "service": name,
            "target": {"entity_id": entity_id},
        })
        .to_string(),
        &mut buffer,
        SERVICE_TIMEOUT_MS,
    )?;
    ensure_success(reply)
}
