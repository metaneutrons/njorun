//! Simulator network worker. No UI code or GPIO assumptions live here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{RuntimeCommand, RuntimeEvent};
use njorun_app_core::{
    ActionAcknowledgement, AppCore, ConnectionStatus, DetailAction, PrimaryTap,
    settings::{AppliedSettings, Settings},
};
use njorun_ha_client::{
    ClientError, Connection, Credentials, ManagementSession, snapshot::Snapshot, storage,
};
use serde_json::{Value, json};

/// File ownership is per simulator identity; do not share this directory between processes.
pub struct RuntimeOptions {
    pub credentials: PathBuf,
    pub state_directory: PathBuf,
}

struct StateFiles {
    settings: PathBuf,
    cache: PathBuf,
    daylight: PathBuf,
    last_daylight: Option<bool>,
}

impl StateFiles {
    fn new(directory: &Path) -> Self {
        let daylight = directory.join("daylight.json");
        Self {
            settings: directory.join("settings.json"),
            cache: directory.join("dashboard.json"),
            last_daylight: storage::read(&daylight).ok(),
            daylight,
        }
    }

    fn update_daylight(
        &mut self,
        snapshot: &Snapshot,
        events: &SyncSender<RuntimeEvent>,
    ) -> Result<(), ClientError> {
        let value = match snapshot
            .states
            .get("sun.sun")
            .and_then(|state| state["state"].as_str())
        {
            Some("above_horizon") => true,
            Some("below_horizon") => false,
            _ => return Ok(()),
        };
        if self.last_daylight != Some(value) {
            storage::write(&self.daylight, &value)?;
            self.last_daylight = Some(value);
            emit(events, RuntimeEvent::Daylight(value))?;
        }
        Ok(())
    }
}

/// Run until Stop or until the UI receiver disappears. Network errors reconnect;
/// no home-control action is replayed across a reconnect.
///
/// # Errors
/// Returns a sanitized storage/authentication error for unusable local credentials.
pub fn run(
    options: &RuntimeOptions,
    commands: &Receiver<RuntimeCommand>,
    events: &SyncSender<RuntimeEvent>,
) -> Result<(), ClientError> {
    let mut credentials: Credentials = storage::read_private(&options.credentials)?;
    let mut files = StateFiles::new(&options.state_directory);
    let mut applied: AppliedSettings = if files.settings.exists() {
        storage::read(&files.settings)?
    } else {
        AppliedSettings::default()
    };
    if !applied.settings.is_valid(&["portrait", "landscape"]) {
        return Err(ClientError::Storage);
    }
    let mut core = AppCore::new();
    if let Some(daylight) = files.last_daylight {
        emit(events, RuntimeEvent::Daylight(daylight))?;
    }
    if let Ok(cache) = storage::read_bounded::<Snapshot>(&files.cache, 8 * 1024 * 1024) {
        rebuild(&mut core, &cache, &applied.settings)?;
        if files.last_daylight.is_none() {
            files.update_daylight(&cache, events)?;
        }
    }
    emit(events, RuntimeEvent::Settings(Box::new(applied.clone())))?;
    emit(events, RuntimeEvent::Dashboard(core.dashboard().clone()))?;
    let mut backoff = 1_u64;
    loop {
        if discard_actions(commands)? {
            return Ok(());
        }
        core.set_connection(ConnectionStatus::Synchronizing);
        emit(
            events,
            RuntimeEvent::Status("Connecting to Home Assistant".into()),
        )?;
        let result = connected(
            &credentials,
            &mut core,
            &mut applied,
            &mut files,
            commands,
            events,
        );
        if matches!(result, Ok(true)) {
            return Ok(());
        }
        if matches!(result, Err(ClientError::Authentication)) && credentials.refresh().is_ok() {
            storage::write(&options.credentials, &credentials)?;
            backoff = 1;
        }
        core.set_connection(ConnectionStatus::Offline);
        emit(events, RuntimeEvent::Dashboard(core.dashboard().clone()))?;
        let message = result.err().unwrap_or(ClientError::Connection).to_string();
        emit(
            events,
            RuntimeEvent::Status(format!("{message}. Reconnecting…")),
        )?;
        let jitter = u64::from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_millis()
                % 500,
        );
        let deadline = Instant::now() + Duration::from_millis(backoff * 1000 + jitter);
        while Instant::now() < deadline {
            if discard_actions(commands)? {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        backoff = (backoff * 2).min(30);
    }
}

fn connected(
    credentials: &Credentials,
    core: &mut AppCore,
    applied: &mut AppliedSettings,
    files: &mut StateFiles,
    commands: &Receiver<RuntimeCommand>,
    events: &SyncSender<RuntimeEvent>,
) -> Result<bool, ClientError> {
    let mut connection = Connection::connect(
        &credentials.ha_url,
        &credentials.access_token,
        credentials.allow_http,
    )?;
    let mut management = establish_management(&mut connection, credentials)?;
    if management.is_none() {
        emit(
            events,
            RuntimeEvent::Status("Dashboard connected; Njörun management is unavailable".into()),
        )?;
    }
    let mut snapshot = Snapshot::fetch(&mut connection)?;
    files.update_daylight(&snapshot, events)?;
    if discard_actions(commands)? {
        return Ok(true);
    }
    rebuild(core, &snapshot, &applied.settings)?;
    core.set_connection(ConnectionStatus::Ready);
    emit(events, RuntimeEvent::Dashboard(core.dashboard().clone()))?;
    if management.is_some() {
        emit(events, RuntimeEvent::Status("Connected".into()))?;
    }
    let mut pending_config: Option<Value> = None;
    let mut action_deadlines: BTreeMap<String, Instant> = BTreeMap::new();
    let mut cache_dirty = true;
    let mut last_cache_write = Instant::now();
    let mut last_heartbeat = Instant::now();
    loop {
        match commands.try_recv() {
            Ok(RuntimeCommand::Stop) | Err(TryRecvError::Disconnected) => return Ok(true),
            Ok(RuntimeCommand::SettingsApplied(revision)) => {
                if let (Some(config), Some(session)) = (&pending_config, management.as_mut())
                    && config["revision"].as_u64() == Some(revision)
                {
                    session.send(
                        &mut connection,
                        &json!({"type":"applied","operation_id":config["operation_id"],
                        "revision":revision,"settings":applied.settings}),
                    )?;
                    pending_config = None;
                }
            }
            Ok(command @ (RuntimeCommand::Tap(_) | RuntimeCommand::Action { .. })) => {
                dispatch_command(
                    command,
                    core,
                    &mut connection,
                    events,
                    &mut action_deadlines,
                )?;
            }
            Err(TryRecvError::Empty) => {}
        }
        if last_heartbeat.elapsed() >= Duration::from_secs(15) {
            connection.request(json!({"type":"ping"}))?;
            // HA's WebSocket can be ready before the integration has loaded.
            // Retry fresh device proof without interrupting a healthy dashboard.
            if management.is_none() {
                management = establish_management(&mut connection, credentials)?;
                if management.is_some() {
                    emit(events, RuntimeEvent::Status("Connected".into()))?;
                }
            }
            last_heartbeat = Instant::now();
        }
        if let Some(event) = connection.poll()? {
            if let Some(session) = management.as_mut()
                && event["id"].as_u64() == Some(session.subscription_id)
            {
                let config = session.decode(&event)?;
                apply_configuration(&config, applied, core, &snapshot, &files.settings, events)?;
                pending_config = Some(config);
            } else if snapshot.apply_event(&event)? {
                files.update_daylight(&snapshot, events)?;
                let id = event["event"]["data"]["entity_id"]
                    .as_str()
                    .ok_or(ClientError::Protocol)?;
                if let Some(state) = snapshot.states.get(id) {
                    core.apply_state(
                        serde_json::from_value(state.clone()).map_err(|_| ClientError::Protocol)?,
                    )
                    .map_err(|_| ClientError::Protocol)?;
                }
                rebuild(core, &snapshot, &applied.settings)?;
                emit(events, RuntimeEvent::Dashboard(core.dashboard().clone()))?;
                cache_dirty = true;
            }
        }
        expire_actions(&mut action_deadlines, core, events)?;
        if cache_dirty && last_cache_write.elapsed() >= Duration::from_secs(1) {
            storage::write_bounded(&files.cache, &snapshot, 8 * 1024 * 1024)?;
            cache_dirty = false;
            last_cache_write = Instant::now();
        }
    }
}

fn establish_management(
    connection: &mut Connection,
    credentials: &Credentials,
) -> Result<Option<ManagementSession>, ClientError> {
    match ManagementSession::establish(connection, credentials) {
        Ok(session) => Ok(Some(session)),
        Err(ClientError::Rejected) => Ok(None),
        Err(error) => Err(error),
    }
}

fn expire_actions(
    action_deadlines: &mut BTreeMap<String, Instant>,
    core: &mut AppCore,
    events: &SyncSender<RuntimeEvent>,
) -> Result<(), ClientError> {
    let expired: Vec<_> = action_deadlines
        .iter()
        .filter(|(_, deadline)| **deadline <= Instant::now())
        .map(|(id, _)| id.clone())
        .collect();
    for id in expired {
        action_deadlines.remove(&id);
        if core.mark_unconfirmed(&id).is_ok() {
            emit(events, RuntimeEvent::Dashboard(core.dashboard().clone()))?;
        }
    }
    Ok(())
}

fn apply_configuration(
    config: &Value,
    applied: &mut AppliedSettings,
    core: &mut AppCore,
    snapshot: &Snapshot,
    settings_file: &Path,
    events: &SyncSender<RuntimeEvent>,
) -> Result<(), ClientError> {
    apply_configuration_with_writer(
        config,
        applied,
        core,
        snapshot,
        settings_file,
        events,
        storage::write,
    )
}

fn apply_configuration_with_writer(
    config: &Value,
    applied: &mut AppliedSettings,
    core: &mut AppCore,
    snapshot: &Snapshot,
    settings_file: &Path,
    events: &SyncSender<RuntimeEvent>,
    write: impl FnOnce(&Path, &AppliedSettings) -> Result<(), ClientError>,
) -> Result<(), ClientError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ClientError::Protocol)?
        .as_secs();
    if config["type"] != "configuration"
        || config["operation_id"]
            .as_str()
            .is_none_or(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
        || config["expires_at"]
            .as_u64()
            .is_none_or(|expires| expires <= now)
    {
        return Err(ClientError::Protocol);
    }
    let revision = config["revision"].as_u64().ok_or(ClientError::Protocol)?;
    let settings: Settings =
        serde_json::from_value(config["settings"].clone()).map_err(|_| ClientError::Protocol)?;
    let candidate = applied
        .candidate(revision, settings, &["portrait", "landscape"])
        .ok_or(ClientError::Protocol)?;
    let mut candidate_core = core.clone();
    rebuild(&mut candidate_core, snapshot, &candidate.settings)?;
    let persisted = write(settings_file, &candidate);
    if let Err(error) = persisted {
        if error == ClientError::StorageCommitUncertain {
            // Rename committed the visible candidate. Keep memory consistent,
            // but do not permit an ACK until a later write confirms durability.
            *applied = candidate;
            *core = candidate_core;
        }
        return Err(error);
    }
    *applied = candidate;
    *core = candidate_core;
    // Publish the derived dashboard before the event which permits a UI ACK.
    // FIFO delivery then guarantees that new exclusions/order are rendered first.
    emit(events, RuntimeEvent::Dashboard(core.dashboard().clone()))?;
    emit(events, RuntimeEvent::Settings(Box::new(applied.clone())))?;
    Ok(())
}

fn rebuild(
    core: &mut AppCore,
    snapshot: &Snapshot,
    settings: &Settings,
) -> Result<(), ClientError> {
    let areas: Vec<njorun_app_core::AreaRegistryEntry> =
        serde_json::from_value(json!(snapshot.areas)).map_err(|_| ClientError::Protocol)?;
    let devices: Vec<njorun_app_core::DeviceRegistryEntry> =
        serde_json::from_value(json!(snapshot.devices)).map_err(|_| ClientError::Protocol)?;
    let mut entities: Vec<njorun_app_core::EntityRegistryEntry> =
        serde_json::from_value(json!(snapshot.entities)).map_err(|_| ClientError::Protocol)?;
    for entity in &mut entities {
        if settings.excludes(&entity.entity_id) {
            entity.hidden_by = Some("user".into());
        }
    }
    let states: Vec<njorun_app_core::HaEntityState> =
        serde_json::from_value(json!(snapshot.ordered_states()))
            .map_err(|_| ClientError::Protocol)?;
    let order = settings.dashboard_order();
    core.refresh_from_ha(&areas, &devices, &entities, &states, &order)
        .map_err(|_| ClientError::Limit)
}

fn emit(events: &SyncSender<RuntimeEvent>, event: RuntimeEvent) -> Result<(), ClientError> {
    events.send(event).map_err(|_| ClientError::Connection)
}

fn discard_actions(commands: &Receiver<RuntimeCommand>) -> Result<bool, ClientError> {
    loop {
        match commands.try_recv() {
            Ok(RuntimeCommand::Stop) | Err(TryRecvError::Disconnected) => return Ok(true),
            Ok(
                RuntimeCommand::Tap(_)
                | RuntimeCommand::Action { .. }
                | RuntimeCommand::SettingsApplied(_),
            ) => {}
            Err(TryRecvError::Empty) => return Ok(false),
        }
    }
}

fn dispatch_command(
    command: RuntimeCommand,
    core: &mut AppCore,
    connection: &mut Connection,
    events: &SyncSender<RuntimeEvent>,
    action_deadlines: &mut BTreeMap<String, Instant>,
) -> Result<(), ClientError> {
    match command {
        RuntimeCommand::Tap(entity_id) => {
            dispatch_tap(&entity_id, core, connection, events, action_deadlines)
        }
        RuntimeCommand::Action { entity_id, action } => dispatch_action(
            &entity_id,
            &action,
            core,
            connection,
            events,
            action_deadlines,
        ),
        RuntimeCommand::SettingsApplied(_) | RuntimeCommand::Stop => Ok(()),
    }
}

fn dispatch_tap(
    entity_id: &str,
    core: &mut AppCore,
    connection: &mut Connection,
    events: &SyncSender<RuntimeEvent>,
    action_deadlines: &mut BTreeMap<String, Instant>,
) -> Result<(), ClientError> {
    if let Ok(tap) = core.primary_tap(entity_id) {
        send_tap(tap, core, connection, events, action_deadlines)?;
    }
    Ok(())
}

fn dispatch_action(
    entity_id: &str,
    action: &DetailAction,
    core: &mut AppCore,
    connection: &mut Connection,
    events: &SyncSender<RuntimeEvent>,
    action_deadlines: &mut BTreeMap<String, Instant>,
) -> Result<(), ClientError> {
    if let Ok(tap) = core.request_action(entity_id, action) {
        send_tap(tap, core, connection, events, action_deadlines)?;
    }
    Ok(())
}

fn send_tap(
    tap: PrimaryTap,
    core: &mut AppCore,
    connection: &mut Connection,
    events: &SyncSender<RuntimeEvent>,
    action_deadlines: &mut BTreeMap<String, Instant>,
) -> Result<(), ClientError> {
    let Some((entity_id, call)) = tap.into_call() else {
        return Ok(());
    };
    emit(events, RuntimeEvent::Dashboard(core.dashboard().clone()))?;
    match connection.request(call.payload(&entity_id)) {
        Ok(_) => {
            core.acknowledge(&entity_id, ActionAcknowledgement::Accepted)
                .map_err(|_| ClientError::Protocol)?;
            action_deadlines.insert(entity_id, Instant::now() + Duration::from_secs(5));
        }
        Err(ClientError::Rejected) => {
            core.acknowledge(
                &entity_id,
                ActionAcknowledgement::Rejected {
                    reason: "Home Assistant rejected the action".into(),
                },
            )
            .map_err(|_| ClientError::Protocol)?;
        }
        Err(error) => return Err(error),
    }
    emit(events, RuntimeEvent::Dashboard(core.dashboard().clone()))?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/support/runtime_tests.rs"]
mod tests;
