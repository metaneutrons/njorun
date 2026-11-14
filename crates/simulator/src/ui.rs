//! Native Slint presentation for the simulator runtime.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local};
use njorun_app_core::display::{DisplayController, DisplayState, TouchDisposition};
use njorun_app_core::presentation::{
    self, IconKind, detail_attributes, feedback_value, room_summary, state_value,
};
use njorun_app_core::{
    ActionFeedback, ConnectionStatus, Dashboard, DetailAction, EntityTile, RoomId, TapBehavior,
    settings::AppliedSettings,
};
use slint::{
    ComponentHandle, LogicalSize, Model, ModelRc, PhysicalSize, SharedString, Timer, TimerMode,
    VecModel,
};

use crate::{RuntimeCommand, RuntimeEvent};

#[allow(unsafe_code)]
mod generated {
    slint::include_modules!();
}

use generated::{
    ActionItem, ControlItem, DetailsItem, EntityIcon, InputItem, MainWindow, ResultItem, RoomItem,
    TileItem,
};

#[path = "control_adapter.rs"]
mod control_adapter;
#[cfg(feature = "qualification")]
use generated::{CommissioningWindow, InstanceItem, WifiItem};

#[derive(Clone, Copy)]
enum InitialSize {
    Logical(LogicalSize),
    Physical(PhysicalSize),
}

impl From<LogicalSize> for InitialSize {
    fn from(value: LogicalSize) -> Self {
        Self::Logical(value)
    }
}

impl From<PhysicalSize> for InitialSize {
    fn from(value: PhysicalSize) -> Self {
        Self::Physical(value)
    }
}

impl InitialSize {
    fn apply(self, window: &slint::Window, portrait: bool) {
        match self {
            Self::Logical(size) => {
                let (mut width, mut height) = (size.width, size.height);
                if (portrait && width > height) || (!portrait && height > width) {
                    std::mem::swap(&mut width, &mut height);
                }
                window.set_size(LogicalSize::new(width, height));
            }
            Self::Physical(size) => {
                let (mut width, mut height) = (size.width, size.height);
                if (portrait && width > height) || (!portrait && height > width) {
                    std::mem::swap(&mut width, &mut height);
                }
                window.set_size(PhysicalSize::new(width, height));
            }
        }
    }
}

#[cfg(feature = "qualification")]
#[path = "../tests/support/ui_qualification.rs"]
pub mod qualification;

#[cfg(feature = "qualification")]
pub use qualification::{
    LiveQualificationOptions, LiveQualificationResult, PreviewError, PreviewFrame, PreviewPixel,
    qualify_live_ha_ui, qualify_ui_interactions, render_qualification_previews,
};

#[derive(Clone)]
struct ViewState {
    dashboard: Option<Dashboard>,
    settings: AppliedSettings,
    daylight: bool,
    selected_room: Option<RoomId>,
    selected_entity: Option<String>,
    rooms_page: bool,
    runtime_status: String,
    omitted_entities: usize,
    image_request: Option<(String, String, Instant)>,
    image_last: Option<(String, String, Instant)>,
    entity_image: slint::Image,
    image_status: String,
    pending_settings_acks: VecDeque<u64>,
    display_controller: DisplayController,
    display: DisplayState,
    started_at: Instant,
    clock_label: String,
    clock_minute: Option<u64>,
}

impl ViewState {
    fn new() -> Self {
        Self::with_settings(AppliedSettings::default())
    }

    fn with_settings(settings: AppliedSettings) -> Self {
        let started_at = Instant::now();
        let display_controller = DisplayController::new(0);
        let display = display_controller.state(0, &settings.settings);
        let german = settings.settings.language == "de";
        Self {
            dashboard: None,
            settings,
            daylight: true,
            selected_room: None,
            selected_entity: None,
            rooms_page: false,
            runtime_status: String::new(),
            omitted_entities: 0,
            image_request: None,
            image_last: None,
            entity_image: slint::Image::default(),
            image_status: String::new(),
            pending_settings_acks: VecDeque::new(),
            display_controller,
            display,
            started_at,
            clock_label: local_clock_label(german),
            clock_minute: None,
        }
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(
            Instant::now()
                .saturating_duration_since(self.started_at)
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    }

    fn update_display(&mut self) -> bool {
        let now = self.now_ms();
        let previous = self.display;
        let previous_clock = self.clock_label.clone();
        self.display = self.display_controller.state(now, &self.settings.settings);
        let minute = unix_minute();
        if self.clock_minute != Some(minute) {
            self.clock_label = local_clock_label(self.settings.settings.language == "de");
            self.clock_minute = Some(minute);
        }
        previous != self.display || previous_clock != self.clock_label
    }

    fn touch(&mut self) -> TouchDisposition {
        let now = self.now_ms();
        let disposition = self.display_controller.touch(now, &self.settings.settings);
        self.update_display();
        disposition
    }

    fn record_activity(&mut self) -> bool {
        self.display_controller.activity(self.now_ms());
        self.update_display()
    }
}

/// Run the native panel UI and apply events from the simulator runtime on the UI thread.
///
/// The UI owns only presentation state. It sends tap intents to the runtime and acknowledges
/// settings only after updating its properties and the window orientation.
///
/// # Errors
///
/// Returns a Slint platform error if the window cannot be created or its event loop fails.
pub fn run(
    events: Receiver<RuntimeEvent>,
    commands: SyncSender<RuntimeCommand>,
    width: u32,
    height: u32,
) -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;
    let initial_size = if crate::physical_preview_requested() {
        InitialSize::Physical(PhysicalSize::new(width, height))
    } else {
        InitialSize::Logical(crate::logical_size(width, height))
    };
    initial_size.apply(ui.window(), width <= height);

    let state = Rc::new(RefCell::new(ViewState::new()));
    render(&ui, &state.borrow());
    let timer_commands = commands.clone();
    install_callbacks(&ui, &state, commands);
    let timer = start_event_timer(&ui, state, events, timer_commands, initial_size);

    let (link_tx, link_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        loop {
            if link_tx
                .send(crate::network::sample().unwrap_or(""))
                .is_err()
                || stop_rx.recv_timeout(Duration::from_secs(15))
                    != Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            {
                return;
            }
        }
    });
    let link_timer = Timer::default();
    let weak = ui.as_weak();
    link_timer.start(TimerMode::Repeated, Duration::from_millis(500), move || {
        if let (Ok(link), Some(ui)) = (link_rx.try_recv(), weak.upgrade()) {
            ui.set_network_link(link.into());
        }
    });
    let result = ui.run();
    drop(stop_tx);
    timer.stop();
    result
}

fn install_callbacks(
    ui: &MainWindow,
    state: &Rc<RefCell<ViewState>>,
    commands: SyncSender<RuntimeCommand>,
) {
    install_entity_callbacks(ui, state, commands);
    install_navigation_callbacks(ui, state);
    install_display_callbacks(ui, state);
    install_details_callback(ui, state);
}

#[allow(clippy::too_many_lines)] // Keep the UI event/property mapping together.
fn install_entity_callbacks(
    ui: &MainWindow,
    state: &Rc<RefCell<ViewState>>,
    commands: SyncSender<RuntimeCommand>,
) {
    let action_commands = commands.clone();
    let weak = ui.as_weak();
    let state_for_activation = Rc::clone(state);
    ui.on_activate_entity(move |entity_id| {
        if state_for_activation.borrow_mut().touch() == TouchDisposition::WakeOnly {
            if let Some(ui) = weak.upgrade() {
                render(&ui, &state_for_activation.borrow());
            }
            return;
        }
        let entity_id_ref = entity_id.as_ref();
        let should_inspect = state_for_activation
            .borrow()
            .dashboard
            .as_ref()
            .and_then(|dashboard| dashboard.entity(entity_id_ref))
            .is_some_and(|tile| {
                tile.domain.tap_behavior() == TapBehavior::Inspect || !tile.command_enabled
            });
        if should_inspect {
            open_details(&weak, &state_for_activation, entity_id_ref);
        } else {
            match commands.try_send(RuntimeCommand::Tap(entity_id_ref.to_owned())) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    show_command_enqueue_failure(&weak, &state_for_activation, true);
                }
                Err(TrySendError::Disconnected(_)) => {
                    show_command_enqueue_failure(&weak, &state_for_activation, false);
                }
            }
        }
    });

    let key_window = ui.as_weak();
    ui.on_virtual_key(move |key| {
        if let Some(ui) = key_window.upgrade() {
            control_adapter::virtual_key(&ui, key.as_str());
        }
    });
    let form_commands = action_commands.clone();
    let form_window = ui.as_weak();
    ui.on_control_submit(move |entity_id, key, fields| {
        match control_adapter::form_argument(&fields) {
            Ok(argument) => {
                let result = form_commands.try_send(RuntimeCommand::Action {
                    entity_id: entity_id.to_string(),
                    action: DetailAction::Entity {
                        key: key.to_string(),
                        argument,
                    },
                });
                if let Some(ui) = form_window.upgrade() {
                    ui.set_entity_error(
                        if result.is_err() {
                            "Could not send the action"
                        } else {
                            ""
                        }
                        .into(),
                    );
                }
            }
            Err(error) => {
                if let Some(ui) = form_window.upgrade() {
                    ui.set_entity_error(error.into());
                }
            }
        }
    });

    let weak = ui.as_weak();
    let state_for_action = Rc::clone(state);
    ui.on_detail_action(move |entity_id, key, argument| {
        let action = state_for_action
            .borrow()
            .dashboard
            .as_ref()
            .and_then(|dashboard| dashboard.entity(entity_id.as_ref()))
            .and_then(|tile| control_adapter::action(tile, key.as_ref(), argument.as_ref()));
        let Some(action) = action else {
            return;
        };
        if action_commands
            .try_send(RuntimeCommand::Action {
                entity_id: entity_id.to_string(),
                action,
            })
            .is_err()
        {
            show_command_enqueue_failure(&weak, &state_for_action, true);
        }
    });

    let weak = ui.as_weak();
    let state_for_details = Rc::clone(state);
    ui.on_request_details(move |entity_id| {
        if state_for_details.borrow_mut().touch() == TouchDisposition::WakeOnly {
            if let Some(ui) = weak.upgrade() {
                render(&ui, &state_for_details.borrow());
            }
            return;
        }
        open_details(&weak, &state_for_details, entity_id.as_ref());
    });
}

fn install_navigation_callbacks(ui: &MainWindow, state: &Rc<RefCell<ViewState>>) {
    let weak = ui.as_weak();
    let state_for_room = Rc::clone(state);
    ui.on_room_selected(move |index| {
        let mut state = state_for_room.borrow_mut();
        if state.touch() == TouchDisposition::WakeOnly {
            render_if_alive(&weak, &state);
            return;
        }
        state.selected_room = usize::try_from(index)
            .ok()
            .and_then(|index| state.dashboard.as_ref()?.rooms.get(index))
            .map(|room| room.id.clone());
        state.selected_entity = None;
        if let Some(ui) = weak.upgrade() {
            render(&ui, &state);
        }
    });

    let weak = ui.as_weak();
    let state_for_rooms = Rc::clone(state);
    ui.on_show_rooms(move || {
        let mut state = state_for_rooms.borrow_mut();
        if state.touch() == TouchDisposition::WakeOnly {
            render_if_alive(&weak, &state);
            return;
        }
        state.rooms_page = true;
        state.selected_room = None;
        state.selected_entity = None;
        render_if_alive(&weak, &state);
    });

    let weak = ui.as_weak();
    let state_for_home = Rc::clone(state);
    ui.on_go_home(move || {
        let mut state = state_for_home.borrow_mut();
        if state.touch() == TouchDisposition::WakeOnly {
            render_if_alive(&weak, &state);
            return;
        }
        state.rooms_page = false;
        state.selected_room = None;
        state.selected_entity = None;
        render_if_alive(&weak, &state);
    });
}

fn install_display_callbacks(ui: &MainWindow, state: &Rc<RefCell<ViewState>>) {
    let weak = ui.as_weak();
    let state_for_activity = Rc::clone(state);
    ui.on_activity(move || {
        let changed = state_for_activity.borrow_mut().record_activity();
        if changed && let Some(ui) = weak.upgrade() {
            render(&ui, &state_for_activity.borrow());
        }
    });

    let weak = ui.as_weak();
    let state_for_wake = Rc::clone(state);
    ui.on_wake_display(move || {
        let mut state = state_for_wake.borrow_mut();
        let _ = state.touch();
        render_if_alive(&weak, &state);
    });
}

fn install_details_callback(ui: &MainWindow, state: &Rc<RefCell<ViewState>>) {
    let weak = ui.as_weak();
    let state_for_dismiss = Rc::clone(state);
    ui.on_dismiss_details(move || {
        let mut state = state_for_dismiss.borrow_mut();
        if state.touch() == TouchDisposition::WakeOnly {
            render_if_alive(&weak, &state);
            return;
        }
        state.selected_entity = None;
        if let Some(ui) = weak.upgrade() {
            render(&ui, &state);
        }
    });
}

fn render_if_alive(ui: &slint::Weak<MainWindow>, state: &ViewState) {
    if let Some(ui) = ui.upgrade() {
        render(&ui, state);
    }
}

fn show_command_enqueue_failure(
    ui: &slint::Weak<MainWindow>,
    state: &Rc<RefCell<ViewState>>,
    queue_full: bool,
) {
    if let Some(ui) = ui.upgrade() {
        let state = state.borrow();
        let message = match (queue_full, state.settings.settings.language.as_str()) {
            (true, "de") => "Panel ausgelastet · Erneut tippen",
            (true, _) => "Panel is busy · Tap again",
            (false, "de") => "Befehle nicht verfügbar",
            (false, _) => "Commands unavailable",
        };
        ui.set_connection_text(message.into());
        ui.set_command_warning(true);
    }
}

fn start_event_timer(
    ui: &MainWindow,
    state: Rc<RefCell<ViewState>>,
    events: Receiver<RuntimeEvent>,
    commands: SyncSender<RuntimeCommand>,
    initial_size: impl Into<InitialSize>,
) -> Timer {
    let initial_size = initial_size.into();
    let weak = ui.as_weak();
    let timer = Timer::default();
    let mut detail_tick = Instant::now();
    timer.start(TimerMode::Repeated, Duration::from_millis(40), move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if detail_tick.elapsed() >= Duration::from_secs(1) {
            detail_tick = Instant::now();
            let current = state.borrow();
            let tile = current
                .selected_entity
                .as_ref()
                .and_then(|id| current.dashboard.as_ref()?.entity(id));
            control_adapter::render_media(&ui, tile);
            ui.set_image_status(image_status(&current).into());
        }
        if state.borrow_mut().update_display() {
            render(&ui, &state.borrow());
        }
        update_entity_image(&ui, &state, &commands);
        retry_settings_acknowledgements(&commands, &state);
        for _ in 0..64 {
            let event = match events.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    mark_runtime_disconnected(&ui, &state);
                    break;
                }
            };
            apply_runtime_event(&ui, &state, initial_size, event);
        }
        retry_settings_acknowledgements(&commands, &state);
    });
    timer
}

fn mark_runtime_disconnected(ui: &MainWindow, state: &Rc<RefCell<ViewState>>) {
    let mut state = state.borrow_mut();
    localized(&state, "Runtime disconnected", "Runtime getrennt")
        .clone_into(&mut state.runtime_status);
    render(ui, &state);
}

#[allow(clippy::too_many_lines)] // Keep the UI event/property mapping together.
fn apply_runtime_event(
    ui: &MainWindow,
    state: &Rc<RefCell<ViewState>>,
    initial_size: impl Into<InitialSize>,
    event: RuntimeEvent,
) {
    let initial_size = initial_size.into();
    match event {
        RuntimeEvent::EntityLimit(count) => {
            state.borrow_mut().omitted_entities = count;
            render(ui, &state.borrow());
        }
        RuntimeEvent::Image {
            entity_id,
            path,
            result,
        } => {
            let mut state = state.borrow_mut();
            if state
                .image_request
                .as_ref()
                .is_some_and(|(id, requested_path, _)| id == &entity_id && requested_path == &path)
            {
                state.image_request = None;
                state.image_last = Some((entity_id.clone(), path, Instant::now()));
                if state.selected_entity.as_deref() == Some(entity_id.as_str()) {
                    if let Ok(image) = result.and_then(|data| {
                        slint::Image::load_from_data(&data, None)
                            .map_err(|_| "Image could not be decoded".into())
                    }) {
                        state.entity_image = image;
                        state.image_status.clear();
                    } else {
                        state.entity_image = slint::Image::default();
                        state.image_status = localized(
                            &state,
                            "Image unavailable · Retrying",
                            "Bild nicht verfügbar · Erneuter Versuch",
                        )
                        .into();
                    }
                    render(ui, &state);
                }
            }
        }
        RuntimeEvent::ActionResult { entity_id, value } => {
            if state.borrow().selected_entity.as_deref() == Some(entity_id.as_str()) {
                control_adapter::response(ui, &value);
            }
        }
        RuntimeEvent::ActionError { entity_id, message } => {
            if state.borrow().selected_entity.as_deref() == Some(entity_id.as_str()) {
                ui.set_entity_error(message.into());
            }
        }
        RuntimeEvent::Dashboard(dashboard) => {
            let mut state = state.borrow_mut();
            state.dashboard = Some(dashboard);
            if state.selected_room.as_ref().is_some_and(|selected| {
                !state.dashboard.as_ref().is_some_and(|dashboard| {
                    dashboard.rooms.iter().any(|room| &room.id == selected)
                })
            }) {
                state.selected_room = None;
            }
            if state.selected_entity.as_ref().is_some_and(|selected| {
                state
                    .dashboard
                    .as_ref()
                    .is_none_or(|dashboard| dashboard.entity(selected).is_none())
            }) {
                state.selected_entity = None;
            }
            render(ui, &state);
        }
        RuntimeEvent::Settings(settings) => {
            let mut state = state.borrow_mut();
            let settings = *settings;
            let home_view_changed =
                state.settings.settings.home_view != settings.settings.home_view;
            state.settings = settings;
            if home_view_changed {
                state.selected_room = None;
                state.selected_entity = None;
                state.rooms_page = false;
            } else if state.settings.settings.home_view != "favorites" {
                state.rooms_page = false;
            }
            state.clock_minute = None;
            state.update_display();
            apply_settings(ui, &state, initial_size);
            render(ui, &state);
            // The runtime waits for this before publishing the applied config to HA.
            let revision = state.settings.revision;
            state.pending_settings_acks.push_back(revision);
        }
        RuntimeEvent::Daylight(is_daylight) => {
            let mut state = state.borrow_mut();
            state.daylight = is_daylight;
            apply_settings(ui, &state, initial_size);
            render(ui, &state);
        }
        RuntimeEvent::Status(status) => {
            let mut state = state.borrow_mut();
            state.runtime_status = status;
            render(ui, &state);
        }
    }
}

fn image_status(state: &ViewState) -> String {
    if !state.image_status.is_empty() {
        return state.image_status.clone();
    }
    if let Some((id, _, received)) = &state.image_last
        && state.selected_entity.as_deref() == Some(id.as_str())
        && state.entity_image.size().width > 0
    {
        return if state.settings.settings.language == "de" {
            format!("Bild vor {} s aktualisiert", received.elapsed().as_secs())
        } else {
            format!("Image updated {}s ago", received.elapsed().as_secs())
        };
    }
    String::new()
}

fn update_entity_image(
    ui: &MainWindow,
    state: &Rc<RefCell<ViewState>>,
    commands: &SyncSender<RuntimeCommand>,
) {
    let mut state = state.borrow_mut();
    let desired = state
        .selected_entity
        .as_ref()
        .and_then(|id| state.dashboard.as_ref()?.entity(id))
        .filter(|tile| tile.is_available() && !tile.is_stale)
        .and_then(|tile| {
            let path = match tile.domain {
                njorun_app_core::EntityDomain::Camera => {
                    Some(format!("/api/camera_proxy/{}", tile.entity_id))
                }
                njorun_app_core::EntityDomain::Image => {
                    Some(format!("/api/image_proxy/{}", tile.entity_id))
                }
                njorun_app_core::EntityDomain::MediaPlayer => {
                    njorun_app_core::controls::attribute(tile, "entity_picture")
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| !value.is_empty())
                        .map(|_| format!("/api/media_player_proxy/{}", tile.entity_id))
                }
                _ => None,
            }?;
            Some((tile.entity_id.clone(), path))
        });
    let Some((entity_id, path)) = desired else {
        if state.entity_image.size().width > 0 || !state.image_status.is_empty() {
            state.entity_image = slint::Image::default();
            state.image_status.clear();
            state.image_last = None;
            render(ui, &state);
        }
        return;
    };
    if state
        .image_request
        .as_ref()
        .is_some_and(|(_, _, at)| at.elapsed() < Duration::from_secs(10))
    {
        return;
    }
    if state.image_last.as_ref().is_some_and(|(id, p, at)| {
        id == &entity_id && p == &path && at.elapsed() < Duration::from_secs(3)
    }) {
        return;
    }
    if commands
        .try_send(RuntimeCommand::FetchImage {
            entity_id: entity_id.clone(),
            path: path.clone(),
        })
        .is_ok()
    {
        if !state
            .image_last
            .as_ref()
            .is_some_and(|(id, p, _)| id == &entity_id && p == &path)
        {
            state.entity_image = slint::Image::default();
            state.image_status = localized(&state, "Loading image…", "Bild wird geladen …").into();
        }
        state.image_request = Some((entity_id, path, Instant::now()));
        render(ui, &state);
    }
}

fn retry_settings_acknowledgements(
    commands: &SyncSender<RuntimeCommand>,
    state: &Rc<RefCell<ViewState>>,
) {
    let mut state = state.borrow_mut();
    while let Some(revision) = state.pending_settings_acks.front().copied() {
        match commands.try_send(RuntimeCommand::SettingsApplied(revision)) {
            Ok(()) => {
                state.pending_settings_acks.pop_front();
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => break,
        }
    }
}

fn open_details(ui: &slint::Weak<MainWindow>, state: &Rc<RefCell<ViewState>>, entity_id: &str) {
    let mut state = state.borrow_mut();
    if let Some(dashboard) = &state.dashboard
        && let Some(room) = dashboard
            .rooms
            .iter()
            .find(|room| room.entities.iter().any(|tile| tile.entity_id == entity_id))
    {
        state.selected_room = Some(room.id.clone());
    }
    state.selected_entity = Some(entity_id.to_owned());
    if let Some(ui) = ui.upgrade() {
        render(&ui, &state);
    }
}

// Keep the touched card alive when a clock tick or another HA entity changes.
// A change of identities or order deliberately invalidates the old touch targets.
fn sync_card_rows<T: Clone + PartialEq + 'static>(
    current: &ModelRc<T>,
    rows: Vec<T>,
    same_id: impl Fn(&T, &T) -> bool,
    replace: impl FnOnce(ModelRc<T>),
) {
    if let Some(model) = current.as_any().downcast_ref::<VecModel<T>>()
        && model.row_count() == rows.len()
        && rows
            .iter()
            .enumerate()
            .all(|(i, row)| model.row_data(i).is_some_and(|old| same_id(&old, row)))
    {
        for (i, row) in rows.into_iter().enumerate() {
            if model.row_data(i).as_ref() != Some(&row) {
                model.set_row_data(i, row);
            }
        }
    } else {
        replace(ModelRc::new(VecModel::from(rows)));
    }
}

fn room_items(state: &ViewState, german: bool) -> Vec<RoomItem> {
    state
        .dashboard
        .as_ref()
        .into_iter()
        .flat_map(|dashboard| &dashboard.rooms)
        .map(|room| {
            let id = room_id_string(&room.id);
            let fallback_name = if german && room.id == RoomId::Unassigned {
                "Nicht zugeordnet".into()
            } else {
                room.name.clone()
            };
            RoomItem {
                id: id.as_str().into(),
                icon: state.settings.settings.room_icon(&id, &room.name).into(),
                name: state
                    .settings
                    .settings
                    .room_name(&id, &fallback_name)
                    .into(),
                summary: room_summary(room, german).into(),
            }
        })
        .collect()
}

#[allow(clippy::too_many_lines)] // Keep the UI event/property mapping together.
fn render(ui: &MainWindow, state: &ViewState) {
    let german = state.settings.settings.language == "de";
    let dark_mode = match state.settings.settings.theme.as_str() {
        "dark" => true,
        "automatic" => !state.daylight,
        _ => false,
    };
    let dashboard = state.dashboard.as_ref();
    let rooms = room_items(state, german);
    let selected_index = dashboard
        .and_then(|dashboard| {
            state
                .selected_room
                .as_ref()
                .and_then(|selected| dashboard.rooms.iter().position(|room| &room.id == selected))
        })
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(-1);
    let selected_room = dashboard.and_then(|dashboard| {
        usize::try_from(selected_index)
            .ok()
            .and_then(|index| dashboard.rooms.get(index))
    });
    let tiles: Vec<_> = selected_room
        .into_iter()
        .flat_map(|room| &room.entities)
        .map(|tile| tile_item(tile, german, &state.settings.settings))
        .collect();
    let favorites: Vec<_> = state
        .settings
        .settings
        .favorites
        .iter()
        .filter_map(|entity_id| dashboard.and_then(|dashboard| dashboard.entity(entity_id)))
        .map(|tile| tile_item(tile, german, &state.settings.settings))
        .collect();
    let connection = dashboard.map_or(ConnectionStatus::Synchronizing, |dashboard| {
        dashboard.connection
    });

    sync_card_rows(
        &ui.get_tiles(),
        tiles,
        |a, b| a.entity_id == b.entity_id,
        |model| ui.set_tiles(model),
    );
    sync_card_rows(
        &ui.get_favorites(),
        favorites,
        |a, b| a.entity_id == b.entity_id,
        |model| ui.set_favorites(model),
    );
    sync_card_rows(
        &ui.get_rooms(),
        rooms,
        |a, b| a.id == b.id,
        |model| ui.set_rooms(model),
    );
    ui.set_selected_room(selected_index);
    ui.set_home_view(state.settings.settings.home_view.as_str().into());
    render_appearance(ui, &state.settings.settings, dark_mode);
    ui.set_rooms_page(state.rooms_page);
    ui.set_german(german);
    ui.set_boot_screen(dashboard.is_none());
    ui.set_command_warning(false);
    ui.set_connection_ready(connection.is_ready());
    ui.set_omitted_entities(i32::try_from(state.omitted_entities).unwrap_or(i32::MAX));
    ui.set_management_degraded(
        connection.is_ready() && state.runtime_status.contains("management is unavailable"),
    );
    ui.set_connection_text(connection_label(state, connection, german).into());
    ui.set_details_fullscreen(state.settings.settings.details == "fullscreen");
    ui.set_details_open(state.selected_entity.is_some());
    let details = detail_item(state, german);
    if ui.get_details_data().entity_id != details.entity_id {
        ui.set_editing_controls(false);
        ui.set_entity_error("".into());
        ui.set_entity_results(ModelRc::default());
        ui.set_showing_results(false);
        ui.set_entity_result_message("".into());
    }
    ui.set_details_data(details);
    ui.set_entity_image(
        if state
            .image_last
            .as_ref()
            .is_some_and(|(id, _, _)| state.selected_entity.as_deref() == Some(id.as_str()))
        {
            state.entity_image.clone()
        } else {
            slint::Image::default()
        },
    );
    ui.set_image_status(image_status(state).into());
    control_adapter::render(
        ui,
        state
            .selected_entity
            .as_ref()
            .and_then(|id| dashboard.and_then(|dashboard| dashboard.entity(id))),
        german,
    );
    ui.set_display_off(state.display.is_off);
    ui.set_show_clock(state.display.show_clock);
    ui.set_clock_time(state.clock_label.as_str().into());
    ui.set_display_dim_opacity(f32::from(100_u8.saturating_sub(state.display.brightness)) / 100.0);
}

fn render_appearance(ui: &MainWindow, settings: &njorun_app_core::settings::Settings, dark: bool) {
    ui.set_color_scheme(settings.effective_color_scheme(dark).into());
    ui.set_increased_contrast(settings.effective_increased_contrast());
    ui.set_reduced_motion(settings.reduced_motion);
    ui.set_dark_mode(dark);
    ui.set_theme(settings.theme.as_str().into());
    ui.set_orientation_landscape(settings.orientation.starts_with("landscape"));
    ui.set_orientation_flipped(settings.orientation.ends_with("_flipped"));
    ui.set_settings_managed(true);
}

fn apply_settings(ui: &MainWindow, state: &ViewState, initial_size: impl Into<InitialSize>) {
    let portrait = state.settings.settings.orientation == "portrait";
    initial_size.into().apply(ui.window(), portrait);
    apply_ui_scale(ui.window(), state.settings.settings.ui_scale);
}

thread_local! {
    /// The scale factor the window had before the interface size was applied, for example the
    /// one of a high-density screen.
    static BASE_SCALE: std::cell::Cell<Option<f32>> = const { std::cell::Cell::new(None) };
}

/// Sets the size of the interface in percent on top of the window's own scale. The window keeps
/// its pixels; everything in it is drawn larger or smaller and the layout follows the room that
/// is left (the tablet does the same).
fn apply_ui_scale(window: &slint::Window, percent: u8) {
    let base = BASE_SCALE.with(|base| {
        if base.get().is_none() {
            base.set(Some(window.scale_factor()));
        }
        base.get().unwrap_or(1.0)
    });
    let physical = window.size();
    window.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged {
        scale_factor: base * f32::from(percent) / 100.0,
    });
    window.set_size(physical);
}

fn room_id_string(id: &RoomId) -> String {
    match id {
        RoomId::Area(area_id) => area_id.clone(),
        RoomId::Unassigned => "unassigned".to_owned(),
    }
}

fn tile_item(
    tile: &EntityTile,
    german: bool,
    settings: &njorun_app_core::settings::Settings,
) -> TileItem {
    let feedback = tile.action_feedback.as_ref();
    let value = feedback_value(feedback, tile, german).unwrap_or_else(|| state_value(tile, german));
    let pending = matches!(
        feedback,
        Some(
            ActionFeedback::AwaitingAcknowledgement { .. }
                | ActionFeedback::AcceptedAwaitingState { .. }
                | ActionFeedback::AcceptedAwaitingConfirmation { .. }
        )
    );
    let failed = matches!(
        feedback,
        Some(ActionFeedback::Rejected { .. } | ActionFeedback::Unconfirmed { .. })
    );
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
        icon: entity_icon(tile),
        active: presentation::tile_active(tile),
        pending,
        failed,
        unavailable: !tile.is_available(),
        stale: tile.is_stale,
        command_enabled: tile.command_enabled,
    }
}

const fn tile_kind(tile: &EntityTile) -> &'static str {
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

fn detail_item(state: &ViewState, german: bool) -> DetailsItem {
    let selected = state.selected_entity.as_deref().and_then(|entity_id| {
        state
            .dashboard
            .as_ref()
            .and_then(|dashboard| dashboard.entity(entity_id))
    });
    let Some(tile) = selected else {
        return empty_details_item();
    };
    let reason = match tile.action_feedback.as_ref() {
        Some(ActionFeedback::Rejected { reason }) => reason.clone(),
        _ => String::new(),
    };
    let update_time = tile
        .state
        .as_ref()
        .and_then(|state| state.last_updated.as_deref())
        .map_or_else(String::new, |value| format_update_time(value, german));
    let attributes = detail_attributes(tile, german);
    let view = presentation::detail_view(tile, german);
    let slider = view.slider.clone().unwrap_or(presentation::Slider {
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
        name: state
            .settings
            .settings
            .entity_name(&tile.entity_id, &tile.name)
            .into(),
        value: feedback_value(tile.action_feedback.as_ref(), tile, german)
            .unwrap_or_else(|| presentation::detail_value(tile, german))
            .into(),
        update_time: update_time.into(),
        reason: reason.into(),
        attributes: attributes.into(),
        icon: entity_icon(tile),
        unavailable: !tile.is_available(),
        can_toggle: tile.domain.tap_behavior() == TapBehavior::Toggle,
        controls_enabled: tile.command_enabled,
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
        camera_image: slint::Image::default(),
        camera_loading: false,
    }
}

fn format_update_time(value: &str, german: bool) -> String {
    DateTime::parse_from_rfc3339(value).map_or_else(
        |_| value.to_owned(),
        |timestamp| {
            timestamp
                .with_timezone(&Local)
                .format(if german {
                    "%d.%m.%Y · %H:%M"
                } else {
                    "%b %-d, %Y · %-I:%M %p"
                })
                .to_string()
        },
    )
}

fn empty_details_item() -> DetailsItem {
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

fn entity_icon(tile: &EntityTile) -> EntityIcon {
    match presentation::entity_icon(tile) {
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

fn connection_label(state: &ViewState, connection: ConnectionStatus, german: bool) -> String {
    match connection {
        ConnectionStatus::Offline => {
            if german {
                "Home Assistant offline · Verbindung wird wiederhergestellt…".to_owned()
            } else {
                "Home Assistant offline · Reconnecting…".to_owned()
            }
        }
        ConnectionStatus::Synchronizing => {
            if german {
                "Synchronisiere Home Assistant…".to_owned()
            } else {
                "Synchronizing Home Assistant…".to_owned()
            }
        }
        ConnectionStatus::Ready => {
            if state.runtime_status.contains("management is unavailable") {
                if german {
                    "Dashboard verbunden · Njörun-Verwaltung nicht verfügbar".to_owned()
                } else {
                    "Dashboard connected · Njörun management unavailable".to_owned()
                }
            } else if german {
                "Verbunden".to_owned()
            } else {
                "Connected".to_owned()
            }
        }
    }
}

fn unix_minute() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() / 60)
}

fn local_clock_label(german: bool) -> String {
    chrono::Local::now()
        .format(if german { "%H:%M" } else { "%-I:%M %p" })
        .to_string()
}

fn localized<'a>(state: &ViewState, english: &'a str, german: &'a str) -> &'a str {
    if state.settings.settings.language == "de" {
        german
    } else {
        english
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EntityIcon, InitialSize, MainWindow, ViewState, apply_ui_scale, connection_label,
        entity_icon, format_update_time, local_clock_label, localized, state_value,
    };
    use njorun_app_core::{ConnectionStatus, EntityDomain, EntityTile, HaEntityState};
    use slint::{ComponentHandle, LogicalSize, PhysicalSize};
    use std::collections::BTreeMap;

    fn tile(domain: EntityDomain, state: &str, class: Option<&str>) -> EntityTile {
        let mut attributes = BTreeMap::new();
        if let Some(class) = class {
            attributes.insert("device_class".into(), class.into());
        }
        EntityTile {
            entity_id: "binary_sensor.test".into(),
            name: "Test".into(),
            icon: None,
            domain,
            state: Some(HaEntityState {
                entity_id: "binary_sensor.test".into(),
                state: state.into(),
                attributes,
                last_updated: None,
            }),
            is_stale: false,
            command_enabled: false,
            action_feedback: None,
        }
    }

    #[test]
    fn the_interface_size_keeps_the_pixels_and_changes_what_fits() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("main window");
        ui.window().set_size(PhysicalSize::new(800, 1280));
        let base = ui.window().scale_factor();

        apply_ui_scale(ui.window(), 200);
        assert_eq!(ui.window().size(), PhysicalSize::new(800, 1280));
        assert!((ui.window().scale_factor() / base - 2.0).abs() < 1e-4);

        apply_ui_scale(ui.window(), 75);
        assert_eq!(ui.window().size(), PhysicalSize::new(800, 1280));
        assert!((ui.window().scale_factor() / base - 0.75).abs() < 1e-4);

        // 100 percent is the window as it was.
        apply_ui_scale(ui.window(), 100);
        assert!((ui.window().scale_factor() - base).abs() < 1e-4);
    }

    #[test]
    fn initial_size_preserves_units_and_swaps_orientation() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("main window");

        let logical = InitialSize::from(LogicalSize::new(600.0, 960.0));
        logical.apply(ui.window(), false);
        assert_eq!(ui.window().size(), PhysicalSize::new(960, 600));

        let physical = InitialSize::from(PhysicalSize::new(1280, 800));
        physical.apply(ui.window(), true);
        assert_eq!(ui.window().size(), PhysicalSize::new(800, 1280));
        assert_eq!(
            localized(&ViewState::new(), "English", "Deutsch"),
            "English"
        );
        let mut state = ViewState::new();
        state.runtime_status = "management is unavailable".into();
        assert_eq!(
            connection_label(&state, ConnectionStatus::Ready, false),
            "Dashboard connected · Njörun management unavailable"
        );

        assert_eq!(
            state_value(&tile(EntityDomain::BinarySensor, "on", Some("door")), false),
            "Open"
        );
        assert_eq!(
            state_value(
                &tile(EntityDomain::BinarySensor, "off", Some("motion")),
                false
            ),
            "No motion"
        );
        assert_eq!(
            state_value(
                &tile(EntityDomain::BinarySensor, "on", Some("moisture")),
                true
            ),
            "Feucht"
        );
        assert_eq!(
            state_value(
                &tile(EntityDomain::BinarySensor, "off", Some("moisture")),
                false
            ),
            "Dry"
        );
        assert_eq!(
            state_value(&tile(EntityDomain::BinarySensor, "idle", None), false),
            "idle"
        );
        let sensor = tile(EntityDomain::Sensor, "42", None);
        assert_eq!(state_value(&sensor, false), "42");
        assert_eq!(entity_icon(&sensor), EntityIcon::Sensor);
    }

    #[test]
    fn entity_labels_cover_offline_scene_sensor_and_icon_states() {
        let state = ViewState::new();
        assert_eq!(
            connection_label(&state, ConnectionStatus::Offline, false),
            "Home Assistant offline · Reconnecting…"
        );
        assert_eq!(
            connection_label(&state, ConnectionStatus::Offline, true),
            "Home Assistant offline · Verbindung wird wiederhergestellt…"
        );
        assert_eq!(
            connection_label(&state, ConnectionStatus::Synchronizing, false),
            "Synchronizing Home Assistant…"
        );
        assert_eq!(
            connection_label(&state, ConnectionStatus::Synchronizing, true),
            "Synchronisiere Home Assistant…"
        );

        let mut scene = tile(EntityDomain::Scene, "scening", None);
        assert_eq!(state_value(&scene, false), "Run scene");
        assert_eq!(state_value(&scene, true), "Szene ausführen");
        scene.is_stale = true;
        assert_eq!(state_value(&scene, false), "Scene offline");
        assert_eq!(state_value(&scene, true), "Szene offline");
        assert_eq!(entity_icon(&scene), EntityIcon::Play);

        let mut sensor = tile(EntityDomain::Sensor, "21.5", None);
        sensor
            .state
            .as_mut()
            .expect("sensor state")
            .attributes
            .insert("unit_of_measurement".into(), "°C".into());
        assert_eq!(state_value(&sensor, false), "21.5 °C");
        assert_eq!(entity_icon(&sensor), EntityIcon::Thermometer);

        let mut unavailable = tile(EntityDomain::Switch, "unknown", None);
        assert_eq!(state_value(&unavailable, false), "Unknown");
        assert_eq!(state_value(&unavailable, true), "Unbekannt");
        unavailable.state = None;
        assert_eq!(state_value(&unavailable, false), "Unavailable");
        assert_eq!(entity_icon(&unavailable), EntityIcon::Power);

        assert_eq!(
            format_update_time("not-a-timestamp", false),
            "not-a-timestamp"
        );
        let german_time = format_update_time("2026-11-05T12:00:00+00:00", true);
        assert!(german_time.contains("2026 · "));
        assert!(!german_time.contains("AM") && !german_time.contains("PM"));
        let english_time = format_update_time("2026-11-05T12:00:00+00:00", false);
        assert!(english_time.contains("2026 · "));
        assert!(english_time.ends_with("AM") || english_time.ends_with("PM"));
        let english_clock = local_clock_label(false);
        assert!(english_clock.ends_with("AM") || english_clock.ends_with("PM"));
        assert!(!local_clock_label(true).contains(' '));
    }
}
