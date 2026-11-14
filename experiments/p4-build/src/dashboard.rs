//! Home dashboard state on the device: applies Home Assistant session events to the shared
//! `AppCore`, turns taps into service calls and renders rooms and tiles into Slint.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use njorun_app_core::presentation::{
    self, detail_attributes, detail_value, detail_view, feedback_value, format_local_timestamp,
    room_summary, spaced_code, state_value, IconKind, Slider,
};
use njorun_app_core::{
    ActionAcknowledgement, ActionFeedback, AppCore, ConnectionStatus, Dashboard, EntityDomain,
    EntityTile, PrimaryTap, RoomId, TapBehavior,
};
use slint::{ModelRc, SharedString, VecModel};

use njorun_app_core::problems::ConnectionProblem;

use crate::generated::{
    ActionItem, ControlItem, DetailsItem, EntityIcon, InputItem, MainWindow, ResultItem, RoomItem,
    TileItem,
};
#[path = "../../../crates/simulator/src/control_adapter.rs"]
pub(crate) mod control_adapter;
use crate::ha::{self, CameraFrame, Command, Event};
use crate::management;
use crate::store::HomeAssistantSession;
use njorun_app_core::settings::{AppliedSettings, Settings};

const UNCONFIRMED_AFTER: Duration = Duration::from_secs(8);
const SCENE_FEEDBACK_FOR: Duration = Duration::from_secs(2);
const MAX_REASON_CHARS: usize = 200;

enum Deadline {
    Unconfirmed,
    ClearFeedback,
}

pub(crate) enum Change {
    FirstSnapshot,
    Offline(ConnectionProblem),
    /// Daylight changed; the automatic theme may switch.
    Theme,
    Camera(String, Result<CameraFrame, String>),
    ActionResult(String, serde_json::Value),
    Updated,
    Rejected,
    DataError,
    /// Settings from Home Assistant, stored by the worker, to be shown and then acknowledged.
    Settings(Box<AppliedSettings>),
    /// A live command to carry out; report it with `live_done`.
    Live(njorun_app_core::live::Command),
}

pub(crate) enum UiAction {
    SelectRoom(i32),
    Activate(String),
    Details(String),
    DismissDetails,
    /// A control in the details sheet: entity, action key and argument.
    Detail(String, String, String),
    /// `portrait`, `landscape` or `flip`.
    Orientation(String),
    /// `light`, `dark` or `automatic`.
    Theme(String),
    /// Switch between English and German.
    ToggleLanguage,
    /// Show the list of rooms.
    ShowRooms,
    /// Back to the configured home view.
    GoHome,
}

pub(crate) enum Tap {
    Inspect(String),
    Sent,
    Ignored,
    Error(String),
}

pub(crate) struct Session {
    core: AppCore,
    events: Receiver<Event>,
    commands: Sender<Command>,
    deadlines: HashMap<String, (Instant, Deadline)>,
    seen_snapshot: bool,
    omitted_entities: usize,
    utc_offset: Option<i32>,
    /// Why the last connection attempt failed, until the next snapshot.
    problem: Option<ConnectionProblem>,
    daylight: Option<bool>,
    management: Option<management::Status>,
    /// What Home Assistant configured; decides the rooms, their order and the exclusions.
    settings: Settings,
}

impl Session {
    pub(crate) fn start(
        session: HomeAssistantSession,
        access_token: Option<String>,
        settings: Settings,
    ) -> Result<Self, String> {
        let (command_sender, command_receiver) = channel();
        let (event_sender, event_receiver) = channel();
        ha::spawn(session, access_token, command_receiver, event_sender)
            .map_err(|error| format!("could not start Home Assistant session: {error}"))?;
        let mut core = AppCore::default();
        core.set_connection(ConnectionStatus::Synchronizing);
        Ok(Self {
            core,
            events: event_receiver,
            commands: command_sender,
            deadlines: HashMap::new(),
            seen_snapshot: false,
            omitted_entities: 0,
            utc_offset: None,
            problem: None,
            daylight: None,
            management: None,
            settings,
        })
    }

    /// Asks the worker for a fresh picture from a camera entity.
    pub(crate) fn fetch_camera(&self, entity_id: &str) -> bool {
        self.commands
            .send(Command::FetchCamera {
                entity_id: entity_id.to_owned(),
            })
            .is_ok()
    }

    pub(crate) const fn omitted_entities(&self) -> usize {
        self.omitted_entities
    }

    pub(crate) const fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Takes new settings; `true` when they change what the dashboard lists, so the caller
    /// should have the worker reload from Home Assistant.
    pub(crate) fn set_settings(&mut self, settings: Settings) -> bool {
        let layout = self.settings.layout_differs(&settings);
        self.settings = settings;
        layout
    }

    pub(crate) fn resync(&self) {
        let _ = self.commands.send(Command::Resync);
    }

    /// Tells the worker the settings of `revision` are on screen, so it acknowledges them.
    pub(crate) fn settings_applied(&self, revision: u64) {
        let _ = self.commands.send(Command::SettingsApplied(revision));
    }

    /// Tells the worker the outcome of a live command.
    pub(crate) fn live_done(&self, command_id: String, ok: bool) {
        let _ = self.commands.send(Command::LiveDone { command_id, ok });
    }

    /// Tells the worker what the display shows now.
    pub(crate) fn backlight(&self, backlight: njorun_app_core::live::Backlight) {
        let _ = self.commands.send(Command::Backlight(backlight));
    }

    /// Whether the sun is up in Home Assistant; `None` until known.
    pub(crate) const fn daylight(&self) -> Option<bool> {
        self.daylight
    }

    pub(crate) const fn problem(&self) -> Option<ConnectionProblem> {
        self.problem
    }

    pub(crate) const fn management(&self) -> Option<&management::Status> {
        self.management.as_ref()
    }

    pub(crate) fn utc_offset(&self) -> Option<i32> {
        self.utc_offset
    }

    pub(crate) fn dashboard(&self) -> &Dashboard {
        self.core.dashboard()
    }

    /// Applies everything the worker has reported since the last call.
    pub(crate) fn poll(&mut self) -> Vec<Change> {
        let mut changes = Vec::new();
        loop {
            let event = match self.events.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    changes.push(Change::Offline(ConnectionProblem::Other));
                    break;
                }
            };
            match event {
                Event::Snapshot(mut snapshot) => {
                    self.problem = None;
                    self.omitted_entities = snapshot.omitted_entities;
                    snapshot
                        .states
                        .retain(|state| !self.settings.excludes(&state.entity_id));
                    for entity in &mut snapshot.entities {
                        if self.settings.excludes(&entity.entity_id) {
                            entity.hidden_by = Some("user".to_owned());
                        }
                    }
                    if snapshot.daylight.is_some() && snapshot.daylight != self.daylight {
                        self.daylight = snapshot.daylight;
                        changes.push(Change::Theme);
                    }
                    self.core.set_connection(ConnectionStatus::Ready);
                    match self.core.refresh_from_ha(
                        &snapshot.areas,
                        &snapshot.devices,
                        &snapshot.entities,
                        &snapshot.states,
                        &self.settings.dashboard_order(),
                    ) {
                        Ok(()) => {
                            changes.push(if self.seen_snapshot {
                                Change::Updated
                            } else {
                                Change::FirstSnapshot
                            });
                            self.seen_snapshot = true;
                        }
                        Err(error) => {
                            log::warn!(target: "ui", "dashboard rejected the snapshot: {error}");
                            self.core.set_connection(ConnectionStatus::Offline);
                            changes.push(Change::DataError);
                        }
                    }
                }
                Event::State(state) => match self.core.apply_state(state) {
                    Ok(()) => changes.push(Change::Updated),
                    Err(error) => log::warn!(target: "ui", "ignored a state update: {error}"),
                },
                Event::ServiceResult { entity_id, result } => {
                    if let Ok(value) = &result {
                        if !value.is_null() {
                            changes.push(Change::ActionResult(entity_id.clone(), value.clone()));
                        }
                    }
                    self.acknowledge(&entity_id, result.map(|_| ()));
                    changes.push(Change::Updated);
                }
                Event::Offline(problem) => {
                    self.core.set_connection(ConnectionStatus::Offline);
                    self.deadlines.clear();
                    self.problem = Some(problem);
                    changes.push(Change::Offline(problem));
                }
                Event::Rejected => changes.push(Change::Rejected),
                Event::Camera { entity_id, result } => {
                    changes.push(Change::Camera(entity_id, result));
                }
                Event::UtcOffset(offset) => self.utc_offset = Some(offset),
                Event::Settings(applied) => changes.push(Change::Settings(applied)),
                Event::Live(command) => changes.push(Change::Live(command)),
                Event::Management(status) => {
                    self.management = Some(status);
                    changes.push(Change::Updated);
                }
                Event::Daylight(up) => {
                    if self.daylight != Some(up) {
                        self.daylight = Some(up);
                        changes.push(Change::Theme);
                    }
                }
            }
        }
        changes
    }

    fn acknowledge(&mut self, entity_id: &str, result: Result<(), String>) {
        let acknowledgement = match result {
            Ok(()) => ActionAcknowledgement::Accepted,
            Err(reason) => ActionAcknowledgement::Rejected {
                reason: reason.chars().take(MAX_REASON_CHARS).collect(),
            },
        };
        let accepted = matches!(acknowledgement, ActionAcknowledgement::Accepted);
        if self.core.acknowledge(entity_id, acknowledgement).is_err() {
            return;
        }
        let requested = self.core.dashboard().entity(entity_id).is_some_and(|tile| {
            matches!(tile.action_feedback, Some(ActionFeedback::SceneRequested))
        });
        if accepted && requested {
            self.deadlines.insert(
                entity_id.to_owned(),
                (Instant::now() + SCENE_FEEDBACK_FOR, Deadline::ClearFeedback),
            );
        } else if accepted {
            self.deadlines.insert(
                entity_id.to_owned(),
                (Instant::now() + UNCONFIRMED_AFTER, Deadline::Unconfirmed),
            );
        } else {
            self.deadlines.remove(entity_id);
        }
    }

    /// Expires pending actions; returns whether anything on screen changed.
    pub(crate) fn tick(&mut self) -> bool {
        let now = Instant::now();
        let due: Vec<String> = self
            .deadlines
            .iter()
            .filter(|(_, (when, _))| *when <= now)
            .map(|(id, _)| id.clone())
            .collect();
        let mut changed = false;
        for entity_id in due {
            let Some((_, kind)) = self.deadlines.remove(&entity_id) else {
                continue;
            };
            changed |= match kind {
                Deadline::Unconfirmed => self.core.mark_unconfirmed(&entity_id).is_ok(),
                Deadline::ClearFeedback => self.core.clear_action_feedback(&entity_id).is_ok(),
            };
        }
        changed
    }

    pub(crate) fn tap(&mut self, entity_id: &str) -> Tap {
        match self.core.primary_tap(entity_id) {
            Ok(PrimaryTap::Inspect { entity_id, .. }) => Tap::Inspect(entity_id),
            Ok(tap) => self.send(tap),
            Err(error) => {
                log::warn!(target: "ui", "tap not sent: {error}");
                Tap::Ignored
            }
        }
    }

    /// Runs a details-sheet control such as a slider, a lock button or an option.
    pub(crate) fn detail_action(&mut self, entity_id: &str, key: &str, argument: &str) -> Tap {
        let Some(action) = self
            .core
            .dashboard()
            .entity(entity_id)
            .and_then(|tile| control_adapter::action(tile, key, argument))
        else {
            return Tap::Ignored;
        };
        match self.core.request_action(entity_id, &action) {
            Ok(tap) => self.send(tap),
            Err(error) => {
                log::warn!(target: "ui", "action {key} for {entity_id} not sent: {error}");
                Tap::Error(error.to_string())
            }
        }
    }

    fn send(&mut self, tap: PrimaryTap) -> Tap {
        let Some((entity_id, call)) = tap.into_call() else {
            return Tap::Ignored;
        };
        // A new request owns its own ACK/state deadlines; never inherit an earlier action's timer.
        self.deadlines.remove(&entity_id);
        let command = Command::CallService {
            entity_id: entity_id.clone(),
            call,
        };
        if self.commands.send(command).is_err() {
            self.deadlines.remove(&entity_id);
            return Tap::Ignored;
        }
        Tap::Sent
    }
}

pub(crate) fn room_id_string(id: &RoomId) -> String {
    match id {
        RoomId::Area(area_id) => area_id.clone(),
        RoomId::Unassigned => "unassigned".to_owned(),
    }
}

pub(crate) fn room_at(dashboard: &Dashboard, index: i32) -> Option<RoomId> {
    usize::try_from(index)
        .ok()
        .and_then(|index| dashboard.rooms.get(index))
        .map(|room| room.id.clone())
}

pub(crate) fn render(
    ui: &MainWindow,
    dashboard: &Dashboard,
    selected: Option<&RoomId>,
    details: Option<&str>,
    german: bool,
    dark_mode: bool,
    host: &str,
    utc_offset: Option<i32>,
    camera: &CameraView,
    problem: Option<ConnectionProblem>,
    management: Option<&management::Status>,
    settings: &Settings,
    rooms_page: bool,
) {
    let rooms: Vec<RoomItem> = dashboard
        .rooms
        .iter()
        .map(|room| {
            let id = room_id_string(&room.id);
            RoomItem {
                id: id.as_str().into(),
                icon: settings.room_icon(&id, &room.name).into(),
                name: settings.room_name(&id, &room.name).into(),
                summary: room_summary(room, german).into(),
            }
        })
        .collect();
    let selected_index = selected
        .and_then(|id| dashboard.rooms.iter().position(|room| &room.id == id))
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(-1);
    let tiles: Vec<TileItem> = usize::try_from(selected_index)
        .ok()
        .and_then(|index| dashboard.rooms.get(index))
        .into_iter()
        .flat_map(|room| &room.entities)
        .map(|tile| tile_item(tile, german, settings))
        .collect();
    let favorites: Vec<TileItem> = settings
        .favorites
        .iter()
        .filter_map(|entity_id| dashboard.entity(entity_id))
        .map(|tile| tile_item(tile, german, settings))
        .collect();
    ui.set_favorites(ModelRc::new(VecModel::from(favorites)));
    ui.set_home_view(settings.home_view.as_str().into());
    ui.set_color_scheme(settings.effective_color_scheme(dark_mode).into());
    ui.set_increased_contrast(settings.effective_increased_contrast());
    ui.set_reduced_motion(settings.reduced_motion);
    ui.set_rooms_page(rooms_page);
    ui.set_details_fullscreen(settings.details == "fullscreen");
    ui.set_rooms(ModelRc::new(VecModel::from(rooms)));
    ui.set_tiles(ModelRc::new(VecModel::from(tiles)));
    ui.set_selected_room(selected_index);
    ui.set_connection_ready(dashboard.connection.is_ready());
    let mut label = connection_label(dashboard.connection, host, german, problem);
    if dashboard.connection.is_ready() {
        if let Some(text) = management.and_then(|status| management_label(status, german)) {
            label = text;
        }
    }
    ui.set_management_degraded(
        management.is_some_and(|status| *status != management::Status::Connected),
    );
    let (stage, code) = match management {
        Some(management::Status::Waiting(code)) => ("approve", spaced_code(code)),
        Some(management::Status::Conflict) => ("conflict", String::new()),
        _ => ("", String::new()),
    };
    // Orientation and theme belong to Home Assistant once the tablet is paired.
    ui.set_settings_managed(matches!(
        management,
        Some(management::Status::Connected | management::Status::Disconnected)
    ));
    ui.set_pairing_stage(stage.into());
    ui.set_pairing_code(code.into());
    ui.set_connection_text(label.into());
    ui.set_boot_screen(false);
    ui.set_german(german);
    let detail_tile = details.and_then(|id| dashboard.entity(id));
    let previous = ui.get_details_data().entity_id;
    if previous.as_str() != detail_tile.map_or("", |tile| tile.entity_id.as_str()) {
        ui.set_editing_controls(false);
        ui.set_entity_error("".into());
        ui.set_entity_results(ModelRc::default());
        ui.set_showing_results(false);
        ui.set_entity_result_message("".into());
    }
    ui.set_entity_image(camera.image.cloned().unwrap_or_default());
    ui.set_details_data(detail_tile.map_or_else(empty_details, |tile| {
        detail_item(tile, settings, german, utc_offset, camera)
    }));
    ui.set_details_open(detail_tile.is_some());
    control_adapter::render(ui, detail_tile, german);
}

/// What the status line says about the Njörun integration; `None` when all is well.
fn management_label(status: &management::Status, german: bool) -> Option<String> {
    use management::Status;
    Some(match (status, german) {
        // Approval and ID conflicts are shown full screen, not in the status line.
        (Status::Connected | Status::Waiting(_) | Status::Conflict, _) => return None,
        (Status::NotInstalled, false) => {
            "The Njörun integration is not installed in Home Assistant".to_owned()
        }
        (Status::NotInstalled, true) => {
            "Die Njörun-Integration ist in Home Assistant nicht installiert".to_owned()
        }
        (Status::Disconnected, false) => "Njörun management is reconnecting…".to_owned(),
        (Status::Disconnected, true) => "Njörun-Verwaltung verbindet sich neu …".to_owned(),
    })
}

fn connection_label(
    connection: ConnectionStatus,
    host: &str,
    german: bool,
    problem: Option<ConnectionProblem>,
) -> String {
    if connection == ConnectionStatus::Offline {
        if let Some(problem) = problem {
            return problem.message(host, german);
        }
    }
    match (connection, german) {
        (ConnectionStatus::Ready, true) => format!("Verbunden mit {host}"),
        (ConnectionStatus::Ready, false) => format!("Connected to {host}"),
        (ConnectionStatus::Synchronizing, true) => {
            "Home Assistant wird synchronisiert …".to_owned()
        }
        (ConnectionStatus::Synchronizing, false) => "Synchronizing with Home Assistant…".to_owned(),
        (ConnectionStatus::Offline, true) => {
            "Home Assistant offline · Verbindung wird wiederhergestellt …".to_owned()
        }
        (ConnectionStatus::Offline, false) => "Home Assistant offline · Reconnecting…".to_owned(),
    }
}

fn tile_item(tile: &EntityTile, german: bool, settings: &Settings) -> TileItem {
    let feedback = tile.action_feedback.as_ref();
    let value = feedback_value(feedback, tile, german).unwrap_or_else(|| state_value(tile, german));
    TileItem {
        kind: tile_kind(tile).into(),
        action_enabled: tile.can_primary_action()
            && (tile.domain != njorun_app_core::EntityDomain::MediaPlayer
                || njorun_app_core::media::controls(tile, german)
                    .iter()
                    .any(|control| {
                        matches!(control.key, "media_play" | "media_pause") && control.enabled
                    })),
        entity_id: tile.entity_id.clone().into(),
        name: settings.entity_name(&tile.entity_id, &tile.name).into(),
        value: value.into(),
        icon: icon(presentation::entity_icon(tile)),
        active: presentation::tile_active(tile),
        pending: matches!(
            feedback,
            Some(
                ActionFeedback::AwaitingAcknowledgement { .. }
                    | ActionFeedback::AcceptedAwaitingState { .. }
                    | ActionFeedback::AcceptedAwaitingConfirmation { .. }
            )
        ),
        failed: matches!(
            feedback,
            Some(ActionFeedback::Rejected { .. } | ActionFeedback::Unconfirmed { .. })
        ),
        unavailable: !tile.is_available(),
        stale: tile.is_stale,
        command_enabled: tile.command_enabled,
    }
}

fn tile_kind(tile: &EntityTile) -> &'static str {
    use njorun_app_core::EntityDomain;

    match tile.domain {
        EntityDomain::Light
        | EntityDomain::Switch
        | EntityDomain::Fan
        | EntityDomain::InputBoolean
        | EntityDomain::Automation => "control",
        EntityDomain::Scene
        | EntityDomain::Script
        | EntityDomain::Button
        | EntityDomain::InputButton => "scene",
        EntityDomain::Sensor => "measurement",
        EntityDomain::MediaPlayer => "media",
        _ => "status",
    }
}

fn icon(kind: IconKind) -> EntityIcon {
    match kind {
        IconKind::Lightbulb => EntityIcon::Lightbulb,
        IconKind::Power => EntityIcon::Power,
        IconKind::Play => EntityIcon::Play,
        IconKind::Thermometer => EntityIcon::Thermometer,
        IconKind::Sensor => EntityIcon::Sensor,
        IconKind::BinarySensor => EntityIcon::BinarySensor,
        IconKind::Fan => EntityIcon::Fan,
        IconKind::Lock => EntityIcon::Lock,
        IconKind::LockOpen => EntityIcon::LockOpen,
        IconKind::MediaPlayer => EntityIcon::MediaPlayer,
        IconKind::Camera => EntityIcon::Camera,
        IconKind::Shield => EntityIcon::Shield,
        IconKind::Script => EntityIcon::Script,
        IconKind::Automation => EntityIcon::Automation,
        IconKind::ToggleSwitch => EntityIcon::ToggleSwitch,
        IconKind::Button => EntityIcon::Button,
        IconKind::Number => EntityIcon::Number,
        IconKind::Select => EntityIcon::Select,
        IconKind::Cover => EntityIcon::Cover,
        IconKind::Climate => EntityIcon::Climate,
        IconKind::Water => EntityIcon::Water,
        IconKind::Robot => EntityIcon::Robot,
        IconKind::Calendar => EntityIcon::Calendar,
        IconKind::Weather => EntityIcon::Weather,
        IconKind::Presence => EntityIcon::Presence,
        IconKind::Timer => EntityIcon::Timer,
        IconKind::Update => EntityIcon::Update,
        IconKind::Voice => EntityIcon::Voice,
        IconKind::Remote => EntityIcon::Remote,
        IconKind::Text => EntityIcon::Text,
    }
}

/// The camera picture to show in the details sheet while one is open.
pub(crate) struct CameraView<'a> {
    pub image: Option<&'a slint::Image>,
    pub loading: bool,
}

fn detail_item(
    tile: &EntityTile,
    settings: &Settings,
    german: bool,
    utc_offset: Option<i32>,
    camera: &CameraView,
) -> DetailsItem {
    let reason = match tile.action_feedback.as_ref() {
        Some(ActionFeedback::Rejected { reason }) => reason.clone(),
        _ => String::new(),
    };
    let update_time = tile
        .state
        .as_ref()
        .and_then(|state| state.last_updated.as_deref())
        .map(|value| match utc_offset {
            Some(offset) => format_local_timestamp(value, offset, german),
            None => format_local_timestamp(value, 0, german) + " UTC",
        })
        .unwrap_or_default();
    let view = detail_view(tile, german);
    let slider = view.slider.clone().unwrap_or(Slider {
        action: "",
        min: 0.0,
        max: 100.0,
        step: 1.0,
        value: 0.0,
        unit: String::new(),
    });
    #[allow(clippy::cast_possible_truncation)]
    DetailsItem {
        entity_id: tile.entity_id.clone().into(),
        name: settings.entity_name(&tile.entity_id, &tile.name).into(),
        value: feedback_value(tile.action_feedback.as_ref(), tile, german)
            .unwrap_or_else(|| detail_value(tile, german))
            .into(),
        update_time: update_time.into(),
        reason: reason.into(),
        attributes: detail_attributes(tile, german).into(),
        icon: icon(presentation::entity_icon(tile)),
        unavailable: !tile.is_available(),
        can_toggle: tile.domain.tap_behavior() == TapBehavior::Toggle,
        controls_enabled: tile.command_enabled
            || (tile.domain == EntityDomain::Camera && tile.is_available() && !tile.is_stale),
        control: view.control.into(),
        slider_action: slider.action.into(),
        slider_min: slider.min as f32,
        slider_max: slider.max as f32,
        slider_step: slider.step as f32,
        slider_value: slider.value as f32,
        slider_unit: slider.unit.into(),
        options: ModelRc::new(VecModel::from(
            view.options
                .into_iter()
                .map(SharedString::from)
                .collect::<Vec<_>>(),
        )),
        selected_option: view.selected_option.into(),
        locked: view.locked,
        playing: view.playing,
        has_volume: view.has_volume,
        needs_code: view.needs_code,
        actions: ModelRc::new(VecModel::from(
            view.actions
                .into_iter()
                .map(|(key, label)| ActionItem {
                    key: key.into(),
                    label: label.into(),
                })
                .collect::<Vec<_>>(),
        )),
        camera_image: camera.image.cloned().unwrap_or_default(),
        camera_loading: camera.loading,
    }
}

fn empty_details() -> DetailsItem {
    DetailsItem {
        entity_id: "".into(),
        name: "".into(),
        value: "".into(),
        update_time: "".into(),
        reason: "".into(),
        attributes: "".into(),
        icon: EntityIcon::Lightbulb,
        unavailable: true,
        can_toggle: false,
        controls_enabled: false,
        control: "none".into(),
        slider_action: "".into(),
        slider_min: 0.0,
        slider_max: 100.0,
        slider_step: 1.0,
        slider_value: 0.0,
        slider_unit: "".into(),
        options: ModelRc::default(),
        selected_option: "".into(),
        locked: false,
        playing: false,
        has_volume: false,
        needs_code: false,
        actions: ModelRc::default(),
        camera_image: slint::Image::default(),
        camera_loading: false,
    }
}
