//! Native Slint presentation for the simulator runtime.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use njorun_app_core::display::{DisplayController, DisplayState, TouchDisposition};
use njorun_app_core::{
    ActionFeedback, ActionKind, ConnectionStatus, Dashboard, EntityDomain, EntityTile, Room,
    RoomId, TargetState, settings::AppliedSettings,
};
use slint::{
    ComponentHandle, LogicalSize, Model, ModelRc, PhysicalSize, Timer, TimerMode, VecModel,
};

use crate::{RuntimeCommand, RuntimeEvent};

#[allow(unsafe_code)]
mod generated {
    slint::include_modules!();
}

use generated::{DetailsItem, EntityIcon, MainWindow, RoomItem, TileItem};

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
        Self {
            dashboard: None,
            settings,
            daylight: true,
            selected_room: None,
            selected_entity: None,
            rooms_page: false,
            runtime_status: String::new(),
            pending_settings_acks: VecDeque::new(),
            display_controller,
            display,
            started_at,
            clock_label: local_clock_label(),
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
            self.clock_label = local_clock_label();
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
    let initial_size = crate::logical_size(width, height);
    ui.window().set_size(initial_size);

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

fn install_entity_callbacks(
    ui: &MainWindow,
    state: &Rc<RefCell<ViewState>>,
    commands: SyncSender<RuntimeCommand>,
) {
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
                matches!(
                    tile.domain,
                    EntityDomain::Sensor | EntityDomain::BinarySensor
                ) || !tile.command_enabled
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
    timer.start(TimerMode::Repeated, Duration::from_millis(40), move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if state.borrow_mut().update_display() {
            render(&ui, &state.borrow());
        }
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

fn apply_runtime_event(
    ui: &MainWindow,
    state: &Rc<RefCell<ViewState>>,
    initial_size: impl Into<InitialSize>,
    event: RuntimeEvent,
) {
    let initial_size = initial_size.into();
    match event {
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

fn render(ui: &MainWindow, state: &ViewState) {
    let german = state.settings.settings.language == "de";
    let dark_mode = match state.settings.settings.theme.as_str() {
        "dark" => true,
        "automatic" => !state.daylight,
        _ => false,
    };
    let dashboard = state.dashboard.as_ref();
    let rooms: Vec<_> = dashboard
        .into_iter()
        .flat_map(|dashboard| &dashboard.rooms)
        .map(|room| RoomItem {
            id: room_id_string(&room.id).into(),
            name: if german && room.id == RoomId::Unassigned {
                "Nicht zugeordnet".into()
            } else {
                room.name.as_str().into()
            },
            summary: room_summary(room, german).into(),
        })
        .collect();
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
        .map(|tile| tile_item(tile, german))
        .collect();
    let favorites: Vec<_> = state
        .settings
        .settings
        .favorites
        .iter()
        .filter_map(|entity_id| dashboard.and_then(|dashboard| dashboard.entity(entity_id)))
        .map(|tile| tile_item(tile, german))
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
    ui.set_rooms_page(state.rooms_page);
    ui.set_dark_mode(dark_mode);
    ui.set_german(german);
    ui.set_boot_screen(dashboard.is_none());
    ui.set_command_warning(false);
    ui.set_connection_ready(connection.is_ready());
    ui.set_management_degraded(
        connection.is_ready() && state.runtime_status.contains("management is unavailable"),
    );
    ui.set_connection_text(connection_label(state, connection, german).into());
    ui.set_details_fullscreen(state.settings.settings.details == "fullscreen");
    ui.set_details_open(state.selected_entity.is_some());
    ui.set_details_data(detail_item(state, german));
    ui.set_display_off(state.display.is_off);
    ui.set_show_clock(state.display.show_clock);
    ui.set_clock_time(state.clock_label.as_str().into());
    ui.set_display_dim_opacity(f32::from(100_u8.saturating_sub(state.display.brightness)) / 100.0);
}

fn apply_settings(ui: &MainWindow, state: &ViewState, initial_size: impl Into<InitialSize>) {
    let portrait = state.settings.settings.orientation == "portrait";
    initial_size.into().apply(ui.window(), portrait);
}

fn room_id_string(id: &RoomId) -> String {
    match id {
        RoomId::Area(area_id) => area_id.clone(),
        RoomId::Unassigned => "unassigned".to_owned(),
    }
}

fn room_summary(room: &Room, german: bool) -> String {
    if room.entities.is_empty() {
        if german {
            "Noch keine Steuerelemente".to_owned()
        } else {
            "No controls yet".to_owned()
        }
    } else if german {
        format!(
            "{} {}",
            room.entities.len(),
            if room.entities.len() == 1 {
                "Steuerelement"
            } else {
                "Steuerelemente"
            }
        )
    } else {
        format!(
            "{} {}",
            room.entities.len(),
            if room.entities.len() == 1 {
                "control"
            } else {
                "controls"
            }
        )
    }
}

fn tile_item(tile: &EntityTile, german: bool) -> TileItem {
    let feedback = tile.action_feedback.as_ref();
    let value = feedback_value(feedback, tile, german).unwrap_or_else(|| state_value(tile, german));
    let pending = matches!(
        feedback,
        Some(
            ActionFeedback::AwaitingAcknowledgement { .. }
                | ActionFeedback::AcceptedAwaitingState { .. }
        )
    );
    let failed = matches!(
        feedback,
        Some(ActionFeedback::Rejected { .. } | ActionFeedback::Unconfirmed { .. })
    );
    TileItem {
        entity_id: tile.entity_id.clone().into(),
        name: tile.name.clone().into(),
        value: value.into(),
        icon: entity_icon(tile),
        active: !tile.is_stale
            && matches!(
                tile.domain,
                EntityDomain::Light | EntityDomain::Switch | EntityDomain::BinarySensor
            )
            && tile.state_value() == Some("on"),
        pending,
        failed,
        unavailable: !tile.is_available(),
        stale: tile.is_stale,
        command_enabled: tile.command_enabled,
    }
}

fn state_value(tile: &EntityTile, german: bool) -> String {
    let Some(state) = &tile.state else {
        return unavailable_label(german).to_owned();
    };
    if state.state == "unavailable" || state.state == "unknown" {
        return unavailable_label(german).to_owned();
    }

    let base_value = match tile.domain {
        EntityDomain::Light | EntityDomain::Switch | EntityDomain::BinarySensor => {
            let class = (tile.domain == EntityDomain::BinarySensor)
                .then(|| {
                    state
                        .attributes
                        .get("device_class")
                        .and_then(serde_json::Value::as_str)
                })
                .flatten();
            let (on, off) = match (class, german) {
                (Some("door" | "window" | "opening"), true) => ("Offen", "Geschlossen"),
                (Some("door" | "window" | "opening"), false) => ("Open", "Closed"),
                (Some("motion"), true) => ("Bewegung erkannt", "Keine Bewegung"),
                (Some("motion"), false) => ("Motion detected", "No motion"),
                (Some("moisture"), true) => ("Feucht", "Trocken"),
                (Some("moisture"), false) => ("Wet", "Dry"),
                (_, true) => ("Ein", "Aus"),
                (_, false) => ("On", "Off"),
            };
            match state.state.as_str() {
                "on" => on,
                "off" => off,
                other => other,
            }
            .to_owned()
        }
        EntityDomain::Scene if tile.is_stale => {
            return if german {
                "Szene offline"
            } else {
                "Scene offline"
            }
            .to_owned();
        }
        EntityDomain::Scene => {
            if german {
                "Szene ausführen".to_owned()
            } else {
                "Run scene".to_owned()
            }
        }
        EntityDomain::Sensor => {
            let unit = state
                .attributes
                .get("unit_of_measurement")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if unit.is_empty() {
                state.state.clone()
            } else {
                format!("{} {}", state.state, unit)
            }
        }
    };

    let value = if tile.domain == EntityDomain::Light
        && state.state == "on"
        && let Some(brightness) = state
            .attributes
            .get("brightness")
            .and_then(serde_json::Value::as_i64)
    {
        format!(
            "{} · {}%",
            if german { "Ein" } else { "On" },
            brightness_percent(brightness)
        )
    } else {
        base_value
    };

    if tile.is_stale {
        format!(
            "{} · {}",
            if german {
                "Letzter Stand"
            } else {
                "Last known"
            },
            value
        )
    } else {
        value
    }
}

fn feedback_value(
    feedback: Option<&ActionFeedback>,
    tile: &EntityTile,
    german: bool,
) -> Option<String> {
    match feedback? {
        ActionFeedback::AwaitingAcknowledgement { action } => {
            let target = match action {
                ActionKind::Toggle(target) => *target,
                ActionKind::RunScene => {
                    return Some(if german {
                        "Szene wird ausgeführt …".to_owned()
                    } else {
                        "Running scene…".to_owned()
                    });
                }
            };
            Some(turning_label(target, german).to_owned())
        }
        ActionFeedback::AcceptedAwaitingState { target } => {
            Some(turning_label(*target, german).to_owned())
        }
        ActionFeedback::SceneRequested => Some(if german {
            "Angefordert".to_owned()
        } else {
            "Requested".to_owned()
        }),
        ActionFeedback::Confirmed { .. } => Some(state_value(tile, german)),
        ActionFeedback::Rejected { .. } => Some(if german {
            "Aktion fehlgeschlagen".to_owned()
        } else {
            "Action failed".to_owned()
        }),
        ActionFeedback::Unconfirmed { .. } => Some(if german {
            "Nicht bestätigt".to_owned()
        } else {
            "Unconfirmed".to_owned()
        }),
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
        .unwrap_or("")
        .to_owned();
    let attributes = detail_attributes(tile, german);
    DetailsItem {
        entity_id: tile.entity_id.clone().into(),
        name: tile.name.clone().into(),
        value: feedback_value(tile.action_feedback.as_ref(), tile, german)
            .unwrap_or_else(|| state_value(tile, german))
            .into(),
        update_time: update_time.into(),
        reason: reason.into(),
        attributes: attributes.into(),
        icon: entity_icon(tile),
        unavailable: !tile.is_available(),
    }
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
    }
}

fn detail_attributes(tile: &EntityTile, german: bool) -> String {
    let Some(state) = &tile.state else {
        return String::new();
    };
    let mut values = Vec::new();
    if let Some(brightness) = state
        .attributes
        .get("brightness")
        .and_then(serde_json::Value::as_i64)
    {
        let percent = brightness_percent(brightness);
        values.push(format!(
            "{}: {percent}%",
            if german { "Helligkeit" } else { "Brightness" }
        ));
    }
    if let Some(unit) = state
        .attributes
        .get("unit_of_measurement")
        .and_then(serde_json::Value::as_str)
    {
        values.push(format!(
            "{}: {unit}",
            if german { "Einheit" } else { "Unit" }
        ));
    }
    values.join(" · ")
}

fn brightness_percent(brightness: i64) -> u8 {
    let percent = (brightness.clamp(0, 255) * 100 + 127) / 255;
    u8::try_from(percent).unwrap_or_default()
}

fn entity_icon(tile: &EntityTile) -> EntityIcon {
    match tile.icon.as_deref() {
        Some("mdi:lightbulb") => EntityIcon::Lightbulb,
        Some("mdi:power") => EntityIcon::Power,
        Some("mdi:play") => EntityIcon::Play,
        Some("mdi:thermometer") => EntityIcon::Thermometer,
        Some("mdi:checkbox-blank-circle-outline") => EntityIcon::BinarySensor,
        _ => match tile.domain {
            EntityDomain::Light => EntityIcon::Lightbulb,
            EntityDomain::Switch => EntityIcon::Power,
            EntityDomain::Scene => EntityIcon::Play,
            EntityDomain::Sensor => {
                let attributes = tile.state.as_ref().map(|state| &state.attributes);
                if attributes.is_some_and(|attrs| {
                    attrs
                        .get("device_class")
                        .and_then(serde_json::Value::as_str)
                        == Some("temperature")
                        || matches!(
                            attrs
                                .get("unit_of_measurement")
                                .and_then(serde_json::Value::as_str),
                            Some("°C" | "°F" | "K")
                        )
                }) {
                    EntityIcon::Thermometer
                } else {
                    EntityIcon::Sensor
                }
            }
            EntityDomain::BinarySensor => EntityIcon::BinarySensor,
        },
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

fn local_clock_label() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

fn localized<'a>(state: &ViewState, english: &'a str, german: &'a str) -> &'a str {
    if state.settings.settings.language == "de" {
        german
    } else {
        english
    }
}

const fn unavailable_label(german: bool) -> &'static str {
    if german {
        "Nicht verfügbar"
    } else {
        "Unavailable"
    }
}

const fn turning_label(target: TargetState, german: bool) -> &'static str {
    match (target, german) {
        (TargetState::On, false) => "Turning on…",
        (TargetState::Off, false) => "Turning off…",
        (TargetState::On, true) => "Wird eingeschaltet…",
        (TargetState::Off, true) => "Wird ausgeschaltet…",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EntityIcon, InitialSize, MainWindow, ViewState, connection_label, entity_icon, localized,
        state_value,
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
}
