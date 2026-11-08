//! Home dashboard state on the device: applies Home Assistant session events to the shared
//! `AppCore`, turns taps into service calls and renders rooms and tiles into Slint.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use njorun_app_core::presentation::{
    self, detail_attributes, detail_view, feedback_value, format_local_timestamp, room_summary,
    state_value, IconKind, Slider,
};
use njorun_app_core::{
    ActionAcknowledgement, ActionFeedback, AppCore, ConnectionStatus, Dashboard, DashboardOrder,
    DetailAction, EntityTile, PrimaryTap, RoomId,
};
use slint::{ModelRc, SharedString, VecModel};

use njorun_app_core::problems::ConnectionProblem;

use crate::generated::{ActionItem, DetailsItem, EntityIcon, MainWindow, RoomItem, TileItem};
use crate::ha::{self, CameraFrame, Command, Event};
use crate::store::HomeAssistantSession;

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
    Updated,
    Rejected,
    DataError,
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
}

pub(crate) enum Tap {
    Inspect(String),
    Sent,
    Ignored,
}

pub(crate) struct Session {
    core: AppCore,
    events: Receiver<Event>,
    commands: Sender<Command>,
    deadlines: HashMap<String, (Instant, Deadline)>,
    seen_snapshot: bool,
    utc_offset: Option<i32>,
    /// Why the last connection attempt failed, until the next snapshot.
    problem: Option<ConnectionProblem>,
    daylight: Option<bool>,
}

impl Session {
    pub(crate) fn start(
        session: HomeAssistantSession,
        access_token: Option<String>,
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
            utc_offset: None,
            problem: None,
            daylight: None,
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

    /// Whether the sun is up in Home Assistant; `None` until known.
    pub(crate) const fn daylight(&self) -> Option<bool> {
        self.daylight
    }

    pub(crate) const fn problem(&self) -> Option<ConnectionProblem> {
        self.problem
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
                Event::Snapshot(snapshot) => {
                    self.problem = None;
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
                        &DashboardOrder::default(),
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
                            println!("Njörun D1001 dashboard rejected the snapshot: {error}");
                            self.core.set_connection(ConnectionStatus::Offline);
                            changes.push(Change::DataError);
                        }
                    }
                }
                Event::State(state) => match self.core.apply_state(state) {
                    Ok(()) => changes.push(Change::Updated),
                    Err(error) => println!("Njörun D1001 ignored a state update: {error}"),
                },
                Event::ServiceResult { entity_id, result } => {
                    self.acknowledge(&entity_id, result);
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
        let scene = entity_id.starts_with("scene.");
        let accepted = matches!(acknowledgement, ActionAcknowledgement::Accepted);
        if self.core.acknowledge(entity_id, acknowledgement).is_err() {
            return;
        }
        if accepted && scene {
            self.deadlines.insert(
                entity_id.to_owned(),
                (Instant::now() + SCENE_FEEDBACK_FOR, Deadline::ClearFeedback),
            );
        } else if !accepted {
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
                println!("Njörun D1001 tap not sent: {error}");
                Tap::Ignored
            }
        }
    }

    /// Runs a details-sheet control such as a slider, a lock button or an option.
    pub(crate) fn detail_action(&mut self, entity_id: &str, key: &str, argument: &str) -> Tap {
        let Some(domain) = self
            .core
            .dashboard()
            .entity(entity_id)
            .map(|tile| tile.domain)
        else {
            return Tap::Ignored;
        };
        let Some(action) = DetailAction::from_ui(key, argument, domain) else {
            return Tap::Ignored;
        };
        match self.core.request_action(entity_id, &action) {
            Ok(tap) => self.send(tap),
            Err(error) => {
                println!("Njörun D1001 action {key} for {entity_id} not sent: {error}");
                Tap::Ignored
            }
        }
    }

    fn send(&mut self, tap: PrimaryTap) -> Tap {
        let Some((entity_id, call)) = tap.into_call() else {
            return Tap::Ignored;
        };
        self.deadlines.insert(
            entity_id.clone(),
            (Instant::now() + UNCONFIRMED_AFTER, Deadline::Unconfirmed),
        );
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
    host: &str,
    utc_offset: Option<i32>,
    camera: &CameraView,
    problem: Option<ConnectionProblem>,
) {
    let rooms: Vec<RoomItem> = dashboard
        .rooms
        .iter()
        .map(|room| RoomItem {
            id: room_id_string(&room.id).into(),
            name: room.name.clone().into(),
            summary: room_summary(room, german).into(),
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
        .map(|tile| tile_item(tile, german))
        .collect();
    ui.set_rooms(ModelRc::new(VecModel::from(rooms)));
    ui.set_tiles(ModelRc::new(VecModel::from(tiles)));
    ui.set_selected_room(selected_index);
    ui.set_connection_ready(dashboard.connection.is_ready());
    ui.set_connection_text(connection_label(dashboard.connection, host, german, problem).into());
    ui.set_boot_screen(false);
    ui.set_german(german);
    let detail_tile = details.and_then(|id| dashboard.entity(id));
    ui.set_details_data(detail_tile.map_or_else(empty_details, |tile| {
        detail_item(tile, german, utc_offset, camera)
    }));
    ui.set_details_open(detail_tile.is_some());
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

fn tile_item(tile: &EntityTile, german: bool) -> TileItem {
    let feedback = tile.action_feedback.as_ref();
    let value = feedback_value(feedback, tile, german).unwrap_or_else(|| state_value(tile, german));
    TileItem {
        entity_id: tile.entity_id.clone().into(),
        name: tile.name.clone().into(),
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
    }
}

/// The camera picture to show in the details sheet while one is open.
pub(crate) struct CameraView<'a> {
    pub image: Option<&'a slint::Image>,
    pub loading: bool,
}

fn detail_item(
    tile: &EntityTile,
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
        name: tile.name.clone().into(),
        value: feedback_value(tile.action_feedback.as_ref(), tile, german)
            .unwrap_or_else(|| state_value(tile, german))
            .into(),
        update_time: update_time.into(),
        reason: reason.into(),
        attributes: detail_attributes(tile, german).into(),
        icon: icon(presentation::entity_icon(tile)),
        unavailable: !tile.is_available(),
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
