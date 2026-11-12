//! Persistent Home Assistant session: registry/state snapshot, live `state_changed`
//! updates and service calls over one authenticated WebSocket.
//!
//! All socket work runs on one worker thread. Large JSON replies are parsed element by
//! element and filtered while parsing, so memory use follows the dashboard size instead of
//! the size of the whole installation.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::marker::PhantomData;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};

use njorun_app_core::presentation::parse_utc_offset;
use njorun_app_core::{
    AreaRegistryEntry, DeviceRegistryEntry, EntityRegistryEntry, HaEntityState, ServiceCall,
    MAX_ENTITY_COUNT,
};
use serde::de::{DeserializeOwned, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

use njorun_app_core::problems::ConnectionProblem;

use crate::board;
use crate::commissioning::{refresh_access_token, websocket_url, RestoreError};
use crate::logging;
use crate::management::{self, Handled};
use crate::store::HomeAssistantSession;
use njorun_app_core::settings::AppliedSettings;

const SNAPSHOT_BUFFER_BYTES: usize = 6 * 1024 * 1024;
const EVENT_BUFFER_BYTES: usize = 100 * 1024;
const SMALL_BUFFER_BYTES: usize = 8 * 1024;
const REQUEST_TIMEOUT_MS: u32 = 30_000;
const SERVICE_TIMEOUT_MS: u32 = 15_000;
const SERVICE_REPLY_TIMEOUT: Duration = Duration::from_secs(20);
const HEALTH_LOG_EVERY: Duration = Duration::from_secs(30);
/// Largest internal block below which reconnecting is treated as starved. Healthy: about
/// 24 KB with software AES; the failure in #70 left 12 KB and never recovered.
const RECONNECT_MIN_BLOCK: usize = 16 * 1024;
/// Consecutive losses with too little internal RAM before the tablet restarts to recover.
const STARVED_LOSSES_BEFORE_RESTART: u32 = 5;
/// The only attributes the tiles and controls read; everything else is dropped while parsing.
const KEPT_ATTRIBUTES: [&str; 18] = [
    "friendly_name",
    "icon",
    "unit_of_measurement",
    "device_class",
    "brightness",
    "supported_color_modes",
    "percentage",
    "percentage_step",
    "volume_level",
    "media_title",
    "media_artist",
    "options",
    "min",
    "max",
    "step",
    "code_format",
    "supported_features",
    "entity_picture",
];
const REGISTRY_EVENTS: [&str; 3] = [
    "entity_registry_updated",
    "device_registry_updated",
    "area_registry_updated",
];
const RESYNC_DEBOUNCE: Duration = Duration::from_secs(3);
const CAMERA_WIDTH: u16 = 800;
const CAMERA_HEIGHT: u16 = 600;
const MAX_NAME_CHARS: usize = 128;
const MAX_STATE_CHARS: usize = 256;
const MAX_ICON_CHARS: usize = 64;

pub(crate) struct Snapshot {
    pub areas: Vec<AreaRegistryEntry>,
    pub devices: Vec<DeviceRegistryEntry>,
    pub entities: Vec<EntityRegistryEntry>,
    pub states: Vec<HaEntityState>,
    /// `sun.sun` is above the horizon; `None` when Home Assistant has no sun entity.
    pub daylight: Option<bool>,
}

const SUN_ENTITY: &str = "sun.sun";

/// A decoded camera picture in RGB24.
pub(crate) struct CameraFrame {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

pub(crate) enum Event {
    Snapshot(Box<Snapshot>),
    State(HaEntityState),
    ServiceResult {
        entity_id: String,
        result: Result<(), String>,
    },
    Camera {
        entity_id: String,
        result: Result<CameraFrame, String>,
    },
    Offline(ConnectionProblem),
    Rejected,
    /// Home Assistant's current UTC offset in seconds, for the local wall clock.
    UtcOffset(i32),
    /// Whether the sun is up (`sun.sun`), for the automatic theme.
    Daylight(bool),
    /// State of the link to the Njörun integration.
    Management(management::Status),
    /// Settings from Home Assistant, stored and waiting to be shown; answer with
    /// `Command::SettingsApplied`.
    Settings(Box<AppliedSettings>),
    /// A live command (decision D6) to carry out; answer with `Command::LiveDone`.
    Live(njorun_app_core::live::Command),
}

pub(crate) enum Command {
    /// Reload everything from Home Assistant, after settings changed what the dashboard lists.
    Resync,
    /// The UI shows the settings of this revision: acknowledge them to Home Assistant.
    SettingsApplied(u64),
    /// The outcome of a live command.
    LiveDone { command_id: String, ok: bool },
    /// What the display shows now, for the telemetry.
    Backlight(njorun_app_core::live::Backlight),
    /// Fetch one picture from a camera entity.
    FetchCamera { entity_id: String },
    CallService {
        entity_id: String,
        call: ServiceCall,
    },
}

enum Outcome {
    Rejected,
    Lost(String, ConnectionProblem),
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
    let mut starved = 0_u32;
    loop {
        match connect_and_serve(session, access_token.take(), commands, events) {
            Outcome::Rejected => {
                let _ = events.send(Event::Rejected);
                return;
            }
            Outcome::Stopped => return,
            Outcome::Resync => {
                log::info!(target: "ha", "HA session resync");
                backoff = Duration::from_secs(3);
                starved = 0;
            }
            Outcome::Lost(reason, problem) => {
                let (free, largest) = board::internal_heap();
                log::warn!(target: "ha", "HA session lost: {reason} (internal heap free {free} largest {largest} min {})",
                    board::internal_min_free()
                );
                if events.send(Event::Offline(problem)).is_err() {
                    return;
                }
                board::ha_close();
                let (free, largest) = board::internal_heap();
                if largest < RECONNECT_MIN_BLOCK {
                    starved += 1;
                    log::warn!(target: "ha", "internal heap after close: free {free} largest {largest}; too fragmented to reconnect ({starved}/{STARVED_LOSSES_BEFORE_RESTART})");
                    if starved >= STARVED_LOSSES_BEFORE_RESTART {
                        log::error!(target: "ha", "internal RAM stays too fragmented to reconnect; restarting");
                        std::thread::sleep(Duration::from_millis(500));
                        board::restart();
                    }
                } else {
                    starved = 0;
                }
                fail_commands_for(backoff, commands, events);
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

fn fail_commands_for(wait: Duration, commands: &Receiver<Command>, events: &Sender<Event>) {
    let deadline = std::time::Instant::now() + wait;
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        logging::tick();
        // A network trial that waits for a session runs out here while Home Assistant is
        // unreachable, and the tablet then returns to the stored network.
        crate::network_apply::poll();
        match commands.recv_timeout(left.min(Duration::from_millis(500))) {
            Ok(Command::CallService { entity_id, .. }) => {
                let _ = events.send(Event::ServiceResult {
                    entity_id,
                    result: Err("Home Assistant is offline".to_owned()),
                });
            }
            Ok(Command::FetchCamera { entity_id }) => {
                let _ = events.send(Event::Camera {
                    entity_id,
                    result: Err("Home Assistant is offline".to_owned()),
                });
            }
            // Nothing to acknowledge to while offline; HA sends the settings again on reconnect.
            // Live results and the backlight state belong to a session that no longer exists,
            // and commands are never replayed (decision D6).
            Ok(
                Command::SettingsApplied(_)
                | Command::Resync
                | Command::LiveDone { .. }
                | Command::Backlight(_),
            ) => {}
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
            Err(RestoreError::HomeAssistantUnreachable(problem)) => {
                return Outcome::Lost("token refresh failed".to_owned(), problem);
            }
            Err(RestoreError::Wifi) => {
                return Outcome::Lost("Wi-Fi lost".to_owned(), ConnectionProblem::Unreachable);
            }
        },
    };
    let mut token = access_token.clone();
    let Ok(url) = websocket_url(&session.url) else {
        return Outcome::Lost(
            "invalid Home Assistant URL".to_owned(),
            ConnectionProblem::BadResponse,
        );
    };
    let auth = json!({"type": "auth", "access_token": access_token}).to_string();
    if let Err(code) = board::ha_open(&url, &auth) {
        let problem = ConnectionProblem::from_code(code);
        if problem == ConnectionProblem::SignInRejected {
            return Outcome::Rejected;
        }
        return Outcome::Lost(format!("WebSocket open failed: {code:#x}"), problem);
    }
    let (free, largest) = board::internal_heap();
    log::debug!(target: "ha", "HA session open, internal heap free {free} largest {largest}");

    let mut next_id = 1_u64;
    let (snapshot, wanted) = match fetch_snapshot(&mut next_id) {
        Ok(result) => result,
        Err(reason) => return Outcome::Lost(reason, ConnectionProblem::BadResponse),
    };
    let mut newest: HashMap<String, String> = snapshot
        .states
        .iter()
        .filter_map(|state| Some((state.entity_id.clone(), state.last_updated.clone()?)))
        .collect();
    log::info!(target: "ha", "snapshot: {} areas, {} devices, {} entities, {} states",
        snapshot.areas.len(),
        snapshot.devices.len(),
        snapshot.entities.len(),
        snapshot.states.len()
    );
    if events.send(Event::Snapshot(Box::new(snapshot))).is_err() {
        return Outcome::Stopped;
    }

    let clock_id = subscribe_clock(&mut next_id);
    let registry_ids = subscribe_registry(&mut next_id);
    let mut link = management::Link::open(&mut next_id, &session.url);
    let mut resync_at: Option<Instant> = None;
    let mut event_buffer = vec![0_u8; EVENT_BUFFER_BYTES];
    let mut backlog = true;
    let mut pending: HashMap<u64, (String, Instant)> = HashMap::new();
    let mut health = Health::new();
    loop {
        match commands.try_recv() {
            Ok(Command::SettingsApplied(revision)) => {
                if let Some(link) = link.as_mut() {
                    link.acknowledge(&mut next_id, revision);
                }
                continue;
            }
            Ok(Command::LiveDone { command_id, ok }) => {
                if let Some(link) = link.as_mut() {
                    link.command_done(&mut next_id, &command_id, ok);
                }
                continue;
            }
            Ok(Command::Backlight(backlight)) => {
                if let Some(link) = link.as_mut() {
                    link.set_backlight(backlight);
                }
                continue;
            }
            Ok(Command::Resync) => {
                log::info!(target: "ha", "settings changed the dashboard layout; reloading");
                board::ha_close();
                return Outcome::Resync;
            }
            Ok(Command::FetchCamera { entity_id }) => {
                let result = fetch_camera(session, &mut token, &entity_id);
                if let Err(error) = &result {
                    log::warn!(target: "camera", "camera {entity_id} failed: {error}");
                }
                if events.send(Event::Camera { entity_id, result }).is_err() {
                    return Outcome::Stopped;
                }
                continue;
            }
            Ok(Command::CallService { entity_id, call }) => {
                match send_service(&mut next_id, &entity_id, &call) {
                    Ok(id) => {
                        pending.insert(id, (entity_id, Instant::now()));
                    }
                    Err(reason) => {
                        log::warn!(target: "ha", "service call for {entity_id} not sent: {reason}");
                        let result = Err(reason);
                        if events
                            .send(Event::ServiceResult { entity_id, result })
                            .is_err()
                        {
                            return Outcome::Stopped;
                        }
                    }
                }
                continue;
            }
            Err(TryRecvError::Disconnected) => return Outcome::Stopped,
            Err(TryRecvError::Empty) => {}
        }
        logging::tick();
        if let Some(link) = link.as_mut() {
            link.tick(&mut next_id);
            link.health(&mut next_id);
            link.report(&mut next_id);
            if let Some(status) = link.changed_status() {
                if events.send(Event::Management(status)).is_err() {
                    return Outcome::Stopped;
                }
            }
        }
        if !pending.is_empty() && !expire_pending(&mut pending, events) {
            return Outcome::Stopped;
        }
        if resync_at.is_some_and(|when| when <= Instant::now()) {
            log::info!(target: "ha", "Home Assistant changed its registries; reloading");
            board::ha_close();
            return Outcome::Resync;
        }
        health.tick(pending.len());
        match board::ha_poll_event(&mut event_buffer, if backlog { 0 } else { 200 }) {
            Ok(Some(length)) => {
                health.received += 1;
                let id = event_id(&event_buffer[..length]);
                if let Some(link) = link.as_mut() {
                    match link.handle(&mut next_id, &event_buffer[..length], id) {
                        Handled::NotMine => {}
                        Handled::Consumed => continue,
                        Handled::Settings(applied) => {
                            if events.send(Event::Settings(applied)).is_err() {
                                return Outcome::Stopped;
                            }
                            continue;
                        }
                        Handled::Live(command) => {
                            if events.send(Event::Live(command)).is_err() {
                                return Outcome::Stopped;
                            }
                            continue;
                        }
                    }
                }
                if id.is_some_and(|id| registry_ids.contains(&id)) {
                    resync_at.get_or_insert_with(|| Instant::now() + RESYNC_DEBOUNCE);
                    continue;
                }
                if let Some((id, result)) = parse_service_reply(&event_buffer[..length]) {
                    if let Some((entity_id, sent)) = pending.remove(&id) {
                        log::info!(target: "ha", "service reply for {entity_id} after {} ms: {}",
                            sent.elapsed().as_millis(),
                            result.as_ref().map_or_else(String::as_str, |()| "ok")
                        );
                        if events
                            .send(Event::ServiceResult { entity_id, result })
                            .is_err()
                        {
                            return Outcome::Stopped;
                        }
                    }
                    continue;
                }
                if let Some(offset) =
                    clock_id.and_then(|id| parse_clock_event(&event_buffer[..length], id))
                {
                    if events.send(Event::UtcOffset(offset)).is_err() {
                        return Outcome::Stopped;
                    }
                    continue;
                }
                if let Some(state) = parse_state_event(&event_buffer[..length], &wanted) {
                    if state.entity_id == SUN_ENTITY {
                        let up = state.state == "above_horizon";
                        if events.send(Event::Daylight(up)).is_err() {
                            return Outcome::Stopped;
                        }
                        continue;
                    }
                    let stale = backlog
                        && matches!(
                            (newest.get(&state.entity_id), &state.last_updated),
                            (Some(known), Some(current)) if current <= known
                        );
                    if !stale {
                        health.applied += 1;
                        if pending
                            .values()
                            .any(|(entity, _)| *entity == state.entity_id)
                        {
                            log::info!(target: "ha", "state {} -> {} (awaited)",
                                state.entity_id, state.state
                            );
                        }
                        if events.send(Event::State(state)).is_err() {
                            return Outcome::Stopped;
                        }
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
            Err(code) => {
                return Outcome::Lost(
                    format!("event stream ended: {code}"),
                    ConnectionProblem::Unreachable,
                )
            }
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
    let mut wanted: HashSet<String> = entities.iter().map(|e| e.entity_id.clone()).collect();
    // The sun is not a dashboard entity, but it drives the automatic theme.
    wanted.insert(SUN_ENTITY.to_owned());
    let states: Vec<LeanState> = list(next_id, "get_states", &mut buffer, |state: &LeanState| {
        wanted.contains(&state.entity_id)
    })?;
    let mut daylight = None;
    let states = states
        .into_iter()
        .filter(|state| {
            if state.entity_id == SUN_ENTITY {
                daylight = Some(state.state == "above_horizon");
                false
            } else {
                true
            }
        })
        .map(LeanState::into_state)
        .collect();
    Ok((
        Snapshot {
            areas,
            devices,
            entities,
            states,
            daylight,
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
    log::warn!(target: "ha", "showing {} of {before} supported entities (sensors dropped first)",
        entities.len()
    );
}

/// How much an entity matters when the dashboard is full: 0 actuators and helpers, 1 scenes,
/// scripts, automations, buttons and cameras, 2 binary sensors, 3 sensors; `None` if unsupported.
fn domain_rank(entity_id: &str) -> Option<u8> {
    match entity_id.split_once('.')?.0 {
        "light"
        | "switch"
        | "fan"
        | "lock"
        | "media_player"
        | "alarm_control_panel"
        | "input_boolean"
        | "number"
        | "input_number"
        | "select"
        | "input_select" => Some(0),
        "scene" | "script" | "automation" | "button" | "input_button" | "camera" => Some(1),
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
            log::warn!(target: "ha", "clock subscription failed: {error}");
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

/// Counters for the periodic health line, which makes dropped messages and memory trends visible.
struct Health {
    last: Instant,
    received: u32,
    applied: u32,
}

impl Health {
    fn new() -> Self {
        Self {
            last: Instant::now(),
            received: 0,
            applied: 0,
        }
    }

    fn tick(&mut self, pending: usize) {
        if self.last.elapsed() < HEALTH_LOG_EVERY {
            return;
        }
        let (free, largest) = board::internal_heap();
        log::info!(target: "ha", "health: {} messages, {} applied, {pending} calls pending, internal heap free {free} largest {largest} min {}",
            self.received,
            self.applied,
            board::internal_min_free()
        );
        *self = Self::new();
    }
}

/// Fails calls Home Assistant never answered; returns false when the receiver is gone.
fn expire_pending(pending: &mut HashMap<u64, (String, Instant)>, events: &Sender<Event>) -> bool {
    let expired: Vec<u64> = pending
        .iter()
        .filter(|(_, (_, sent))| sent.elapsed() >= SERVICE_REPLY_TIMEOUT)
        .map(|(id, _)| *id)
        .collect();
    for id in expired {
        if let Some((entity_id, _)) = pending.remove(&id) {
            log::warn!(target: "ha", "service call for {entity_id} timed out");
            let sent = events.send(Event::ServiceResult {
                entity_id,
                result: Err("Home Assistant did not answer".to_owned()),
            });
            if sent.is_err() {
                return false;
            }
        }
    }
    true
}

/// `{"id":N,"type":"result","success":...}` for a call made with `send_service`.
fn parse_service_reply(message: &[u8]) -> Option<(u64, Result<(), String>)> {
    let rest = message.strip_prefix(b"{\"id\":")?;
    let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    let id: u64 = std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()?;
    if !rest[digits..].starts_with(b",\"type\":\"result\"") {
        return None;
    }
    Some((id, ensure_success(message)))
}

/// Camera ids come from Home Assistant but end up in a URL, so only plain ids are fetched.
fn is_camera_id(entity_id: &str) -> bool {
    entity_id.strip_prefix("camera.").is_some_and(|object| {
        !object.is_empty()
            && object
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

fn fetch_camera(
    session: &HomeAssistantSession,
    token: &mut String,
    entity_id: &str,
) -> Result<CameraFrame, String> {
    if !is_camera_id(entity_id) {
        return Err("not a camera".to_owned());
    }
    let url = format!(
        "{}/api/camera_proxy/{entity_id}?width={CAMERA_WIDTH}",
        session.url
    );
    let body = match board::http_get_bearer(&url, token) {
        Err(board::ERR_REJECTED) => {
            // The access token lasts 30 minutes; refresh it once and retry.
            *token =
                refresh_access_token(session).map_err(|_| "token refresh failed".to_owned())?;
            board::http_get_bearer(&url, token)
        }
        other => other,
    }
    .map_err(|code| format!("request failed: {code}"))?;
    decode_jpeg(&body)
}

/// Decodes a JPEG at the smallest scale that still fills the sheet, which saves most of the
/// work and memory for large camera pictures.
fn decode_jpeg(bytes: &[u8]) -> Result<CameraFrame, String> {
    let mut decoder = jpeg_decoder::Decoder::new(bytes);
    decoder.read_info().map_err(|error| error.to_string())?;
    let (width, height) = decoder
        .scale(CAMERA_WIDTH, CAMERA_HEIGHT)
        .map_err(|error| error.to_string())?;
    let pixels = decoder.decode().map_err(|error| error.to_string())?;
    let format = decoder
        .info()
        .ok_or_else(|| "missing picture info".to_owned())?
        .pixel_format;
    let (width, height) = (usize::from(width), usize::from(height));
    let rgb = match format {
        jpeg_decoder::PixelFormat::RGB24 => pixels,
        jpeg_decoder::PixelFormat::L8 => pixels.iter().flat_map(|gray| [*gray; 3]).collect(),
        _ => return Err("unsupported picture format".to_owned()),
    };
    if rgb.len() != width * height * 3 {
        return Err("unexpected picture size".to_owned());
    }
    Ok(CameraFrame {
        width: u32::try_from(width).map_err(|_| "picture too wide".to_owned())?,
        height: u32::try_from(height).map_err(|_| "picture too tall".to_owned())?,
        rgb,
    })
}

/// Sends a service call without waiting; the reply is matched by id in the event loop.
fn send_service(next_id: &mut u64, entity_id: &str, call: &ServiceCall) -> Result<u64, String> {
    let id = bump(next_id);
    let message = call.message(id, entity_id).to_string();
    log::info!(target: "ha", "service {}.{} for {entity_id} sent as {id}",
        call.domain, call.service
    );
    board::ha_send(&message)
        .map(|()| id)
        .map_err(|code| format!("send failed: {code}"))
}

/// Subscribes to registry changes, so entities added or moved in Home Assistant appear
/// without restarting the tablet. Returns the subscription ids.
fn subscribe_registry(next_id: &mut u64) -> Vec<u64> {
    let mut ids = Vec::new();
    let mut buffer = vec![0_u8; SMALL_BUFFER_BYTES];
    for event_type in REGISTRY_EVENTS {
        let id = bump(next_id);
        let reply = request(
            &json!({"id": id, "type": "subscribe_events", "event_type": event_type}).to_string(),
            &mut buffer,
            SERVICE_TIMEOUT_MS,
        )
        .and_then(ensure_success);
        match reply {
            Ok(()) => ids.push(id),
            Err(error) => log::warn!(target: "ha", "{event_type} subscription failed: {error}"),
        }
    }
    ids
}

/// The subscription id of an event message `{"id":N,"type":"event",...}`.
fn event_id(message: &[u8]) -> Option<u64> {
    let rest = message.strip_prefix(b"{\"id\":")?;
    let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    if !rest[digits..].starts_with(b",\"type\":\"event\"") {
        return None;
    }
    std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()
}
