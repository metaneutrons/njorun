//! Qualification helpers for real Slint software-rendered frames and pointer interactions.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use njorun_app_core::display::DisplayController;
use njorun_app_core::{
    ActionFeedback, ActionKind, AppCore, AreaRegistryEntry, ConnectionStatus, Dashboard,
    DashboardOrder, DetailAction, EntityDomain, EntityRegistryEntry, EntityTile, HaEntityState,
    Room, RoomId, TargetState,
    settings::{AppliedSettings, Settings},
};
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, TargetPixel,
};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::{ComponentHandle, Model, PhysicalSize};

use super::{
    CommissioningWindow, EntityIcon, InstanceItem, MainWindow, ViewState, WifiItem,
    apply_runtime_event, apply_settings, install_callbacks, render,
    retry_settings_acknowledgements, start_event_timer,
};
use crate::runtime::{self, RuntimeOptions};
use crate::{RuntimeCommand, RuntimeEvent};

thread_local! {
    static PREVIEW_WINDOW: Rc<MinimalSoftwareWindow> =
        MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    static PREVIEW_PLATFORM_INSTALLED: Cell<bool> = const { Cell::new(false) };
    static PREVIEW_CLOCK: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

struct PreviewPlatform;

impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(PREVIEW_WINDOW.with(Rc::clone))
    }

    fn duration_since_start(&self) -> Duration {
        PREVIEW_CLOCK.with(Cell::get)
    }
}

/// A pixel produced by Slint's software renderer for a qualification screenshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewPixel {
    /// Red channel.
    pub red: u8,
    /// Green channel.
    pub green: u8,
    /// Blue channel.
    pub blue: u8,
    /// Alpha channel.
    pub alpha: u8,
}

impl TargetPixel for PreviewPixel {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let inverse_alpha = u32::from(u8::MAX - color.alpha);
        self.red = blend_channel(self.red, color.red, inverse_alpha);
        self.green = blend_channel(self.green, color.green, inverse_alpha);
        self.blue = blend_channel(self.blue, color.blue, inverse_alpha);
        self.alpha = u8::MAX;
    }

    fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha: u8::MAX,
        }
    }
}

fn blend_channel(background: u8, foreground: u8, inverse_alpha: u32) -> u8 {
    let blended = u32::from(foreground) + u32::from(background) * inverse_alpha / 255;
    u8::try_from(blended.min(u32::from(u8::MAX))).unwrap_or_default()
}

/// One deterministic real-Slint frame for a supported screen-size fixture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewFrame {
    /// Stable filename stem, without an extension.
    pub name: String,
    /// Physical frame width in pixels.
    pub width: u32,
    /// Physical frame height in pixels.
    pub height: u32,
    /// Row-major RGBA pixels from Slint's software renderer.
    pub pixels: Vec<PreviewPixel>,
}

/// Failure while preparing the real-Slint qualification preview.
#[derive(Debug)]
pub enum PreviewError {
    /// Another Slint platform was already installed in this process.
    PlatformAlreadyInstalled(slint::platform::SetPlatformError),
    /// Slint could not create or show the preview window.
    Window(PlatformError),
    /// The built-in HA fixture did not satisfy app-core validation.
    Fixture(njorun_app_core::CoreError),
    /// The software renderer did not produce a requested frame.
    RenderUnavailable,
    /// A requested pointer or queue interaction did not match its expected result.
    Interaction(&'static str),
    /// Home Assistant runtime returned a sanitized client error.
    Runtime(njorun_ha_client::ClientError),
    /// The live dashboard or target state did not arrive before the bounded timeout.
    Timeout,
    /// Runtime exited before qualification completed.
    RuntimeStopped,
    /// The requested entity is absent or cannot be safely toggled.
    InvalidTarget(&'static str),
    /// UI emitted a tap other than the single requested target tap.
    UnexpectedCommand,
    /// Runtime did not stop within the bounded shutdown interval.
    ShutdownTimeout,
    /// Runtime worker could not be started.
    WorkerStart,
    /// The requested PNG or report could not be written.
    Output(Box<dyn Error>),
}

impl Display for PreviewError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PlatformAlreadyInstalled(error) => {
                write!(formatter, "cannot install preview platform: {error}")
            }
            Self::Window(error) => write!(formatter, "cannot create preview window: {error}"),
            Self::Fixture(error) => write!(formatter, "invalid preview fixture: {error}"),
            Self::RenderUnavailable => {
                formatter.write_str("Slint did not render the preview frame")
            }
            Self::Interaction(check) => write!(formatter, "UI interaction failed: {check}"),
            Self::Runtime(error) => write!(formatter, "live qualification runtime failed: {error}"),
            Self::Timeout => formatter.write_str("live Home Assistant qualification timed out"),
            Self::RuntimeStopped => {
                formatter.write_str("Home Assistant runtime stopped before qualification completed")
            }
            Self::InvalidTarget(reason) => write!(formatter, "invalid live UI target: {reason}"),
            Self::UnexpectedCommand => {
                formatter.write_str("UI emitted an unexpected command during qualification")
            }
            Self::ShutdownTimeout => formatter
                .write_str("Home Assistant runtime did not stop before the shutdown timeout"),
            Self::WorkerStart => {
                formatter.write_str("could not start the Home Assistant runtime worker")
            }
            Self::Output(error) => write!(
                formatter,
                "could not write live qualification output: {error}"
            ),
        }
    }
}

impl Error for PreviewError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::PlatformAlreadyInstalled(error) => Some(error),
            Self::Window(error) => Some(error),
            Self::Fixture(error) => Some(error),
            Self::Runtime(error) => Some(error),
            Self::RenderUnavailable
            | Self::Interaction(_)
            | Self::Timeout
            | Self::RuntimeStopped
            | Self::InvalidTarget(_)
            | Self::UnexpectedCommand
            | Self::ShutdownTimeout
            | Self::WorkerStart => None,
            Self::Output(error) => Some(error.as_ref()),
        }
    }
}

/// Inputs for a bounded qualification against a real Home Assistant instance.
pub struct LiveQualificationOptions {
    /// Credential file created by the pairing flow.
    pub credentials: PathBuf,
    /// Per-simulator state directory.
    pub state_directory: PathBuf,
    /// Entity to activate through the real Slint pointer path.
    pub entity_id: String,
    /// Actual area ID, or `unassigned`, containing the target entity.
    pub room_id: String,
    /// Maximum time to wait for authenticated readiness and the resulting state update.
    pub timeout: Duration,
    /// Initial physical display width.
    pub width: u32,
    /// Initial physical display height.
    pub height: u32,
}

/// Result of one real Home Assistant action driven by the Slint software-rendered UI.
pub struct LiveQualificationResult {
    /// The rendered room frame after Home Assistant's authoritative state changed.
    pub frame: PreviewFrame,
    /// Target entity ID.
    pub entity_id: String,
    /// Target room ID.
    pub room_id: String,
    /// Room display name from the live dashboard.
    pub room_name: String,
    /// Authoritative pre-action state.
    pub state_before: String,
    /// Authoritative post-action state.
    pub state_after: String,
    /// Elapsed wall time from startup through the authoritative state update.
    pub elapsed_ms: u64,
}

/// Connect through the production simulator runtime and activate one live HA entity through
/// the production Slint callbacks and pointer event path.
///
/// This function requires a real paired credential file and never logs or returns credentials.
/// It accepts only a ready light or switch whose current state is `on` or `off`, and only forwards
/// the one requested target tap to the runtime command queue.
///
/// # Errors
///
/// Returns an error if the runtime cannot authenticate, the requested room/entity is not live,
/// Home Assistant does not publish the resulting state in time, rendering fails, or shutdown times
/// out. All network/storage errors use the transport's sanitized error type.
pub fn qualify_live_ha_ui(
    options: &LiveQualificationOptions,
    write_result: impl FnOnce(&LiveQualificationResult) -> Result<(), Box<dyn Error>>,
) -> Result<LiveQualificationResult, PreviewError> {
    validate_live_options(options)?;
    install_preview_platform()?;

    let window = PREVIEW_WINDOW.with(Rc::clone);
    let ui = MainWindow::new().map_err(PreviewError::Window)?;
    let initial_size = PhysicalSize::new(options.width, options.height);
    ui.window().set_size(initial_size);
    let state = Rc::new(RefCell::new(ViewState::new()));
    render(&ui, &state.borrow());
    ui.show().map_err(PreviewError::Window)?;

    let (ui_commands, ui_command_rx) = mpsc::sync_channel(32);
    let (runtime_events, runtime_event_rx) = mpsc::sync_channel(256);
    let mut worker = RuntimeWorker::start(
        RuntimeOptions {
            credentials: options.credentials.clone(),
            state_directory: options.state_directory.clone(),
        },
        runtime_events,
    )?;
    install_callbacks(&ui, &state, ui_commands.clone());
    let timer = start_event_timer(
        &ui,
        Rc::clone(&state),
        runtime_event_rx,
        ui_commands,
        initial_size,
    );

    let started = Instant::now();
    let qualification = qualify_live_session(
        &ui,
        &window,
        &state,
        &mut worker,
        &ui_command_rx,
        options,
        started,
    );
    let output = qualification.as_ref().map_or_else(
        |_| Ok(()),
        |result| write_result(result).map_err(PreviewError::Output),
    );
    let shutdown = worker.stop(&ui_command_rx, Duration::from_secs(20));
    timer.stop();

    match (qualification, output, shutdown) {
        (Ok(result), Ok(()), Ok(())) => Ok(result),
        (Err(error), _, _) | (Ok(_), Err(error), _) | (Ok(_), Ok(()), Err(error)) => Err(error),
    }
}

fn validate_live_options(options: &LiveQualificationOptions) -> Result<(), PreviewError> {
    let pixels = u64::from(options.width) * u64::from(options.height);
    if options.timeout.is_zero() || options.timeout > Duration::from_secs(600) {
        return Err(PreviewError::InvalidTarget(
            "timeout must be between 1 millisecond and 600 seconds",
        ));
    }
    if !(440..=2048).contains(&options.width)
        || !(600..=2048).contains(&options.height)
        || pixels > 4_194_304
    {
        return Err(PreviewError::InvalidTarget(
            "display dimensions must fit the supported software-rendering limit",
        ));
    }
    if options.entity_id.is_empty() || options.room_id.is_empty() {
        return Err(PreviewError::InvalidTarget(
            "entity and room IDs must be nonempty",
        ));
    }
    Ok(())
}

struct RuntimeWorker {
    commands: SyncSender<RuntimeCommand>,
    done: Receiver<Result<(), njorun_ha_client::ClientError>>,
    join: Option<JoinHandle<()>>,
    finished: Option<Result<(), njorun_ha_client::ClientError>>,
}

impl RuntimeWorker {
    fn start(
        options: RuntimeOptions,
        events: SyncSender<RuntimeEvent>,
    ) -> Result<Self, PreviewError> {
        let (command_tx, command_rx) = mpsc::sync_channel(32);
        let (done_tx, done_rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name("njorun-live-ui-runtime".into())
            .spawn(move || {
                let result = runtime::run(&options, &command_rx, &events);
                let _ = done_tx.send(result);
            })
            .map_err(|_| PreviewError::WorkerStart)?;
        Ok(Self {
            commands: command_tx,
            done: done_rx,
            join: Some(join),
            finished: None,
        })
    }

    fn ensure_running(&mut self) -> Result<(), PreviewError> {
        if self.poll_finished()? {
            Err(PreviewError::RuntimeStopped)
        } else {
            Ok(())
        }
    }

    fn poll_finished(&mut self) -> Result<bool, PreviewError> {
        if let Some(result) = &self.finished {
            result
                .as_ref()
                .map_err(|error| PreviewError::Runtime(*error))?;
            return Ok(true);
        }
        match self.done.try_recv() {
            Ok(result) => {
                self.finished = Some(result);
                if let Some(Err(error)) = &self.finished {
                    return Err(PreviewError::Runtime(*error));
                }
                Ok(true)
            }
            Err(TryRecvError::Empty) => Ok(false),
            Err(TryRecvError::Disconnected) => Err(PreviewError::RuntimeStopped),
        }
    }

    fn stop(
        &mut self,
        ui_commands: &Receiver<RuntimeCommand>,
        timeout: Duration,
    ) -> Result<(), PreviewError> {
        let deadline = Instant::now() + timeout;
        let mut pending = std::collections::VecDeque::new();
        let mut stop_sent = false;
        loop {
            while let Ok(command) = ui_commands.try_recv() {
                if !stop_sent && let RuntimeCommand::SettingsApplied(revision) = command {
                    pending.push_back(RuntimeCommand::SettingsApplied(revision));
                }
            }
            if let Err(error) = flush_runtime_commands(&self.commands, &mut pending) {
                match error {
                    PreviewError::RuntimeStopped => {
                        let _ = self.poll_finished();
                        return self.join_finished();
                    }
                    other => return Err(other),
                }
            }
            if !stop_sent && pending.is_empty() {
                match self.commands.try_send(RuntimeCommand::Stop) {
                    Ok(()) => stop_sent = true,
                    Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Disconnected(_)) => {
                        return self.join_finished();
                    }
                }
            }
            match self.poll_finished() {
                Ok(true) => return self.join_finished(),
                Ok(false) => {}
                Err(PreviewError::RuntimeStopped) if self.finished.is_none() => {
                    return self.join_finished();
                }
                Err(error) => {
                    let _ = self.join_finished();
                    return Err(error);
                }
            }
            if Instant::now() >= deadline {
                return Err(PreviewError::ShutdownTimeout);
            }
            advance_preview_clock(Duration::from_millis(10));
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn join_finished(&mut self) -> Result<(), PreviewError> {
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| PreviewError::WorkerStart)?;
        }
        if let Some(Err(error)) = self.finished {
            return Err(PreviewError::Runtime(error));
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn qualify_live_session(
    ui: &MainWindow,
    window: &MinimalSoftwareWindow,
    state: &Rc<RefCell<ViewState>>,
    worker: &mut RuntimeWorker,
    ui_commands: &Receiver<RuntimeCommand>,
    options: &LiveQualificationOptions,
    started: Instant,
) -> Result<LiveQualificationResult, PreviewError> {
    let deadline = started + options.timeout;
    let mut pending = std::collections::VecDeque::new();
    let mut tap_forwarded = false;
    wait_for_authenticated_dashboard(
        state,
        worker,
        ui_commands,
        &mut pending,
        &mut tap_forwarded,
        &options.entity_id,
        &options.room_id,
        deadline,
    )?;

    let target = find_live_target(state, &options.entity_id, &options.room_id)?;
    wake_live_display(ui, state);
    let room_index = i32::try_from(target.room_index)
        .map_err(|_| PreviewError::InvalidTarget("room order exceeds the UI index range"))?;
    ui.invoke_room_selected(room_index);
    verify_room_render(ui, target.entity_index, &options.entity_id)?;
    let action_point =
        locate_action_target(ui, window, target.entity_index, target.room_entity_count)?;
    render(ui, &state.borrow());

    dispatch_click(ui, action_point);
    advance_preview_clock(Duration::from_millis(10));
    drain_ui_commands(
        ui_commands,
        &worker.commands,
        &mut pending,
        &options.entity_id,
        true,
        &mut tap_forwarded,
    )?;
    flush_runtime_commands(&worker.commands, &mut pending)?;
    if !tap_forwarded {
        return Err(PreviewError::Interaction(
            "short target tap did not reach the runtime command queue",
        ));
    }

    wait_for_target_change(
        state,
        worker,
        ui_commands,
        &mut pending,
        &mut tap_forwarded,
        &options.entity_id,
        &target.state_before,
        deadline,
    )?;
    let state_after = live_target_state(state, &options.entity_id)?;
    let expected_state = if target.state_before == "on" {
        "off"
    } else {
        "on"
    };
    if state_after != expected_state {
        return Err(PreviewError::Interaction(
            "Home Assistant did not publish a changed authoritative target state",
        ));
    }
    let frame = render_live_frame(window)?;
    Ok(LiveQualificationResult {
        frame,
        entity_id: options.entity_id.clone(),
        room_id: options.room_id.clone(),
        room_name: target.room_name,
        state_before: target.state_before,
        state_after,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

#[allow(clippy::too_many_arguments)]
fn wait_for_authenticated_dashboard(
    state: &Rc<RefCell<ViewState>>,
    worker: &mut RuntimeWorker,
    ui_commands: &Receiver<RuntimeCommand>,
    pending: &mut std::collections::VecDeque<RuntimeCommand>,
    tap_forwarded: &mut bool,
    entity_id: &str,
    room_id: &str,
    deadline: Instant,
) -> Result<(), PreviewError> {
    loop {
        service_live_event_loop(
            worker,
            ui_commands,
            pending,
            entity_id,
            false,
            tap_forwarded,
        )?;
        let state = state.borrow();
        let ready = state.runtime_status == "Connected"
            && state
                .dashboard
                .as_ref()
                .is_some_and(|dashboard| dashboard.connection == ConnectionStatus::Ready);
        let target_present = state.dashboard.as_ref().is_some_and(|dashboard| {
            dashboard
                .rooms
                .iter()
                .any(|room| room_has_target(room, room_id, entity_id))
        });
        drop(state);
        if ready && target_present {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(PreviewError::Timeout);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn wait_for_target_change(
    state: &Rc<RefCell<ViewState>>,
    worker: &mut RuntimeWorker,
    ui_commands: &Receiver<RuntimeCommand>,
    pending: &mut std::collections::VecDeque<RuntimeCommand>,
    tap_forwarded: &mut bool,
    entity_id: &str,
    state_before: &str,
    deadline: Instant,
) -> Result<(), PreviewError> {
    loop {
        service_live_event_loop(worker, ui_commands, pending, entity_id, true, tap_forwarded)?;
        let expected_state = if state_before == "on" { "off" } else { "on" };
        if live_target_state(state, entity_id).is_ok_and(|value| value == expected_state) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(PreviewError::Timeout);
        }
    }
}

fn service_live_event_loop(
    worker: &mut RuntimeWorker,
    ui_commands: &Receiver<RuntimeCommand>,
    pending: &mut std::collections::VecDeque<RuntimeCommand>,
    target_entity: &str,
    allow_target_tap: bool,
    tap_forwarded: &mut bool,
) -> Result<(), PreviewError> {
    advance_preview_clock(Duration::from_millis(10));
    drain_ui_commands(
        ui_commands,
        &worker.commands,
        pending,
        target_entity,
        allow_target_tap,
        tap_forwarded,
    )?;
    flush_runtime_commands(&worker.commands, pending)?;
    worker.ensure_running()?;
    thread::sleep(Duration::from_millis(10));
    Ok(())
}

fn drain_ui_commands(
    ui_commands: &Receiver<RuntimeCommand>,
    runtime_commands: &SyncSender<RuntimeCommand>,
    pending: &mut std::collections::VecDeque<RuntimeCommand>,
    target_entity: &str,
    allow_target_tap: bool,
    tap_forwarded: &mut bool,
) -> Result<(), PreviewError> {
    loop {
        match ui_commands.try_recv() {
            Ok(RuntimeCommand::SettingsApplied(revision)) => {
                pending.push_back(RuntimeCommand::SettingsApplied(revision));
            }
            Ok(RuntimeCommand::Tap(entity_id))
                if allow_target_tap && !*tap_forwarded && entity_id == target_entity =>
            {
                pending.push_back(RuntimeCommand::Tap(entity_id));
                *tap_forwarded = true;
            }
            Ok(RuntimeCommand::FetchImage { entity_id, path }) => {
                pending.push_back(RuntimeCommand::FetchImage { entity_id, path });
            }
            Ok(RuntimeCommand::Tap(_) | RuntimeCommand::Action { .. } | RuntimeCommand::Stop) => {
                return Err(PreviewError::UnexpectedCommand);
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
    flush_runtime_commands(runtime_commands, pending)
}

fn flush_runtime_commands(
    runtime_commands: &SyncSender<RuntimeCommand>,
    pending: &mut std::collections::VecDeque<RuntimeCommand>,
) -> Result<(), PreviewError> {
    while let Some(command) = pending.pop_front() {
        match runtime_commands.try_send(command) {
            Ok(()) => {}
            Err(TrySendError::Full(command)) => {
                pending.push_front(command);
                return Ok(());
            }
            Err(TrySendError::Disconnected(_)) => return Err(PreviewError::RuntimeStopped),
        }
    }
    Ok(())
}

struct LiveTarget {
    room_index: usize,
    room_entity_count: usize,
    entity_index: usize,
    room_name: String,
    state_before: String,
}

fn find_live_target(
    state: &Rc<RefCell<ViewState>>,
    entity_id: &str,
    room_id: &str,
) -> Result<LiveTarget, PreviewError> {
    let state = state.borrow();
    let room_id = parse_room_id(room_id);
    let dashboard = state
        .dashboard
        .as_ref()
        .ok_or(PreviewError::InvalidTarget("live dashboard is unavailable"))?;
    let (room_index, room) = dashboard
        .rooms
        .iter()
        .enumerate()
        .find(|(_, room)| room.id == room_id)
        .ok_or(PreviewError::InvalidTarget(
            "requested room is absent from HA",
        ))?;
    let entity_index = room
        .entities
        .iter()
        .position(|tile| tile.entity_id == entity_id)
        .ok_or(PreviewError::InvalidTarget(
            "requested entity is absent from the selected HA room",
        ))?;
    let tile = &room.entities[entity_index];
    let state_before = tile
        .state
        .as_ref()
        .ok_or(PreviewError::InvalidTarget("target has no HA state"))?
        .state
        .clone();
    if !matches!(tile.domain, EntityDomain::Light | EntityDomain::Switch)
        || !matches!(state_before.as_str(), "on" | "off")
        || !tile.command_enabled
        || tile.is_stale
    {
        return Err(PreviewError::InvalidTarget(
            "target must be an available, ready light or switch in on/off state",
        ));
    }
    Ok(LiveTarget {
        room_index,
        room_entity_count: room.entities.len(),
        entity_index,
        room_name: room.name.clone(),
        state_before,
    })
}

fn room_has_target(room: &njorun_app_core::Room, room_id: &str, entity_id: &str) -> bool {
    room.id == parse_room_id(room_id)
        && room.entities.iter().any(|tile| tile.entity_id == entity_id)
}

fn parse_room_id(room_id: &str) -> RoomId {
    if room_id == "unassigned" {
        RoomId::Unassigned
    } else {
        RoomId::Area(room_id.to_owned())
    }
}

fn live_target_state(
    state: &Rc<RefCell<ViewState>>,
    entity_id: &str,
) -> Result<String, PreviewError> {
    state
        .borrow()
        .dashboard
        .as_ref()
        .and_then(|dashboard| dashboard.entity(entity_id))
        .and_then(|tile| tile.state.as_ref())
        .map(|entity| entity.state.clone())
        .ok_or(PreviewError::InvalidTarget("target disappeared from HA"))
}

fn wake_live_display(ui: &MainWindow, state: &Rc<RefCell<ViewState>>) {
    let mut state = state.borrow_mut();
    let now = state.now_ms();
    state.display_controller.wake(now);
    state.update_display();
    render(ui, &state);
}

fn verify_room_render(
    ui: &MainWindow,
    entity_index: usize,
    entity_id: &str,
) -> Result<(), PreviewError> {
    if ui.get_tiles().row_count() <= entity_index
        || ui
            .get_tiles()
            .row_data(entity_index)
            .is_none_or(|tile| tile.entity_id.as_str() != entity_id)
    {
        return Err(PreviewError::InvalidTarget(
            "Slint did not render the requested entity in the selected room",
        ));
    }
    Ok(())
}

// Floor is intentional and the result is clamped to a positive u16-sized grid bound.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn grid_cell_count(available: f32, pitch: f32, maximum: u16) -> usize {
    usize::from((available / pitch).floor().clamp(1.0, f32::from(maximum)) as u16)
}

fn locate_action_target(
    ui: &MainWindow,
    window: &MinimalSoftwareWindow,
    entity_index: usize,
    entity_count: usize,
) -> Result<slint::LogicalPosition, PreviewError> {
    let size = window.size();
    let scale = ui.window().scale_factor().max(f32::EPSILON);
    let width = f32::from(u16::try_from(size.width).unwrap_or(u16::MAX)) / scale;
    let height = f32::from(u16::try_from(size.height).unwrap_or(u16::MAX)) / scale;
    let landscape = width > height && width >= 900.0;
    let header_height = if height < 360.0 {
        0.0
    } else if height < 440.0 {
        56.0
    } else {
        76.0
    };
    let (grid_left, grid_top, grid_width, grid_height) = if landscape {
        // MainWindow reserves 24 px side padding, a 224 px room list, a 1 px divider and
        // 18 px gutters on each side of that divider.
        let grid_top = header_height + 58.0 + 14.0;
        (285.0, grid_top, width - 309.0, height - grid_top - 24.0)
    } else {
        let tabs_height = if height >= 440.0 { 58.0 + 12.0 } else { 0.0 };
        let title_height = if height < 440.0 { 56.0 } else { 48.0 };
        let grid_top = header_height + tabs_height + title_height + 12.0;
        (20.0, grid_top, width - 40.0, height - grid_top - 20.0)
    };
    let gap = 14.0_f32;
    let columns = grid_cell_count(grid_width + gap, 320.0 + gap, 3);
    let row_height = 120.0_f32;
    let row_step = row_height + gap;
    let rows_unpaged = grid_cell_count(grid_height + gap, row_step, 512);
    let paged = entity_count > columns * rows_unpaged;
    let rows = if paged {
        grid_cell_count(grid_height - 64.0 + gap, row_step, 512)
    } else {
        rows_unpaged
    };
    let per_page = columns * rows;
    let page = entity_index / per_page;
    let page_count = entity_count.div_ceil(per_page);
    if ui.get_tiles().row_count() != entity_count || entity_index >= entity_count {
        return Err(PreviewError::InvalidTarget(
            "dashboard tile count changed while locating the live target",
        ));
    }
    if paged {
        let swipe_y = grid_top + ((grid_height - 64.0) / 2.0).max(24.0);
        for _ in 0..page {
            dispatch_swipe(
                ui,
                grid_width.mul_add(0.8, grid_left),
                grid_width.mul_add(0.2, grid_left),
                swipe_y,
            );
        }
    }

    let page_index = entity_index % per_page;
    let row = page_index / columns;
    let column = page_index % columns;
    let margin = if paged { gap + 22.0 } else { 0.0 };
    let numeric = |count| f32::from(u16::try_from(count).expect("bounded grid cell index"));
    let cell_width = gap.mul_add(
        -numeric(columns.saturating_sub(1)),
        (-2.0_f32).mul_add(margin, grid_width),
    ) / numeric(columns);
    // The 56 px action is inset 16 px from the card edge; target its center.
    let x = numeric(column).mul_add(cell_width + gap, grid_left + margin) + cell_width - 44.0;
    let y = numeric(row).mul_add(row_step, grid_top) + row_height / 2.0;
    let content_bottom = grid_top + grid_height - if paged { 64.0 } else { 0.0 };
    if page >= page_count
        || !(0.0..width).contains(&x)
        || !(grid_top..content_bottom).contains(&y)
        || y >= height
    {
        return Err(PreviewError::InvalidTarget(
            "target action button falls outside the configured display",
        ));
    }
    Ok(slint::LogicalPosition::new(x, y))
}

fn render_live_frame(window: &MinimalSoftwareWindow) -> Result<PreviewFrame, PreviewError> {
    let size = window.size();
    let width = usize::try_from(size.width)
        .map_err(|_| PreviewError::InvalidTarget("invalid rendered width"))?;
    let height = usize::try_from(size.height)
        .map_err(|_| PreviewError::InvalidTarget("invalid rendered height"))?;
    let pixel_count = width
        .checked_mul(height)
        .ok_or(PreviewError::InvalidTarget("rendered frame size overflow"))?;
    let mut pixels = vec![PreviewPixel::default(); pixel_count];
    window.request_redraw();
    if !window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, width);
    }) {
        return Err(PreviewError::RenderUnavailable);
    }
    Ok(PreviewFrame {
        name: "live-ha-ui".into(),
        width: size.width,
        height: size.height,
        pixels,
    })
}

fn install_preview_platform() -> Result<(), PreviewError> {
    PREVIEW_PLATFORM_INSTALLED.with(|installed| {
        if installed.get() {
            return Ok(());
        }
        slint::platform::set_platform(Box::new(PreviewPlatform))
            .map_err(PreviewError::PlatformAlreadyInstalled)?;
        PREVIEW_CLOCK.with(|clock| clock.set(Duration::ZERO));
        installed.set(true);
        Ok(())
    })
}

/// Render the D1001 and Guition resolution fixtures with the real Slint software renderer.
///
/// This standalone helper installs an offscreen Slint platform and must run before any other
/// Slint platform is initialized in the process. It returns room overview, room tiles, adaptive
/// Details, Favorites, and Mixed frames for both themes, portrait and landscape.
///
/// # Errors
///
/// Returns an error if Slint is already initialized, the window cannot be shown, the fixture is
/// invalid, or the renderer does not produce a frame.
pub fn render_qualification_previews() -> Result<Vec<PreviewFrame>, PreviewError> {
    let mut frames = Vec::new();
    render_qualification_previews_with(|frame| {
        frames.push(frame);
        Ok(())
    })?;
    Ok(frames)
}

/// Render qualification frames one at a time so a preview writer need not retain the full matrix.
///
/// The renderer calls `consume` after each frame is complete; the frame is dropped before the
/// next display is rendered when the callback writes it directly to disk.
///
/// # Errors
///
/// Returns an error if Slint is already initialized, a fixture fails to render, or the callback
/// cannot consume a frame.
pub fn render_qualification_previews_with(
    mut consume: impl FnMut(PreviewFrame) -> Result<(), Box<dyn Error>>,
) -> Result<(), PreviewError> {
    install_preview_platform()?;
    let window = PREVIEW_WINDOW.with(Rc::clone);
    render_commissioning_frames(&window, &mut consume)?;
    let ui = MainWindow::new().map_err(PreviewError::Window)?;
    ui.show().map_err(PreviewError::Window)?;
    let mut dashboard = preview_dashboard().map_err(PreviewError::Fixture)?;
    for tile in dashboard
        .rooms
        .iter_mut()
        .flat_map(|room| &mut room.entities)
    {
        if tile.entity_id == "light.living_room_lamp" {
            "Wohnzimmerbeleuchtung am großen Lesesessel neben der Terrassentür"
                .clone_into(&mut tile.name);
        }
    }
    for display in DisplayFixture::ALL {
        for theme in [PreviewTheme::Light, PreviewTheme::Dark] {
            for content in PreviewContent::ALL {
                consume_frame(
                    &mut consume,
                    render_preview_frame(&ui, &window, &dashboard, display, theme, content, None)?,
                )?;
            }
        }
    }
    // Keep the state/display baseline above separate from this small palette matrix. Every
    // combination is rendered in both orientations, while the complete set of dashboard states
    // remains covered on all four physical displays without retaining every combination in RAM.
    for display in [DisplayFixture::ALL[0], DisplayFixture::ALL[1]] {
        for theme in [PreviewTheme::Light, PreviewTheme::Dark] {
            for scheme in ["njorun", "graphite", "warm", "porcelain"] {
                for increased_contrast in [false, true] {
                    for reduced_motion in [false, true] {
                        consume_frame(
                            &mut consume,
                            render_preview_frame(
                                &ui,
                                &window,
                                &dashboard,
                                display,
                                theme,
                                PreviewContent::Mixed,
                                Some(PreviewAppearance {
                                    scheme,
                                    increased_contrast,
                                    reduced_motion,
                                }),
                            )?,
                        )?;
                    }
                }
            }
        }
    }
    render_daylight_palette_frames(&ui, &window, &dashboard, &mut consume)?;
    render_domain_detail_frames(&ui, &window, &mut consume)?;
    render_settings_overlay_frames(&ui, &window, &mut consume)?;
    Ok(())
}

fn consume_frame(
    consume: &mut impl FnMut(PreviewFrame) -> Result<(), Box<dyn Error>>,
    frame: PreviewFrame,
) -> Result<(), PreviewError> {
    consume(frame).map_err(PreviewError::Output)
}

fn render_daylight_palette_frames(
    ui: &MainWindow,
    window: &MinimalSoftwareWindow,
    dashboard: &Dashboard,
    consume: &mut impl FnMut(PreviewFrame) -> Result<(), Box<dyn Error>>,
) -> Result<(), PreviewError> {
    let display = DisplayFixture::ALL[0];
    let mut settings = preview_settings(
        display,
        PreviewTheme::Light,
        PreviewContent::Mixed,
        "porcelain",
        true,
        true,
    );
    settings.theme = "automatic".into();
    settings.light_color_scheme = "porcelain".into();
    settings.dark_color_scheme = "warm".into();
    let mut view_state = ViewState::with_settings(AppliedSettings {
        revision: 0,
        settings,
    });
    view_state.dashboard = Some(preview_snapshot(dashboard, PreviewContent::Mixed));
    view_state.selected_room = Some(RoomId::Area("living_room".into()));
    view_state.daylight = false;
    let state = Rc::new(RefCell::new(view_state));
    apply_settings(
        ui,
        &state.borrow(),
        PhysicalSize::new(display.width, display.height),
    );
    render(ui, &state.borrow());

    for (daylight, dark, scheme, suffix) in [
        (false, true, "warm", "automatic-night-warm"),
        (true, false, "porcelain", "automatic-day-porcelain"),
    ] {
        apply_runtime_event(
            ui,
            &state,
            PhysicalSize::new(display.width, display.height),
            RuntimeEvent::Daylight(daylight),
        );
        if ui.get_dark_mode() != dark
            || ui.get_color_scheme().as_str() != scheme
            || !ui.get_increased_contrast()
            || !ui.get_reduced_motion()
        {
            return Err(PreviewError::Interaction(
                "Daylight event did not resolve its mode palette with contrast and reduced motion",
            ));
        }
        let name = format!("{}-{suffix}-contrast-high-motion-reduced", display.name);
        consume_frame(consume, draw_preview_frame(window, display, name)?)?;
    }
    Ok(())
}

fn render_domain_detail_frames(
    ui: &MainWindow,
    window: &MinimalSoftwareWindow,
    consume: &mut impl FnMut(PreviewFrame) -> Result<(), Box<dyn Error>>,
) -> Result<(), PreviewError> {
    const DOMAIN_DETAILS: [(&str, &str, bool, bool); 12] = [
        ("light.living_room_lamp", "slider", true, true),
        ("switch.wall_switch", "none", true, true),
        ("fan.ceiling", "slider", true, true),
        ("number.target_temperature", "slider", false, true),
        ("select.hvac_mode", "options", false, true),
        ("lock.front_door", "lock", false, true),
        ("alarm_control_panel.home", "alarm", false, true),
        ("media_player.kitchen", "media", false, true),
        ("camera.driveway", "camera", false, true),
        ("automation.morning", "automation", true, true),
        ("script.goodnight", "none", false, true),
        ("sensor.room_temperature", "none", false, true),
    ];
    let dashboard = domain_preview_dashboard().map_err(PreviewError::Fixture)?;
    for display in [DisplayFixture::ALL[0], DisplayFixture::ALL[1]] {
        for theme in [PreviewTheme::Light, PreviewTheme::Dark] {
            for (entity_id, control, can_toggle, controls_enabled) in DOMAIN_DETAILS {
                let mut state = ViewState::with_settings(AppliedSettings {
                    revision: 0,
                    settings: preview_settings(
                        display,
                        theme,
                        PreviewContent::Details,
                        "njorun",
                        false,
                        false,
                    ),
                });
                state.dashboard = Some(dashboard.clone());
                state.selected_room = Some(RoomId::Area("living_room".into()));
                state.selected_entity = Some(entity_id.into());
                apply_settings(ui, &state, PhysicalSize::new(display.width, display.height));
                render(ui, &state);
                let details = ui.get_details_data();
                if details.entity_id.as_str() != entity_id
                    || details.control.as_str() != control
                    || details.can_toggle != can_toggle
                    || details.controls_enabled != controls_enabled
                {
                    return Err(PreviewError::Interaction(
                        "domain details did not use the presentation model mapping",
                    ));
                }
                let name = format!(
                    "{}-{}-details-{}",
                    display.name,
                    theme.name(),
                    entity_id.replace('.', "-")
                );
                consume_frame(consume, draw_preview_frame(window, display, name)?)?;
            }
        }
    }

    render_disabled_detail_frame(ui, window, &dashboard, consume, DetailState::Stale)?;
    render_disabled_detail_frame(ui, window, &dashboard, consume, DetailState::Unavailable)?;
    render_disabled_detail_frame(ui, window, &dashboard, consume, DetailState::Rejected)?;
    Ok(())
}

#[derive(Clone, Copy)]
enum DetailState {
    Stale,
    Unavailable,
    Rejected,
}

fn render_disabled_detail_frame(
    ui: &MainWindow,
    window: &MinimalSoftwareWindow,
    dashboard: &Dashboard,
    consume: &mut impl FnMut(PreviewFrame) -> Result<(), Box<dyn Error>>,
    condition: DetailState,
) -> Result<(), PreviewError> {
    let display = DisplayFixture::ALL[0];
    let mut snapshot = dashboard.clone();
    let tile = snapshot
        .rooms
        .iter_mut()
        .flat_map(|room| &mut room.entities)
        .find(|tile| tile.entity_id == "light.living_room_lamp")
        .ok_or(PreviewError::Interaction(
            "light detail fixture disappeared",
        ))?;
    let (name, frame_name) = match condition {
        DetailState::Stale => {
            tile.is_stale = true;
            tile.command_enabled = false;
            ("stale-light", "stale")
        }
        DetailState::Unavailable => {
            if let Some(state) = tile.state.as_mut() {
                state.state = "unavailable".into();
            }
            tile.command_enabled = false;
            ("unavailable-light", "unavailable")
        }
        DetailState::Rejected => {
            tile.action_feedback = Some(ActionFeedback::Rejected {
                reason: "Home Assistant rejected the requested change".into(),
            });
            ("rejected-light", "rejected")
        }
    };
    let theme = PreviewTheme::Light;
    let mut state = ViewState::with_settings(AppliedSettings {
        revision: 0,
        settings: preview_settings(
            display,
            theme,
            PreviewContent::Details,
            "njorun",
            false,
            false,
        ),
    });
    state.dashboard = Some(snapshot);
    state.selected_room = Some(RoomId::Area("living_room".into()));
    state.selected_entity = Some("light.living_room_lamp".into());
    apply_settings(ui, &state, PhysicalSize::new(display.width, display.height));
    render(ui, &state);
    let details = ui.get_details_data();
    if !details.can_toggle || details.controls_enabled != matches!(condition, DetailState::Rejected)
    {
        return Err(PreviewError::Interaction(
            "stale/unavailable detail controls were not disabled",
        ));
    }
    consume_frame(
        consume,
        draw_preview_frame(
            window,
            display,
            format!("{}-details-{name}-{frame_name}", display.name),
        )?,
    )
}

fn domain_preview_dashboard() -> Result<Dashboard, njorun_app_core::CoreError> {
    let mut dashboard = preview_dashboard()?;
    let living_room = dashboard
        .rooms
        .iter_mut()
        .find(|room| room.id == RoomId::Area("living_room".into()))
        .expect("demo living room exists");
    living_room.entities.extend([
        domain_tile("switch.wall_switch", EntityDomain::Switch, "off", []),
        domain_tile(
            "fan.ceiling",
            EntityDomain::Fan,
            "on",
            [
                ("percentage", serde_json::json!(58)),
                ("percentage_step", serde_json::json!(2)),
            ],
        ),
        domain_tile(
            "number.target_temperature",
            EntityDomain::Number,
            "21.5",
            [
                ("min", serde_json::json!(16)),
                ("max", serde_json::json!(30)),
                ("step", serde_json::json!(0.5)),
                ("unit_of_measurement", serde_json::json!("°C")),
            ],
        ),
        domain_tile(
            "select.hvac_mode",
            EntityDomain::Select,
            "heat",
            [(
                "options",
                serde_json::json!(["heat", "cool", "auto", "off"]),
            )],
        ),
        domain_tile("lock.front_door", EntityDomain::Lock, "locked", []),
        domain_tile(
            "alarm_control_panel.home",
            EntityDomain::AlarmControlPanel,
            "armed_home",
            [("code_format", serde_json::json!("number"))],
        ),
        domain_tile(
            "media_player.kitchen",
            EntityDomain::MediaPlayer,
            "playing",
            [
                (
                    "supported_features",
                    serde_json::json!(1 | 4 | 8 | 16 | 32 | 512 | 16_384),
                ),
                ("volume_level", serde_json::json!(0.42)),
                ("media_title", serde_json::json!("A quiet Sunday")),
                ("media_artist", serde_json::json!("Local Radio")),
            ],
        ),
        domain_tile("camera.driveway", EntityDomain::Camera, "idle", []),
        domain_tile("automation.morning", EntityDomain::Automation, "on", []),
        domain_tile("script.goodnight", EntityDomain::Script, "off", []),
        domain_tile(
            "sensor.room_temperature",
            EntityDomain::Sensor,
            "20.8",
            [("unit_of_measurement", serde_json::json!("°C"))],
        ),
    ]);
    Ok(dashboard)
}

fn domain_tile<const N: usize>(
    entity_id: &str,
    domain: EntityDomain,
    value: &str,
    attributes: [(&str, serde_json::Value); N],
) -> EntityTile {
    let name = match entity_id {
        "switch.wall_switch" => "Wall switch",
        "fan.ceiling" => "Ceiling fan",
        "number.target_temperature" => "Target temperature",
        "select.hvac_mode" => "Heating mode",
        "lock.front_door" => "Front door",
        "alarm_control_panel.home" => "Home alarm",
        "media_player.kitchen" => "Kitchen speaker",
        "camera.driveway" => "Driveway camera",
        "automation.morning" => "Morning routine",
        "script.goodnight" => "Good night",
        "sensor.room_temperature" => "Room temperature",
        _ => entity_id,
    };
    let mut data = BTreeMap::from([
        ("friendly_name".into(), serde_json::json!(entity_id)),
        ("icon".into(), serde_json::json!("mdi:home")),
    ]);
    data.extend(
        attributes
            .into_iter()
            .map(|(key, value)| (key.into(), value)),
    );
    EntityTile {
        entity_id: entity_id.into(),
        name: name.into(),
        icon: None,
        domain,
        state: Some(HaEntityState {
            entity_id: entity_id.into(),
            state: value.into(),
            attributes: data,
            last_updated: Some("2026-11-14T09:41:00+00:00".into()),
        }),
        is_stale: false,
        command_enabled: true,
        action_feedback: None,
    }
}

fn render_settings_overlay_frames(
    ui: &MainWindow,
    window: &MinimalSoftwareWindow,
    consume: &mut impl FnMut(PreviewFrame) -> Result<(), Box<dyn Error>>,
) -> Result<(), PreviewError> {
    for display in [DisplayFixture::ALL[0], DisplayFixture::ALL[1]] {
        for theme in [PreviewTheme::Light, PreviewTheme::Dark] {
            let mut state = ViewState::with_settings(AppliedSettings {
                revision: 0,
                settings: preview_settings(
                    display,
                    theme,
                    PreviewContent::Mixed,
                    "njorun",
                    false,
                    false,
                ),
            });
            state.dashboard = Some(preview_dashboard().map_err(PreviewError::Fixture)?);
            state.selected_room = Some(RoomId::Area("living_room".into()));
            apply_settings(ui, &state, PhysicalSize::new(display.width, display.height));
            render(ui, &state);
            ui.set_allow_forget(true);
            ui.set_network_link("wifi3".into());
            ui.set_connection_text("Home Assistant is connected over Wi-Fi".into());
            ui.set_inspect_status(true);
            let light_name = format!("{}-{}-settings-overlay", display.name, theme.name());
            let normal = draw_preview_frame(window, display, light_name)?;

            dispatch_click(
                ui,
                slint::LogicalPosition::new(
                    if display.orientation == "portrait" {
                        566.0
                    } else {
                        1106.0
                    },
                    296.0,
                ),
            );
            let reset_name = format!("{}-{}-settings-reset-armed", display.name, theme.name());
            let armed = draw_preview_frame(window, display, reset_name)?;
            if normal.pixels == armed.pixels {
                return Err(PreviewError::Interaction(
                    "Reset setup preview did not enter its armed state",
                ));
            }
            consume_frame(consume, normal)?;
            consume_frame(consume, armed)?;
            dispatch_click(
                ui,
                slint::LogicalPosition::new(
                    if display.orientation == "portrait" {
                        748.0
                    } else {
                        1228.0
                    },
                    182.0,
                ),
            );
            if ui.get_inspect_status() {
                return Err(PreviewError::Interaction("settings preview did not close"));
            }
            ui.set_allow_forget(false);
        }
    }
    Ok(())
}

fn draw_preview_frame(
    window: &MinimalSoftwareWindow,
    display: DisplayFixture,
    name: impl Into<String>,
) -> Result<PreviewFrame, PreviewError> {
    let width = usize::try_from(display.width).expect("fixture width fits usize");
    let height = usize::try_from(display.height).expect("fixture height fits usize");
    let mut pixels = vec![PreviewPixel::default(); width * height];
    window.request_redraw();
    if !window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, width);
    }) {
        return Err(PreviewError::RenderUnavailable);
    }
    Ok(PreviewFrame {
        name: name.into(),
        width: display.width,
        height: display.height,
        pixels,
    })
}

/// First-run, network, pairing and login states on every supported display.
fn render_commissioning_frames(
    window: &MinimalSoftwareWindow,
    consume: &mut impl FnMut(PreviewFrame) -> Result<(), Box<dyn Error>>,
) -> Result<(), PreviewError> {
    let ui = CommissioningWindow::new().map_err(PreviewError::Window)?;
    ui.show().map_err(PreviewError::Window)?;
    set_commissioning_fixtures(&ui);
    render_commissioning_baseline(&ui, window, consume)?;
    render_commissioning_palette_matrix(&ui, window, consume)?;
    render_commissioning_error_states(&ui, window, consume)?;
    ui.hide().map_err(PreviewError::Window)
}

const COMMISSIONING_STAGES: [&str; 16] = [
    "boot",
    "theme",
    "wifi",
    "password",
    "connecting",
    "ip-address",
    "ip-prefix",
    "ip-gateway",
    "ip-dns",
    "discovery",
    "discovering",
    "manual",
    "authorizing",
    "pairing",
    "loading",
    "browser",
];

fn set_commissioning_fixtures(ui: &CommissioningWindow) {
    let network = |name: &str, signal, secured| WifiItem {
        name: name.into(),
        signal,
        secured,
    };
    ui.set_wifi_networks(slint::ModelRc::new(slint::VecModel::from(vec![
        network("Schmieder24", 4, true),
        network("Guest", 3, false),
        network("Neighbour", 1, true),
    ])));
    ui.set_instances(slint::ModelRc::new(slint::VecModel::from(vec![
        InstanceItem {
            name: "Home".into(),
            endpoint: "https://home.example".into(),
        },
        InstanceItem {
            name: "Cabin".into(),
            endpoint: "http://192.168.2.20:8123".into(),
        },
    ])));
    let mut seed = 7_u32;
    let modules: Vec<bool> = (0..25 * 25)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed >> 24 & 1 == 1
        })
        .collect();
    ui.set_qr_modules(slint::ModelRc::new(slint::VecModel::from(modules)));
    ui.set_qr_size(25);
    ui.set_selected_wifi("Schmieder24".into());
    ui.set_selected_instance("https://home.example".into());
    ui.set_login_code("A1B2".into());
    ui.set_pairing_name("Njörun panel 031".into());
    ui.set_pairing_code("360 971".into());
    ui.set_clock_time("9:41".into());
}

fn render_commissioning_baseline(
    ui: &CommissioningWindow,
    window: &MinimalSoftwareWindow,
    consume: &mut impl FnMut(PreviewFrame) -> Result<(), Box<dyn Error>>,
) -> Result<(), PreviewError> {
    for display in DisplayFixture::ALL {
        set_commissioning_display(ui, window, display);
        for theme in [PreviewTheme::Light, PreviewTheme::Dark] {
            set_commissioning_palette(ui, theme, "njorun", false, false);
            for stage in COMMISSIONING_STAGES {
                ui.set_stage(stage.into());
                ui.set_theme_choice(theme.settings_value().into());
                ui.set_error_message("".into());
                ui.set_input_value(setup_value(stage).into());
                ui.set_input_valid_url(stage == "manual");
                draw_commissioning_frame(
                    ui,
                    window,
                    display,
                    &format!("{}-{}-setup-{stage}", display.name, theme.name()),
                    consume,
                )?;
            }
        }
    }
    // Saved-profile boot/reconnect reuses the commissioning surface without OOBE chrome.
    for display in [DisplayFixture::ALL[0], DisplayFixture::ALL[1]] {
        set_commissioning_display(ui, window, display);
        ui.set_oobe(false);
        for theme in [PreviewTheme::Light, PreviewTheme::Dark] {
            set_commissioning_palette(ui, theme, "njorun", false, false);
            for stage in ["connecting", "loading"] {
                ui.set_stage(stage.into());
                ui.set_error_message("".into());
                draw_commissioning_frame(
                    ui,
                    window,
                    display,
                    &format!("{}-{}-restore-{stage}", display.name, theme.name()),
                    consume,
                )?;
            }
        }
        ui.set_oobe(true);
    }
    Ok(())
}

fn render_commissioning_palette_matrix(
    ui: &CommissioningWindow,
    window: &MinimalSoftwareWindow,
    consume: &mut impl FnMut(PreviewFrame) -> Result<(), Box<dyn Error>>,
) -> Result<(), PreviewError> {
    for display in [DisplayFixture::ALL[0], DisplayFixture::ALL[1]] {
        set_commissioning_display(ui, window, display);
        for theme in [PreviewTheme::Light, PreviewTheme::Dark] {
            for scheme in ["njorun", "graphite", "warm", "porcelain"] {
                for increased_contrast in [false, true] {
                    for reduced_motion in [false, true] {
                        set_commissioning_palette(
                            ui,
                            theme,
                            scheme,
                            increased_contrast,
                            reduced_motion,
                        );
                        ui.set_stage("theme".into());
                        ui.set_theme_choice(theme.settings_value().into());
                        let contrast = if increased_contrast {
                            "contrast-high"
                        } else {
                            "contrast-normal"
                        };
                        let motion = if reduced_motion {
                            "motion-reduced"
                        } else {
                            "motion-full"
                        };
                        draw_commissioning_frame(
                            ui,
                            window,
                            display,
                            &format!(
                                "{}-{}-{scheme}-{contrast}-{motion}-setup-theme",
                                display.name,
                                theme.name()
                            ),
                            consume,
                        )?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn render_commissioning_error_states(
    ui: &CommissioningWindow,
    window: &MinimalSoftwareWindow,
    consume: &mut impl FnMut(PreviewFrame) -> Result<(), Box<dyn Error>>,
) -> Result<(), PreviewError> {
    let display = DisplayFixture::ALL[0];
    set_commissioning_display(ui, window, display);
    set_commissioning_palette(ui, PreviewTheme::Light, "njorun", false, false);
    for (stage, error, suffix) in [
        (
            "wifi",
            "This network could not be joined. Check the password and try again.",
            "wifi-error",
        ),
        ("password", "The password was rejected.", "password-error"),
        (
            "discovery",
            "No Home Assistant installation was found.",
            "discovery-error",
        ),
        (
            "manual",
            "Enter a valid Home Assistant URL.",
            "manual-error",
        ),
        (
            "connecting",
            "The network could not be reached.",
            "connecting-error",
        ),
        (
            "pairing",
            "The approval expired. Try again.",
            "pairing-error",
        ),
        ("browser", "Browser approval timed out.", "browser-error"),
    ] {
        ui.set_stage(stage.into());
        ui.set_error_message(error.into());
        ui.set_input_value(
            if stage == "manual" {
                "homeassistant"
            } else {
                setup_value(stage)
            }
            .into(),
        );
        ui.set_input_valid_url(false);
        draw_commissioning_frame(
            ui,
            window,
            display,
            &format!("{}-setup-{suffix}", display.name),
            consume,
        )?;
    }

    ui.set_error_message("".into());
    ui.set_stage("password".into());
    ui.set_input_value("hunter2".into());
    let before_symbols = commissioning_pixels(ui, window, display)?;
    dispatch_commissioning_click(ui, 64.0, 1230.0);
    let pixels = commissioning_pixels(ui, window, display)?;
    if pixels == before_symbols {
        return Err(PreviewError::Interaction(
            "virtual keyboard symbol toggle did not change the password screen",
        ));
    }
    consume_frame(
        consume,
        PreviewFrame {
            name: "d1001-portrait-setup-password-symbols".into(),
            width: display.width,
            height: display.height,
            pixels,
        },
    )?;
    Ok(())
}

fn setup_value(stage: &str) -> &'static str {
    match stage {
        "password" => "hunter2",
        "manual" => "https://home.example",
        "ip-address" => "192.168.1.50",
        "ip-prefix" => "24",
        "ip-gateway" => "192.168.1.1",
        "ip-dns" => "1.1.1.1 8.8.8.8",
        _ => "",
    }
}

fn set_commissioning_display(
    ui: &CommissioningWindow,
    window: &MinimalSoftwareWindow,
    display: DisplayFixture,
) {
    window.set_size(PhysicalSize::new(display.width, display.height));
    ui.set_viewport_width(i32::try_from(display.width).expect("fixture width fits i32"));
    ui.set_viewport_height(i32::try_from(display.height).expect("fixture height fits i32"));
    ui.set_landscape(display.orientation == "landscape");
}

fn set_commissioning_palette(
    ui: &CommissioningWindow,
    theme: PreviewTheme,
    scheme: &str,
    increased_contrast: bool,
    reduced_motion: bool,
) {
    ui.set_dark_mode(matches!(theme, PreviewTheme::Dark));
    ui.set_color_scheme(scheme.into());
    ui.set_increased_contrast(increased_contrast);
    ui.set_reduced_motion(reduced_motion);
}

fn draw_commissioning_frame(
    ui: &CommissioningWindow,
    window: &MinimalSoftwareWindow,
    display: DisplayFixture,
    name: &str,
    consume: &mut impl FnMut(PreviewFrame) -> Result<(), Box<dyn Error>>,
) -> Result<(), PreviewError> {
    let pixels = commissioning_pixels(ui, window, display)?;
    consume_frame(
        consume,
        PreviewFrame {
            name: name.to_owned(),
            width: display.width,
            height: display.height,
            pixels,
        },
    )
}

fn commissioning_pixels(
    _ui: &CommissioningWindow,
    window: &MinimalSoftwareWindow,
    display: DisplayFixture,
) -> Result<Vec<PreviewPixel>, PreviewError> {
    let width = usize::try_from(display.width).expect("fixture width fits usize");
    let height = usize::try_from(display.height).expect("fixture height fits usize");
    let mut pixels = vec![PreviewPixel::default(); width * height];
    window.request_redraw();
    if !window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, width);
    }) {
        return Err(PreviewError::RenderUnavailable);
    }
    Ok(pixels)
}

fn dispatch_commissioning_click(ui: &CommissioningWindow, x: f32, y: f32) {
    use slint::platform::{PointerEventButton, WindowEvent};
    for event in [
        WindowEvent::PointerPressed {
            position: slint::LogicalPosition::new(x, y),
            button: PointerEventButton::Left,
        },
        WindowEvent::PointerReleased {
            position: slint::LogicalPosition::new(x, y),
            button: PointerEventButton::Left,
        },
    ] {
        ui.window().dispatch_event(event);
        slint::platform::update_timers_and_animations();
    }
}

/// Exercise the real dashboard callbacks and Slint pointer/timer path offscreen.
///
/// This verifies one short tap, hold-to-details with a consumed release, movement cancellation,
/// details dismissal, consumed wake touches for both display-off and clock idle, full-queue
/// nonblocking behavior, and retry of a `SettingsApplied` acknowledgement after queue space
/// becomes available. It uses the same `MainWindow`, callbacks, Slint timers, and command channel
/// logic as `run`.
///
/// # Errors
///
/// Returns an error if a Slint platform is already active or any interaction differs from its
/// expected command/details state.
pub fn qualify_ui_interactions() -> Result<(), PreviewError> {
    {
        let harness = interaction_harness()?;
        qualify_tap_hold_scroll(&harness)?;
        qualify_tile_semantics(&harness)?;
        qualify_paged_grid(&harness)?;
        qualify_update_during_touch(&harness)?;
        qualify_display_wake(&harness)?;
        qualify_full_command_queue(&harness)?;
        qualify_stale_and_unavailable_states(&harness)?;
        qualify_status_panel(&harness)?;
        qualify_daylight_palette_resolution(&harness)?;
        qualify_large_detail_scroll(&harness)?;
        qualify_action_feedback(&harness)?;
        qualify_wake_and_navigation_callbacks(&harness)?;
        qualify_dashboard_reconciliation(&harness)?;
        qualify_settings_acknowledgement(&harness)?;
    }
    qualify_disconnected_command_queue()?;
    qualify_runtime_event_disconnect()
}

struct InteractionHarness {
    ui: MainWindow,
    state: Rc<RefCell<ViewState>>,
    commands: SyncSender<RuntimeCommand>,
    received: Receiver<RuntimeCommand>,
    size: PhysicalSize,
}

fn interaction_harness() -> Result<InteractionHarness, PreviewError> {
    install_preview_platform()?;
    let ui = MainWindow::new().map_err(PreviewError::Window)?;
    let size = PhysicalSize::new(800, 1280);
    let dashboard = preview_dashboard().map_err(PreviewError::Fixture)?;
    let mut view_state = ViewState::with_settings(AppliedSettings {
        settings: Settings::default(),
        ..AppliedSettings::default()
    });
    view_state.dashboard = Some(dashboard);
    view_state.selected_room = Some(RoomId::Area("living_room".into()));
    let state = Rc::new(RefCell::new(view_state));
    ui.window().set_size(size);
    render(&ui, &state.borrow());
    ui.show().map_err(PreviewError::Window)?;

    let (commands, received) = std::sync::mpsc::sync_channel(1);
    install_callbacks(&ui, &state, commands.clone());
    Ok(InteractionHarness {
        ui,
        state,
        commands,
        received,
        size,
    })
}

fn qualify_display_wake(harness: &InteractionHarness) -> Result<(), PreviewError> {
    let InteractionHarness {
        ui,
        state,
        received,
        ..
    } = harness;
    {
        let mut state = state.borrow_mut();
        state.display_controller.turn_off();
        state.update_display();
        render(ui, &state);
    }
    if !ui.get_display_off() {
        return Err(PreviewError::Interaction(
            "manual display-off state did not cover the dashboard",
        ));
    }
    let action_point = slint::LogicalPosition::new(365.0, 270.0);
    dispatch_click(ui, action_point);
    if ui.get_display_off() {
        return Err(PreviewError::Interaction(
            "first touch did not wake the manually-off display",
        ));
    }
    expect_no_command(received, "wake touch activated an entity")?;
    dispatch_click(ui, action_point);
    expect_tap(received).and_then(|()| qualify_clock_wake(harness, action_point))
}

fn qualify_clock_wake(
    harness: &InteractionHarness,
    action_point: slint::LogicalPosition,
) -> Result<(), PreviewError> {
    let InteractionHarness {
        ui,
        state,
        received,
        ..
    } = harness;
    {
        let mut state = state.borrow_mut();
        state.settings.settings.screensaver = "clock".into();
        state.settings.settings.idle_timeout = 10;
        state.settings.settings.idle_brightness = 12;
        state.started_at = Instant::now().checked_sub(Duration::from_secs(11)).ok_or(
            PreviewError::Interaction("could not advance the idle test clock"),
        )?;
        state.display_controller = DisplayController::new(0);
        state.update_display();
        render(ui, &state);
    }
    if !ui.get_show_clock() {
        return Err(PreviewError::Interaction(
            "clock timeout did not show the idle screen",
        ));
    }
    dispatch_click(ui, action_point);
    if ui.get_show_clock() {
        return Err(PreviewError::Interaction(
            "first touch did not wake the clock screen",
        ));
    }
    expect_no_command(received, "clock wake touch activated an entity")?;
    dispatch_click(ui, action_point);
    expect_tap(received)
}

fn qualify_update_during_touch(harness: &InteractionHarness) -> Result<(), PreviewError> {
    use slint::platform::{PointerEventButton, WindowEvent};
    let point = slint::LogicalPosition::new(150.0, 270.0);
    let press = || {
        dispatch_pointer(
            &harness.ui,
            WindowEvent::PointerPressed {
                position: point,
                button: PointerEventButton::Left,
            },
        );
    };
    let release = || {
        dispatch_pointer(
            &harness.ui,
            WindowEvent::PointerReleased {
                position: point,
                button: PointerEventButton::Left,
            },
        );
    };
    press();
    {
        let mut state = harness.state.borrow_mut();
        let tile = state
            .dashboard
            .as_mut()
            .and_then(|dashboard| dashboard_entity_mut(dashboard, "sensor.living_room_temperature"))
            .ok_or(PreviewError::Interaction("fixture sensor disappeared"))?;
        "Kitchen sensor".clone_into(&mut tile.name);
        render(&harness.ui, &state);
    }
    release();
    if !harness.ui.get_details_open()
        || harness.ui.get_details_data().entity_id.as_str() != "light.living_room_lamp"
    {
        return Err(PreviewError::Interaction(
            "body tap did not open details for the control held before a name update",
        ));
    }
    expect_no_command(
        &harness.received,
        "a control-body details tap queued a command after a name update",
    )?;
    harness.ui.invoke_dismiss_details();
    press();
    {
        let mut state = harness.state.borrow_mut();
        let room = state
            .dashboard
            .as_mut()
            .and_then(|dashboard| {
                dashboard
                    .rooms
                    .iter_mut()
                    .find(|room| room.id == RoomId::Area("living_room".into()))
            })
            .ok_or(PreviewError::Interaction("fixture room disappeared"))?;
        room.entities.swap(0, 1);
        render(&harness.ui, &state);
    }
    release();
    expect_no_command(
        &harness.received,
        "list reordered during touch and activated a card",
    )?;
    if harness.ui.get_details_open() {
        return Err(PreviewError::Interaction(
            "list reorder during touch left a stale details target active",
        ));
    }
    let dashboard = preview_dashboard().map_err(PreviewError::Fixture)?;
    {
        let mut state = harness.state.borrow_mut();
        state.dashboard = Some(dashboard);
        render(&harness.ui, &state);
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // One pointer sequence verifies tap, hold, scroll and reorder safety.
fn qualify_tap_hold_scroll(harness: &InteractionHarness) -> Result<(), PreviewError> {
    let InteractionHarness { ui, received, .. } = harness;
    let body_point = slint::LogicalPosition::new(150.0, 270.0);
    let action_point = slint::LogicalPosition::new(365.0, 270.0);
    let tiles = ui.get_tiles();
    for (entity_id, active, kind, action_enabled) in [
        ("light.living_room_lamp", true, "control", true),
        ("switch.reading_light", false, "control", true),
        ("scene.movie_time", false, "scene", true),
        (
            "sensor.living_room_temperature",
            false,
            "measurement",
            false,
        ),
    ] {
        if !(0..tiles.row_count())
            .filter_map(|index| tiles.row_data(index))
            .any(|tile| {
                tile.entity_id.as_str() == entity_id
                    && tile.active == active
                    && tile.kind.as_str() == kind
                    && tile.action_enabled == action_enabled
            })
        {
            return Err(PreviewError::Interaction(
                "tile kind, action availability or active state did not match its HA domain",
            ));
        }
    }
    let rooms = ui.get_rooms();
    let living_room_icon = (0..rooms.row_count())
        .filter_map(|index| rooms.row_data(index))
        .find(|room| room.id.as_str() == "living_room")
        .is_some_and(|room| room.icon.as_str() == "sofa");
    if !living_room_icon {
        return Err(PreviewError::Interaction(
            "living-room shortcut did not receive its curated room icon",
        ));
    }

    // A control's main body inspects it. Its explicit trailing power target activates it.
    dispatch_click(ui, body_point);
    if !ui.get_details_open()
        || ui.get_details_data().entity_id.as_str() != "light.living_room_lamp"
    {
        return Err(PreviewError::Interaction(
            "control body did not open its details",
        ));
    }
    expect_no_command(received, "control body queued an action instead of details")?;
    ui.invoke_dismiss_details();
    dispatch_click(ui, action_point);
    expect_tap_for(received, "light.living_room_lamp")?;

    dispatch_pointer(
        ui,
        slint::platform::WindowEvent::PointerPressed {
            position: body_point,
            button: slint::platform::PointerEventButton::Left,
        },
    );
    advance_preview_clock(Duration::from_millis(800));
    if !ui.get_details_open() {
        return Err(PreviewError::Interaction("long press did not open Details"));
    }
    dispatch_pointer(
        ui,
        slint::platform::WindowEvent::PointerReleased {
            position: body_point,
            button: slint::platform::PointerEventButton::Left,
        },
    );
    expect_no_command(received, "long press queued a Tap")?;
    // Test the actual close target in full-screen mode; a sheet's top edge deliberately
    // changes with the entity's advertised controls and therefore has no fixed Y coordinate.
    ui.set_details_fullscreen(true);
    dispatch_click(ui, slint::LogicalPosition::new(746.0, 56.0));
    if ui.get_details_open() {
        return Err(PreviewError::Interaction(
            "Details close button did not dismiss",
        ));
    }
    ui.set_details_fullscreen(false);

    let moved_point = slint::LogicalPosition::new(150.0, 310.0);
    dispatch_pointer(
        ui,
        slint::platform::WindowEvent::PointerPressed {
            position: body_point,
            button: slint::platform::PointerEventButton::Left,
        },
    );
    dispatch_pointer(
        ui,
        slint::platform::WindowEvent::PointerMoved {
            position: moved_point,
        },
    );
    dispatch_pointer(
        ui,
        slint::platform::WindowEvent::PointerReleased {
            position: moved_point,
            button: slint::platform::PointerEventButton::Left,
        },
    );
    expect_no_command(received, "movement cancellation queued a Tap")?;
    Ok(())
}

fn qualify_tile_semantics(harness: &InteractionHarness) -> Result<(), PreviewError> {
    let ui = &harness.ui;
    let received = &harness.received;
    // Scene body and explicit play target both run the scene exactly once.
    let scene_body = slint::LogicalPosition::new(150.0, 404.0);
    let scene_action = slint::LogicalPosition::new(365.0, 404.0);
    dispatch_click(ui, scene_body);
    expect_tap_for(received, "scene.movie_time")?;
    if ui.get_details_open() {
        return Err(PreviewError::Interaction(
            "scene body opened details instead of running the scene",
        ));
    }
    dispatch_click(ui, scene_action);
    expect_tap_for(received, "scene.movie_time")?;

    // Measurement body and chevron both open details without an entity command.
    let measurement_body = slint::LogicalPosition::new(540.0, 404.0);
    let measurement_chevron = slint::LogicalPosition::new(742.0, 404.0);
    dispatch_click(ui, measurement_body);
    if !ui.get_details_open()
        || ui.get_details_data().entity_id.as_str() != "sensor.living_room_temperature"
    {
        return Err(PreviewError::Interaction(
            "measurement body did not open its details",
        ));
    }
    expect_no_command(received, "measurement body queued an action")?;
    ui.invoke_dismiss_details();
    dispatch_click(ui, measurement_chevron);
    if !ui.get_details_open()
        || ui.get_details_data().entity_id.as_str() != "sensor.living_room_temperature"
    {
        return Err(PreviewError::Interaction(
            "measurement chevron did not open its details",
        ));
    }
    expect_no_command(received, "measurement chevron queued an action")?;
    ui.invoke_dismiss_details();

    let dashboard = harness
        .state
        .borrow()
        .dashboard
        .clone()
        .ok_or(PreviewError::Interaction("fixture dashboard disappeared"))?;
    let hallway_index = dashboard
        .rooms
        .iter()
        .position(|room| room.id == RoomId::Area("hallway".into()))
        .and_then(|index| i32::try_from(index).ok())
        .ok_or(PreviewError::Interaction("fixture hallway disappeared"))?;
    ui.invoke_room_selected(hallway_index);
    let status = ui.get_tiles().row_data(0).ok_or(PreviewError::Interaction(
        "fixture binary status tile disappeared",
    ))?;
    if status.kind.as_str() != "status" || status.action_enabled {
        return Err(PreviewError::Interaction(
            "binary sensor was not a read-only status tile",
        ));
    }
    let status_body = slint::LogicalPosition::new(150.0, 270.0);
    let status_chevron = slint::LogicalPosition::new(365.0, 270.0);
    dispatch_click(ui, status_body);
    if !ui.get_details_open()
        || ui.get_details_data().entity_id.as_str() != "binary_sensor.hallway_motion"
    {
        return Err(PreviewError::Interaction(
            "status body did not open its details",
        ));
    }
    expect_no_command(received, "status body queued an action")?;
    ui.invoke_dismiss_details();
    dispatch_click(ui, status_chevron);
    if !ui.get_details_open()
        || ui.get_details_data().entity_id.as_str() != "binary_sensor.hallway_motion"
    {
        return Err(PreviewError::Interaction(
            "status chevron did not open its details",
        ));
    }
    expect_no_command(received, "status chevron queued an action")?;
    ui.invoke_dismiss_details();
    ui.invoke_room_selected(0);
    qualify_switch_details_power(harness)
}

fn qualify_switch_details_power(harness: &InteractionHarness) -> Result<(), PreviewError> {
    let ui = &harness.ui;
    let received = &harness.received;
    dispatch_click(ui, slint::LogicalPosition::new(540.0, 270.0));
    if !ui.get_details_open()
        || ui.get_details_data().entity_id.as_str() != "switch.reading_light"
        || !ui.get_details_data().can_toggle
        || !ui.get_details_data().controls_enabled
    {
        return Err(PreviewError::Interaction(
            "switch body did not open details with an enabled Power action",
        ));
    }
    expect_no_command(received, "switch body tap queued an action")?;

    // The details sheet grows from the bottom. Probe its safe central column from its top edge to
    // the screen bottom, reopening after outside taps, until the full-width Power button fires.
    for y in (640_u16..1272).step_by(8) {
        dispatch_click(ui, slint::LogicalPosition::new(400.0, f32::from(y)));
        match received.try_recv() {
            Ok(RuntimeCommand::Action {
                entity_id,
                action: DetailAction::Entity { key, argument },
            }) if entity_id == "switch.reading_light" && key == "turn_on" && argument.is_null() => {
                expect_no_command(received, "switch Power action emitted extra commands")?;
                ui.invoke_dismiss_details();
                return Ok(());
            }
            Ok(_) => {
                return Err(PreviewError::Interaction(
                    "switch details Power action emitted an unexpected command",
                ));
            }
            Err(TryRecvError::Disconnected) => {
                return Err(PreviewError::Interaction(
                    "switch details Power command queue disconnected",
                ));
            }
            Err(TryRecvError::Empty) => {
                if !ui.get_details_open() {
                    ui.invoke_request_details("switch.reading_light".into());
                }
            }
        }
    }
    Err(PreviewError::Interaction(
        "switch details Power action was not reachable",
    ))
}

/// More tiles than one screen holds are shown in pages: a swipe turns the page without tapping a
/// tile, a tap on the other page reaches the tile that is there, and the swipe back returns.
fn qualify_paged_grid(harness: &InteractionHarness) -> Result<(), PreviewError> {
    let InteractionHarness {
        ui,
        state,
        received,
        ..
    } = harness;
    let original = state.borrow().dashboard.clone();
    {
        let mut state = state.borrow_mut();
        let many = preview_snapshot(
            &preview_dashboard().map_err(PreviewError::Fixture)?,
            PreviewContent::ManyTiles,
        );
        state.dashboard = Some(many);
        render(ui, &state);
    }
    let swipe = |from: f32, to: f32| dispatch_swipe(ui, from, to, 640.0);
    // The first tile of the second page, as the preview of page 2 shows it.
    let first_of_second_page_action = slint::LogicalPosition::new(360.0, 293.0);

    swipe(600.0, 150.0);
    expect_no_command(received, "a swipe over the tiles queued a Tap")?;
    dispatch_click(ui, first_of_second_page_action);
    if !matches!(received.try_recv(), Ok(RuntimeCommand::Tap(id)) if id == "light.extra_11") {
        return Err(PreviewError::Interaction(
            "a tap on the second page did not reach its first tile",
        ));
    }
    expect_no_command(
        received,
        "the tap on the second page emitted more than one command",
    )?;

    swipe(150.0, 600.0);
    expect_no_command(received, "the swipe back queued a Tap")?;
    dispatch_click(ui, first_of_second_page_action);
    if matches!(received.try_recv(), Ok(RuntimeCommand::Tap(id)) if id == "light.extra_11") {
        return Err(PreviewError::Interaction(
            "the swipe back did not return to the first page",
        ));
    }
    // A short swipe does not turn the page.
    swipe(500.0, 440.0);
    expect_no_command(received, "a short swipe queued a Tap")?;

    let mut state = state.borrow_mut();
    state.dashboard = original;
    render(ui, &state);
    Ok(())
}

fn qualify_full_command_queue(harness: &InteractionHarness) -> Result<(), PreviewError> {
    let InteractionHarness {
        ui,
        commands,
        received,
        ..
    } = harness;
    state_set_language(harness, "de");
    let tap_point = slint::LogicalPosition::new(365.0, 270.0);
    commands
        .try_send(RuntimeCommand::Stop)
        .map_err(|_| PreviewError::Interaction("could not fill command queue"))?;
    dispatch_click(ui, tap_point);
    if ui.get_connection_text().as_str() != "Panel ausgelastet · Erneut tippen" {
        return Err(PreviewError::Interaction(
            "full command queue did not show busy status",
        ));
    }
    if !matches!(received.try_recv(), Ok(RuntimeCommand::Stop)) {
        return Err(PreviewError::Interaction(
            "busy Tap replaced the queued command",
        ));
    }
    expect_no_command(received, "full-queue Tap was replayed")?;
    state_set_language(harness, "en");
    Ok(())
}

fn qualify_settings_acknowledgement(harness: &InteractionHarness) -> Result<(), PreviewError> {
    let InteractionHarness {
        ui,
        state,
        commands,
        received,
        size,
    } = harness;
    commands
        .try_send(RuntimeCommand::Stop)
        .map_err(|_| PreviewError::Interaction("could not refill command queue"))?;
    let settings = Settings {
        orientation: "landscape".into(),
        theme: "dark".into(),
        brightness: 40,
        home_view: "favorites".into(),
        favorites: vec!["light.living_room_lamp".into()],
        ..Settings::default()
    };
    apply_runtime_event(
        ui,
        state,
        *size,
        RuntimeEvent::Settings(Box::new(AppliedSettings {
            revision: 42,
            settings,
        })),
    );
    if !ui.get_dark_mode()
        || ui.get_home_view().as_str() != "favorites"
        || ui.get_favorites().row_count() != 1
        || (ui.get_display_dim_opacity() - 0.6).abs() >= 0.01
        || state.borrow().pending_settings_acks.len() != 1
    {
        return Err(PreviewError::Interaction(
            "settings were not applied before acknowledgement",
        ));
    }
    retry_settings_acknowledgements(commands, state);
    if state.borrow().pending_settings_acks.len() != 1 {
        return Err(PreviewError::Interaction(
            "full queue dropped SettingsApplied",
        ));
    }
    if !matches!(received.try_recv(), Ok(RuntimeCommand::Stop)) {
        return Err(PreviewError::Interaction(
            "queued command disappeared before retry",
        ));
    }
    retry_settings_acknowledgements(commands, state);
    if !matches!(received.try_recv(), Ok(RuntimeCommand::SettingsApplied(42)))
        || !state.borrow().pending_settings_acks.is_empty()
    {
        return Err(PreviewError::Interaction(
            "SettingsApplied was not retried after queue space",
        ));
    }
    Ok(())
}

/// The panel behind the Home Assistant icon closes with its close button and with a tap
/// anywhere outside it, and an outside tap is consumed instead of reaching a tile.
fn qualify_status_panel(harness: &InteractionHarness) -> Result<(), PreviewError> {
    reset_interaction_harness(
        harness,
        preview_dashboard().map_err(PreviewError::Fixture)?,
        Settings::default(),
    );
    let ui = &harness.ui;
    ui.window().set_size(PhysicalSize::new(800, 1280));
    ui.set_allow_forget(true);
    while harness.received.try_recv().is_ok() {}

    ui.set_inspect_status(true);
    dispatch_click(ui, slint::LogicalPosition::new(400.0, 250.0));
    if !ui.get_inspect_status() {
        return Err(PreviewError::Interaction(
            "a tap inside the status panel closed it",
        ));
    }
    // 150/310 lies on a tile: the tap must only close the panel.
    dispatch_click(ui, slint::LogicalPosition::new(150.0, 310.0));
    if ui.get_inspect_status() {
        return Err(PreviewError::Interaction(
            "a tap outside the status panel did not close it",
        ));
    }
    if ui.get_details_open() {
        return Err(PreviewError::Interaction(
            "closing the status panel also opened a tile's details",
        ));
    }
    expect_no_command(&harness.received, "the closing tap reached a tile")?;

    ui.set_inspect_status(true);
    dispatch_click(ui, slint::LogicalPosition::new(748.0, 182.0));
    if ui.get_inspect_status() {
        return Err(PreviewError::Interaction(
            "the status panel's close button did not close it",
        ));
    }
    expect_no_command(&harness.received, "the close button reached a tile")
}

fn qualify_daylight_palette_resolution(harness: &InteractionHarness) -> Result<(), PreviewError> {
    reset_interaction_harness(
        harness,
        preview_dashboard().map_err(PreviewError::Fixture)?,
        Settings::default(),
    );
    {
        let mut state = harness.state.borrow_mut();
        state.settings.settings.theme = "automatic".into();
        state.settings.settings.light_color_scheme = "porcelain".into();
        state.settings.settings.dark_color_scheme = "warm".into();
        state.settings.settings.increased_contrast = true;
        state.settings.settings.reduced_motion = true;
        state.daylight = true;
        render(&harness.ui, &state);
    }
    assert_resolved_palette(&harness.ui, false, "porcelain")?;

    apply_runtime_event(
        &harness.ui,
        &harness.state,
        harness.size,
        RuntimeEvent::Daylight(false),
    );
    assert_resolved_palette(&harness.ui, true, "warm")?;
    apply_runtime_event(
        &harness.ui,
        &harness.state,
        harness.size,
        RuntimeEvent::Daylight(true),
    );
    assert_resolved_palette(&harness.ui, false, "porcelain")
}

fn assert_resolved_palette(ui: &MainWindow, dark: bool, scheme: &str) -> Result<(), PreviewError> {
    if ui.get_dark_mode() != dark
        || ui.get_color_scheme().as_str() != scheme
        || !ui.get_increased_contrast()
        || !ui.get_reduced_motion()
    {
        return Err(PreviewError::Interaction(
            "automatic daylight did not resolve its independent palette while preserving accessibility settings",
        ));
    }
    Ok(())
}

/// At 200% scale a long light name pushes its brightness control below the viewport. The test
/// opens the detail sheet with a body tap, scrolls the sheet, then changes brightness by dragging
/// the now-visible slider through the normal runtime callback path.
#[allow(clippy::too_many_lines)] // The complete 200% gesture flow must share one harness state.
fn qualify_large_detail_scroll(harness: &InteractionHarness) -> Result<(), PreviewError> {
    let mut dashboard = preview_dashboard().map_err(PreviewError::Fixture)?;
    let long_name =
        "Living room lighting for the reading chair beside the terrace door and bookcase";
    dashboard_entity_mut(&mut dashboard, "light.living_room_lamp")
        .ok_or(PreviewError::Interaction("fixture light missing"))?
        .name = long_name.into();
    let settings = Settings {
        ui_scale: 200,
        ..Settings::default()
    };
    reset_interaction_harness(harness, dashboard, settings);
    let ui = &harness.ui;
    ui.window().set_size(PhysicalSize::new(800, 1280));
    apply_settings(ui, &harness.state.borrow(), harness.size);
    render(ui, &harness.state.borrow());
    while harness.received.try_recv().is_ok() {}

    // The physical display remains 800x1280 while the 200% interface exposes a 400x640 logical
    // viewport. Probe the first card's body before its trailing action target.
    let mut opened_target = false;
    for y in (160_u16..610).step_by(12) {
        for x in (24_u16..320).step_by(16) {
            dispatch_click(ui, slint::LogicalPosition::new(f32::from(x), f32::from(y)));
            if ui.get_details_open() {
                if ui.get_details_data().entity_id.as_str() == "light.living_room_lamp" {
                    opened_target = true;
                    break;
                }
                ui.invoke_dismiss_details();
            }
            expect_no_command(
                &harness.received,
                "a 200% control body tap activated an entity",
            )?;
        }
        if opened_target {
            break;
        }
    }
    if !opened_target {
        return Err(PreviewError::Interaction(
            "200% control body did not open the target details",
        ));
    }
    if ui.get_details_data().name.as_str() != long_name {
        return Err(PreviewError::Interaction(
            "200% detail sheet lost the configured long entity name",
        ));
    }
    expect_no_command(&harness.received, "opening 200% details enqueued a command")?;

    dispatch_vertical_swipe(ui, 200.0, 580.0, 200.0, 260.0);
    if !ui.get_details_open() {
        return Err(PreviewError::Interaction(
            "scrolling the 200% detail sheet dismissed it",
        ));
    }
    if !matches!(ui.get_details_data().control.as_str(), "slider") {
        return Err(PreviewError::Interaction(
            "200% detail sheet did not expose the brightness slider model",
        ));
    }

    // Drag a bounded scan across the lower half of the scrolled sheet. Only the slider produces
    // an Action here; the test requires exactly one SetBrightness value for the light.
    for y in (300_u16..600).rev().step_by(20) {
        dispatch_horizontal_drag(ui, 80.0, 280.0, f32::from(y));
        match harness.received.try_recv() {
            Ok(RuntimeCommand::Action {
                entity_id,
                action: DetailAction::Entity { key, argument },
            }) if entity_id == "light.living_room_lamp"
                && key == "brightness"
                && argument
                    .as_f64()
                    .is_some_and(|percent| (1.0..=100.0).contains(&percent)) =>
            {
                expect_no_command(
                    &harness.received,
                    "200% brightness slider emitted more than one action",
                )?;
                restore_interaction_harness(harness)?;
                return Ok(());
            }
            Ok(RuntimeCommand::Action {
                entity_id,
                action: DetailAction::Entity { key, .. },
            }) if entity_id == "light.living_room_lamp"
                && matches!(key.as_str(), "turn_on" | "turn_off") =>
            {
                // The generic Power button shares the details sheet. Skip its row while searching
                // for the scrolled slider; it is a separate, valid explicit control action.
            }
            Ok(_) => {
                return Err(PreviewError::Interaction(
                    "200% detail slider emitted an unexpected action",
                ));
            }
            Err(TryRecvError::Disconnected) => {
                return Err(PreviewError::Interaction(
                    "200% detail action queue disconnected",
                ));
            }
            Err(TryRecvError::Empty) => {}
        }
        if !ui.get_details_open() {
            return Err(PreviewError::Interaction(
                "200% detail sheet closed before its brightness slider was reached",
            ));
        }
    }
    Err(PreviewError::Interaction(
        "scrolling did not make the 200% brightness slider reachable",
    ))
}

fn restore_interaction_harness(harness: &InteractionHarness) -> Result<(), PreviewError> {
    reset_interaction_harness(
        harness,
        preview_dashboard().map_err(PreviewError::Fixture)?,
        Settings::default(),
    );
    harness.ui.window().set_size(harness.size);
    apply_settings(&harness.ui, &harness.state.borrow(), harness.size);
    render(&harness.ui, &harness.state.borrow());
    Ok(())
}

fn reset_interaction_harness(
    harness: &InteractionHarness,
    dashboard: Dashboard,
    mut settings: Settings,
) {
    settings.screensaver = "disabled".into();
    let mut state = harness.state.borrow_mut();
    state.dashboard = Some(dashboard);
    state.settings = AppliedSettings {
        revision: 0,
        settings,
    };
    state.daylight = true;
    state.selected_room = Some(RoomId::Area("living_room".into()));
    state.selected_entity = None;
    state.rooms_page = false;
    state.runtime_status.clear();
    state.pending_settings_acks.clear();
    state.started_at = Instant::now();
    state.display_controller = DisplayController::new(0);
    state.update_display();
    render(&harness.ui, &state);
}

fn state_set_language(harness: &InteractionHarness, language: &str) {
    harness.state.borrow_mut().settings.settings.language = language.into();
    render(&harness.ui, &harness.state.borrow());
}

#[derive(Clone, Copy)]
enum FlagExpectation {
    Yes,
    No,
}

impl FlagExpectation {
    const fn matches(self, actual: bool) -> bool {
        match self {
            Self::Yes => actual,
            Self::No => !actual,
        }
    }
}

#[derive(Clone, Copy)]
struct TileExpectation {
    value: &'static str,
    stale: FlagExpectation,
    unavailable: FlagExpectation,
    failed: FlagExpectation,
    pending: FlagExpectation,
}

impl TileExpectation {
    const fn exact(value: &'static str) -> Self {
        Self {
            value,
            stale: FlagExpectation::No,
            unavailable: FlagExpectation::No,
            failed: FlagExpectation::No,
            pending: FlagExpectation::No,
        }
    }

    const fn stale(mut self) -> Self {
        self.stale = FlagExpectation::Yes;
        self
    }

    const fn unavailable(mut self) -> Self {
        self.unavailable = FlagExpectation::Yes;
        self
    }

    const fn failed(mut self) -> Self {
        self.failed = FlagExpectation::Yes;
        self
    }

    const fn pending(mut self) -> Self {
        self.pending = FlagExpectation::Yes;
        self
    }
}

fn expect_tile_view(
    ui: &MainWindow,
    entity_id: &str,
    expected: TileExpectation,
) -> Result<(), PreviewError> {
    let tiles = ui.get_tiles();
    if tiles.row_count() == 0 {
        return Err(PreviewError::Interaction(
            "selected room rendered no tiles during state qualification",
        ));
    }
    if ui.get_selected_room() != 0 {
        return Err(PreviewError::Interaction(
            "state qualification did not display the first room",
        ));
    }
    let tile = (0..tiles.row_count()).find_map(|index| {
        tiles
            .row_data(index)
            .filter(|tile| tile.entity_id.as_str() == entity_id)
    });
    let Some(tile) = tile else {
        let message = match entity_id {
            "sensor.living_room_temperature" => "living room sensor was not rendered",
            "scene.movie_time" => "movie scene was not rendered",
            "light.living_room_lamp" => "living room light was not rendered",
            _ => "expected entity tile was not rendered",
        };
        return Err(PreviewError::Interaction(message));
    };
    if tile.value.as_str() != expected.value
        || !expected.stale.matches(tile.stale)
        || !expected.unavailable.matches(tile.unavailable)
        || !expected.failed.matches(tile.failed)
        || !expected.pending.matches(tile.pending)
    {
        return Err(PreviewError::Interaction(
            "rendered tile state did not match its HA feedback",
        ));
    }
    Ok(())
}

fn qualify_stale_and_unavailable_states(harness: &InteractionHarness) -> Result<(), PreviewError> {
    let mut dashboard = preview_dashboard().map_err(PreviewError::Fixture)?;
    dashboard.connection = ConnectionStatus::Offline;
    dashboard.is_stale = true;
    for tile in dashboard
        .rooms
        .iter_mut()
        .flat_map(|room| &mut room.entities)
    {
        tile.is_stale = true;
        tile.command_enabled = false;
    }
    dashboard.rooms.push(Room {
        id: RoomId::Area("empty_room".into()),
        name: "Empty room".into(),
        entities: Vec::new(),
    });
    reset_interaction_harness(
        harness,
        dashboard,
        Settings {
            language: "de".into(),
            ..Settings::default()
        },
    );
    if harness.ui.get_connection_text().as_str()
        != "Home Assistant offline · Verbindung wird wiederhergestellt…"
    {
        return Err(PreviewError::Interaction(
            "offline status was not localized",
        ));
    }
    let rooms = harness.ui.get_rooms();
    let empty_summary = (0..rooms.row_count())
        .filter_map(|index| rooms.row_data(index))
        .find(|room| room.name.as_str() == "Empty room")
        .is_some_and(|room| room.summary.as_str() == "Noch keine Steuerelemente");
    if !empty_summary {
        return Err(PreviewError::Interaction(
            "empty room summary was not localized",
        ));
    }
    let living_summary = (0..rooms.row_count())
        .filter_map(|index| rooms.row_data(index))
        .find(|room| room.id.as_str() == "living_room")
        .is_some_and(|room| room.summary.as_str() == "4 Steuerelemente");
    if !living_summary {
        return Err(PreviewError::Interaction(
            "room control count was not localized",
        ));
    }
    qualify_stale_sensor_details(harness)?;
    qualify_unavailable_sensor_states(harness)?;
    qualify_stale_switch_details(harness)
}

fn qualify_stale_sensor_details(harness: &InteractionHarness) -> Result<(), PreviewError> {
    expect_tile_view(
        &harness.ui,
        "sensor.living_room_temperature",
        TileExpectation::exact("Letzter Stand · 21,4 °C").stale(),
    )?;
    harness
        .ui
        .invoke_activate_entity("sensor.living_room_temperature".into());
    let details = harness.ui.get_details_data();
    if !harness.ui.get_details_open()
        || details.value.as_str() != "Letzter Stand · 21,4 °C"
        || !details.attributes.is_empty()
    {
        return Err(PreviewError::Interaction(
            "stale read-only entity did not open usable details",
        ));
    }
    harness.ui.invoke_dismiss_details();
    if harness.ui.get_details_open() {
        return Err(PreviewError::Interaction(
            "Details dismissal callback did not close",
        ));
    }
    Ok(())
}

fn qualify_unavailable_sensor_states(harness: &InteractionHarness) -> Result<(), PreviewError> {
    let mut state = harness.state.borrow_mut();
    let sensor = state
        .dashboard
        .as_mut()
        .and_then(|dashboard| dashboard_entity_mut(dashboard, "sensor.living_room_temperature"));
    let Some(sensor) = sensor else {
        return Err(PreviewError::Interaction("fixture sensor disappeared"));
    };
    sensor.state = None;
    render(&harness.ui, &state);
    drop(state);
    expect_tile_view(
        &harness.ui,
        "sensor.living_room_temperature",
        TileExpectation::exact("Nicht verfügbar")
            .stale()
            .unavailable(),
    )?;
    harness
        .ui
        .invoke_activate_entity("sensor.living_room_temperature".into());
    if !harness.ui.get_details_data().attributes.as_str().is_empty() {
        return Err(PreviewError::Interaction(
            "entity without a state exposed stale attributes",
        ));
    }
    let mut state = harness.state.borrow_mut();
    let sensor = state
        .dashboard
        .as_mut()
        .and_then(|dashboard| dashboard_entity_mut(dashboard, "sensor.living_room_temperature"))
        .ok_or(PreviewError::Interaction("fixture sensor disappeared"))?;
    sensor.state = Some(preview_state(
        "sensor.living_room_temperature",
        "unknown",
        "Living room temperature",
        "mdi:thermometer",
        None,
        Some("°C"),
    ));
    render(&harness.ui, &state);
    drop(state);
    expect_tile_view(
        &harness.ui,
        "sensor.living_room_temperature",
        TileExpectation::exact("Unbekannt").stale().unavailable(),
    )?;
    let mut state = harness.state.borrow_mut();
    let sensor = state
        .dashboard
        .as_mut()
        .and_then(|dashboard| dashboard_entity_mut(dashboard, "sensor.living_room_temperature"))
        .ok_or(PreviewError::Interaction("fixture sensor disappeared"))?;
    sensor.state = Some(preview_state(
        "sensor.living_room_temperature",
        "unavailable",
        "Living room temperature",
        "mdi:thermometer",
        None,
        Some("°C"),
    ));
    state.settings.settings.language = "en".into();
    render(&harness.ui, &state);
    drop(state);
    expect_tile_view(
        &harness.ui,
        "sensor.living_room_temperature",
        TileExpectation::exact("Unavailable").stale().unavailable(),
    )
}

fn qualify_stale_switch_details(harness: &InteractionHarness) -> Result<(), PreviewError> {
    harness
        .ui
        .invoke_activate_entity("switch.reading_light".into());
    if !harness.ui.get_details_open()
        || harness.ui.get_details_data().entity_id.as_str() != "switch.reading_light"
    {
        return Err(PreviewError::Interaction(
            "offline non-read-only control did not open Details",
        ));
    }
    expect_no_command(&harness.received, "offline switch tap queued an action")
}

fn qualify_action_feedback(harness: &InteractionHarness) -> Result<(), PreviewError> {
    let mut dashboard = preview_dashboard().map_err(PreviewError::Fixture)?;
    for tile in dashboard
        .rooms
        .iter_mut()
        .flat_map(|room| &mut room.entities)
    {
        tile.icon = None;
    }
    reset_interaction_harness(
        harness,
        dashboard,
        Settings {
            language: "de".into(),
            ..Settings::default()
        },
    );
    qualify_connection_and_room_labels(harness)?;
    qualify_scene_feedback(harness)?;
    qualify_light_feedback(harness)?;
    qualify_icon_fallbacks(harness)
}

fn qualify_connection_and_room_labels(harness: &InteractionHarness) -> Result<(), PreviewError> {
    let mut dashboard = harness
        .state
        .borrow()
        .dashboard
        .clone()
        .ok_or(PreviewError::Interaction("fixture dashboard disappeared"))?;
    dashboard.rooms.push(Room {
        id: RoomId::Area("empty_room".into()),
        name: "Empty room".into(),
        entities: Vec::new(),
    });
    dashboard.connection = ConnectionStatus::Ready;
    {
        let mut state = harness.state.borrow_mut();
        state.dashboard = Some(dashboard.clone());
        state.runtime_status = "Dashboard connected; Njörun management is unavailable".into();
    }
    apply_runtime_event(
        &harness.ui,
        &harness.state,
        harness.size,
        RuntimeEvent::Dashboard(dashboard.clone()),
    );
    if harness.ui.get_connection_text().as_str()
        != "Dashboard verbunden · Njörun-Verwaltung nicht verfügbar"
    {
        return Err(PreviewError::Interaction(
            "management-unavailable status was not localized",
        ));
    }
    dashboard.connection = ConnectionStatus::Synchronizing;
    harness.state.borrow_mut().runtime_status = "Connecting to Home Assistant".into();
    apply_runtime_event(
        &harness.ui,
        &harness.state,
        harness.size,
        RuntimeEvent::Dashboard(dashboard),
    );
    if harness.ui.get_connection_text().as_str() != "Synchronisiere Home Assistant…" {
        return Err(PreviewError::Interaction("sync status was not localized"));
    }
    state_set_language(harness, "en");
    let rooms = harness.ui.get_rooms();
    let summary = (0..rooms.row_count())
        .filter_map(|index| rooms.row_data(index))
        .find(|room| room.name.as_str() == "Empty room")
        .is_some_and(|room| room.summary.as_str() == "No controls yet");
    if !summary {
        return Err(PreviewError::Interaction(
            "empty room English summary was missing",
        ));
    }
    Ok(())
}

fn qualify_scene_feedback(harness: &InteractionHarness) -> Result<(), PreviewError> {
    set_feedback(
        harness,
        "scene.movie_time",
        Some(ActionFeedback::SceneRequested),
    )?;
    expect_tile_view(
        &harness.ui,
        "scene.movie_time",
        TileExpectation::exact("Requested"),
    )?;
    state_set_language(harness, "de");
    expect_tile_view(
        &harness.ui,
        "scene.movie_time",
        TileExpectation::exact("Angefordert"),
    )?;
    set_feedback(
        harness,
        "scene.movie_time",
        Some(ActionFeedback::AwaitingAcknowledgement {
            action: ActionKind::RunScene,
        }),
    )?;
    expect_tile_view(
        &harness.ui,
        "scene.movie_time",
        TileExpectation::exact("Szene wird ausgeführt …").pending(),
    )
}

fn qualify_light_feedback(harness: &InteractionHarness) -> Result<(), PreviewError> {
    qualify_rejected_action_details(harness)?;
    qualify_pending_action_feedback(harness)
}

fn qualify_rejected_action_details(harness: &InteractionHarness) -> Result<(), PreviewError> {
    set_feedback(
        harness,
        "light.living_room_lamp",
        Some(ActionFeedback::Rejected {
            reason: "HA rejected the test request".into(),
        }),
    )?;
    expect_tile_view(
        &harness.ui,
        "light.living_room_lamp",
        TileExpectation::exact("Aktion fehlgeschlagen").failed(),
    )?;
    harness
        .ui
        .invoke_activate_entity("light.living_room_lamp".into());
    let details = harness.ui.get_details_data();
    if details.reason.as_str() != "HA rejected the test request" || !details.attributes.is_empty() {
        return Err(PreviewError::Interaction(
            "rejected action details omitted the reason or repeated brightness metadata",
        ));
    }
    state_set_language(harness, "en");
    expect_tile_view(
        &harness.ui,
        "light.living_room_lamp",
        TileExpectation::exact("Action failed").failed(),
    )?;
    Ok(())
}

fn qualify_pending_action_feedback(harness: &InteractionHarness) -> Result<(), PreviewError> {
    state_set_language(harness, "de");
    set_feedback(
        harness,
        "light.living_room_lamp",
        Some(ActionFeedback::Unconfirmed {
            action: ActionKind::Toggle(TargetState::Off),
        }),
    )?;
    expect_tile_view(
        &harness.ui,
        "light.living_room_lamp",
        TileExpectation::exact("Nicht bestätigt").failed(),
    )?;
    state_set_language(harness, "en");
    set_feedback(
        harness,
        "light.living_room_lamp",
        Some(ActionFeedback::AcceptedAwaitingState {
            target: TargetState::Off,
        }),
    )?;
    expect_tile_view(
        &harness.ui,
        "light.living_room_lamp",
        TileExpectation::exact("Turning off…").pending(),
    )?;
    state_set_language(harness, "de");
    set_feedback(
        harness,
        "light.living_room_lamp",
        Some(ActionFeedback::AcceptedAwaitingState {
            target: TargetState::Off,
        }),
    )?;
    expect_tile_view(
        &harness.ui,
        "light.living_room_lamp",
        TileExpectation::exact("Wird ausgeschaltet…").pending(),
    )?;
    set_feedback(
        harness,
        "light.living_room_lamp",
        Some(ActionFeedback::AwaitingAcknowledgement {
            action: ActionKind::Toggle(TargetState::On),
        }),
    )?;
    expect_tile_view(
        &harness.ui,
        "light.living_room_lamp",
        TileExpectation::exact("Wird eingeschaltet…").pending(),
    )
}

fn qualify_icon_fallbacks(harness: &InteractionHarness) -> Result<(), PreviewError> {
    harness.ui.invoke_room_selected(0);
    let tiles = harness.ui.get_tiles();
    let expected = [
        ("light.living_room_lamp", EntityIcon::Lightbulb),
        ("switch.reading_light", EntityIcon::Power),
        ("scene.movie_time", EntityIcon::Play),
        ("sensor.living_room_temperature", EntityIcon::Thermometer),
    ];
    for (entity_id, icon) in expected {
        let rendered = (0..tiles.row_count())
            .filter_map(|index| tiles.row_data(index))
            .find(|tile| tile.entity_id.as_str() == entity_id)
            .is_some_and(|tile| tile.icon == icon);
        if !rendered {
            return Err(PreviewError::Interaction(
                "domain icon fallback was incorrect",
            ));
        }
    }
    let binary_index = harness
        .state
        .borrow()
        .dashboard
        .as_ref()
        .and_then(|dashboard| {
            dashboard
                .rooms
                .iter()
                .position(|room| room.id == RoomId::Area("hallway".into()))
        })
        .and_then(|index| i32::try_from(index).ok())
        .ok_or(PreviewError::Interaction("fixture hallway was missing"))?;
    {
        let mut state = harness.state.borrow_mut();
        let dashboard = state
            .dashboard
            .as_mut()
            .ok_or(PreviewError::Interaction("fixture dashboard disappeared"))?;
        let binary = dashboard_entity_mut(dashboard, "binary_sensor.hallway_motion").ok_or(
            PreviewError::Interaction("fixture binary sensor disappeared"),
        )?;
        binary.icon = Some("mdi:checkbox-blank-circle-outline".into());
        render(&harness.ui, &state);
    }
    harness.ui.invoke_room_selected(binary_index);
    if harness.ui.get_tiles().row_data(0).is_none_or(|tile| {
        tile.entity_id.as_str() != "binary_sensor.hallway_motion"
            || tile.icon != EntityIcon::BinarySensor
    }) {
        return Err(PreviewError::Interaction(
            "binary sensor icon was not rendered",
        ));
    }
    {
        let mut state = harness.state.borrow_mut();
        let dashboard = state
            .dashboard
            .as_mut()
            .ok_or(PreviewError::Interaction("fixture dashboard disappeared"))?;
        let binary = dashboard_entity_mut(dashboard, "binary_sensor.hallway_motion").ok_or(
            PreviewError::Interaction("fixture binary sensor disappeared"),
        )?;
        binary.icon = None;
        render(&harness.ui, &state);
    }
    if harness.ui.get_tiles().row_data(0).is_none_or(|tile| {
        tile.entity_id.as_str() != "binary_sensor.hallway_motion"
            || tile.icon != EntityIcon::BinarySensor
    }) {
        return Err(PreviewError::Interaction(
            "binary sensor fallback icon was incorrect",
        ));
    }
    Ok(())
}

fn set_feedback(
    harness: &InteractionHarness,
    entity_id: &str,
    feedback: Option<ActionFeedback>,
) -> Result<(), PreviewError> {
    let mut state = harness.state.borrow_mut();
    let tile = state
        .dashboard
        .as_mut()
        .and_then(|dashboard| dashboard_entity_mut(dashboard, entity_id))
        .ok_or(PreviewError::Interaction("feedback entity disappeared"))?;
    tile.action_feedback = feedback;
    if entity_id == "light.living_room_lamp" {
        tile.command_enabled = false;
    }
    render(&harness.ui, &state);
    Ok(())
}

fn dashboard_entity_mut<'a>(
    dashboard: &'a mut Dashboard,
    entity_id: &str,
) -> Option<&'a mut njorun_app_core::EntityTile> {
    dashboard
        .rooms
        .iter_mut()
        .flat_map(|room| &mut room.entities)
        .find(|tile| tile.entity_id == entity_id)
}

fn qualify_wake_and_navigation_callbacks(harness: &InteractionHarness) -> Result<(), PreviewError> {
    reset_interaction_harness(
        harness,
        preview_dashboard().map_err(PreviewError::Fixture)?,
        Settings::default(),
    );
    qualify_wake_only_entity_callbacks(harness)?;
    qualify_wake_only_navigation_callbacks(harness)?;
    qualify_interactive_navigation_callbacks(harness)
}

fn qualify_wake_only_entity_callbacks(harness: &InteractionHarness) -> Result<(), PreviewError> {
    turn_display_off(harness);
    harness
        .ui
        .invoke_activate_entity("sensor.living_room_temperature".into());
    if harness.ui.get_display_off() || harness.ui.get_details_open() {
        return Err(PreviewError::Interaction(
            "first read-only activation did not consume the wake touch",
        ));
    }
    expect_no_command(&harness.received, "wake touch queued an entity command")?;
    harness
        .ui
        .invoke_activate_entity("sensor.living_room_temperature".into());
    if !harness.ui.get_details_open() {
        return Err(PreviewError::Interaction(
            "second read-only activation did not open Details",
        ));
    }
    turn_display_off(harness);
    harness.ui.invoke_dismiss_details();
    if !harness.ui.get_details_open() || harness.ui.get_display_off() {
        return Err(PreviewError::Interaction(
            "dismiss on the off screen was not consumed as a wake touch",
        ));
    }
    harness.ui.invoke_dismiss_details();
    if harness.ui.get_details_open() {
        return Err(PreviewError::Interaction(
            "Details did not dismiss after wake",
        ));
    }
    turn_display_off(harness);
    harness
        .ui
        .invoke_request_details("sensor.living_room_temperature".into());
    if harness.ui.get_details_open() || harness.ui.get_display_off() {
        return Err(PreviewError::Interaction(
            "first explicit Details request did not only wake the display",
        ));
    }
    harness
        .ui
        .invoke_request_details("sensor.living_room_temperature".into());
    if !harness.ui.get_details_open() {
        return Err(PreviewError::Interaction(
            "explicit Details request did not open after wake",
        ));
    }
    Ok(())
}

fn qualify_wake_only_navigation_callbacks(
    harness: &InteractionHarness,
) -> Result<(), PreviewError> {
    turn_display_off(harness);
    harness.ui.invoke_room_selected(1);
    if harness.state.borrow().selected_room != Some(RoomId::Area("living_room".into()))
        || harness.ui.get_display_off()
    {
        return Err(PreviewError::Interaction(
            "room selection wake touch changed the selected room",
        ));
    }
    turn_display_off(harness);
    harness.ui.invoke_show_rooms();
    if harness.ui.get_rooms_page() {
        return Err(PreviewError::Interaction(
            "first Rooms touch changed the page while waking",
        ));
    }
    turn_display_off(harness);
    harness.ui.invoke_go_home();
    if harness.ui.get_rooms_page() {
        return Err(PreviewError::Interaction(
            "first Favorites touch changed the page while waking",
        ));
    }
    harness.ui.invoke_wake_display();
    if harness.ui.get_display_off() {
        return Err(PreviewError::Interaction(
            "wake callback did not wake the display",
        ));
    }
    Ok(())
}

fn qualify_interactive_navigation_callbacks(
    harness: &InteractionHarness,
) -> Result<(), PreviewError> {
    state_set_home_view(harness, "favorites");
    harness.ui.invoke_show_rooms();
    if !harness.ui.get_rooms_page() || harness.state.borrow().selected_room.is_some() {
        return Err(PreviewError::Interaction("Rooms navigation did not open"));
    }
    harness.ui.invoke_room_selected(0);
    if !harness.ui.get_rooms_page() || harness.ui.get_selected_room() != 0 {
        return Err(PreviewError::Interaction(
            "room shortcut did not select a room",
        ));
    }
    harness.ui.invoke_room_selected(-1);
    if harness.ui.get_selected_room() != -1 || !harness.ui.get_rooms_page() {
        return Err(PreviewError::Interaction(
            "back did not restore the originating room list",
        ));
    }
    harness.ui.invoke_room_selected(0);
    harness
        .ui
        .invoke_request_details("light.living_room_lamp".into());
    if !harness.ui.get_details_open() {
        return Err(PreviewError::Interaction("fixture details did not open"));
    }
    harness.ui.invoke_dismiss_details();
    if harness.ui.get_selected_room() != 0 || !harness.ui.get_rooms_page() {
        return Err(PreviewError::Interaction("closing details lost the room"));
    }
    harness
        .ui
        .invoke_request_details("light.living_room_lamp".into());
    harness.ui.invoke_go_home();
    if harness.ui.get_details_open()
        || harness.ui.get_rooms_page()
        || harness.state.borrow().selected_room.is_some()
    {
        return Err(PreviewError::Interaction(
            "Home did not return directly from details to Favorites",
        ));
    }
    for (size, button_center) in [
        (
            PhysicalSize::new(1280, 800),
            slint::LogicalPosition::new(120.0, 129.0),
        ),
        (
            PhysicalSize::new(800, 1280),
            slint::LogicalPosition::new(85.0, 105.0),
        ),
    ] {
        harness.ui.window().set_size(size);
        harness.ui.invoke_show_rooms();
        harness.ui.invoke_room_selected(0);
        dispatch_click(&harness.ui, button_center);
        if harness.ui.get_selected_room() != -1 || harness.ui.get_rooms_page() {
            return Err(PreviewError::Interaction(
                "pinned Home target did not return to the starting dashboard",
            ));
        }
    }
    Ok(())
}

fn state_set_home_view(harness: &InteractionHarness, home_view: &str) {
    harness.state.borrow_mut().settings.settings.home_view = home_view.into();
    render(&harness.ui, &harness.state.borrow());
}

fn turn_display_off(harness: &InteractionHarness) {
    let mut state = harness.state.borrow_mut();
    state.display_controller.turn_off();
    state.update_display();
    render(&harness.ui, &state);
}

fn qualify_dashboard_reconciliation(harness: &InteractionHarness) -> Result<(), PreviewError> {
    let mut dashboard = preview_dashboard().map_err(PreviewError::Fixture)?;
    reset_interaction_harness(harness, dashboard.clone(), Settings::default());
    {
        let mut state = harness.state.borrow_mut();
        state.selected_entity = Some("sensor.living_room_temperature".into());
        render(&harness.ui, &state);
    }
    let living_room = dashboard
        .rooms
        .iter_mut()
        .find(|room| room.id == RoomId::Area("living_room".into()))
        .ok_or(PreviewError::Interaction("fixture living room disappeared"))?;
    living_room
        .entities
        .retain(|tile| tile.entity_id != "sensor.living_room_temperature");
    apply_runtime_event(
        &harness.ui,
        &harness.state,
        harness.size,
        RuntimeEvent::Dashboard(dashboard.clone()),
    );
    if harness.state.borrow().selected_entity.is_some()
        || harness.state.borrow().selected_room != Some(RoomId::Area("living_room".into()))
    {
        return Err(PreviewError::Interaction(
            "removed entity left stale Details selection",
        ));
    }
    harness.state.borrow_mut().selected_entity = Some("switch.reading_light".into());
    dashboard
        .rooms
        .retain(|room| room.id != RoomId::Area("living_room".into()));
    apply_runtime_event(
        &harness.ui,
        &harness.state,
        harness.size,
        RuntimeEvent::Dashboard(dashboard),
    );
    if harness.state.borrow().selected_room.is_some()
        || harness.state.borrow().selected_entity.is_some()
    {
        return Err(PreviewError::Interaction(
            "removed room left stale room or Details selection",
        ));
    }
    reset_interaction_harness(
        harness,
        preview_dashboard().map_err(PreviewError::Fixture)?,
        Settings::default(),
    );
    Ok(())
}

fn qualify_disconnected_command_queue() -> Result<(), PreviewError> {
    let harness = interaction_harness()?;
    drop(harness.received);
    harness
        .ui
        .invoke_activate_entity("light.living_room_lamp".into());
    if harness.ui.get_connection_text().as_str() != "Commands unavailable" {
        return Err(PreviewError::Interaction(
            "disconnected command queue did not show unavailable status",
        ));
    }
    Ok(())
}

fn qualify_runtime_event_disconnect() -> Result<(), PreviewError> {
    let harness = interaction_harness()?;
    state_set_language(&harness, "de");
    let (events, event_rx) = mpsc::sync_channel(1);
    drop(events);
    let timer = start_event_timer(
        &harness.ui,
        Rc::clone(&harness.state),
        event_rx,
        harness.commands.clone(),
        harness.size,
    );
    advance_preview_clock(Duration::from_millis(50));
    if harness.state.borrow().runtime_status != "Runtime getrennt" {
        return Err(PreviewError::Interaction(
            "closed runtime event stream was not reported",
        ));
    }
    timer.stop();
    Ok(())
}

fn dispatch_pointer(ui: &MainWindow, event: slint::platform::WindowEvent) {
    ui.window().dispatch_event(event);
    slint::platform::update_timers_and_animations();
}

fn advance_preview_clock(elapsed: Duration) {
    let mut remaining = elapsed;
    while !remaining.is_zero() {
        let step = remaining.min(Duration::from_millis(10));
        PREVIEW_CLOCK.with(|clock| clock.set(clock.get().saturating_add(step)));
        slint::platform::update_timers_and_animations();
        remaining = remaining.saturating_sub(step);
    }
}

/// A finger that goes from `from_x` to `to_x` at height `y` in eight moves and lets go, followed by
/// the time the page animation needs.
fn dispatch_swipe(ui: &MainWindow, from_x: f32, to_x: f32, y: f32) {
    use slint::platform::{PointerEventButton, WindowEvent};
    let at = |x: f32| slint::LogicalPosition::new(x, y);
    dispatch_pointer(
        ui,
        WindowEvent::PointerPressed {
            position: at(from_x),
            button: PointerEventButton::Left,
        },
    );
    for step in 1..=8_u8 {
        let x = from_x + (to_x - from_x) * f32::from(step) / 8.0;
        dispatch_pointer(ui, WindowEvent::PointerMoved { position: at(x) });
        advance_preview_clock(Duration::from_millis(16));
    }
    dispatch_pointer(
        ui,
        WindowEvent::PointerReleased {
            position: at(to_x),
            button: PointerEventButton::Left,
        },
    );
    advance_preview_clock(Duration::from_millis(400));
}

fn dispatch_vertical_swipe(ui: &MainWindow, x: f32, from_y: f32, to_x: f32, to_y: f32) {
    use slint::platform::{PointerEventButton, WindowEvent};
    dispatch_pointer(
        ui,
        WindowEvent::PointerPressed {
            position: slint::LogicalPosition::new(x, from_y),
            button: PointerEventButton::Left,
        },
    );
    for step in 1..=8_u8 {
        let fraction = f32::from(step) / 8.0;
        let position = slint::LogicalPosition::new(
            (to_x - x).mul_add(fraction, x),
            (to_y - from_y).mul_add(fraction, from_y),
        );
        dispatch_pointer(ui, WindowEvent::PointerMoved { position });
        advance_preview_clock(Duration::from_millis(16));
    }
    dispatch_pointer(
        ui,
        WindowEvent::PointerReleased {
            position: slint::LogicalPosition::new(to_x, to_y),
            button: PointerEventButton::Left,
        },
    );
    advance_preview_clock(Duration::from_millis(400));
}

fn dispatch_horizontal_drag(ui: &MainWindow, from_x: f32, to_x: f32, y: f32) {
    use slint::platform::{PointerEventButton, WindowEvent};
    dispatch_pointer(
        ui,
        WindowEvent::PointerPressed {
            position: slint::LogicalPosition::new(from_x, y),
            button: PointerEventButton::Left,
        },
    );
    for step in 1..=8_u8 {
        let x = from_x + (to_x - from_x) * f32::from(step) / 8.0;
        dispatch_pointer(
            ui,
            WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(x, y),
            },
        );
    }
    dispatch_pointer(
        ui,
        WindowEvent::PointerReleased {
            position: slint::LogicalPosition::new(to_x, y),
            button: PointerEventButton::Left,
        },
    );
}

fn dispatch_click(ui: &MainWindow, position: slint::LogicalPosition) {
    use slint::platform::{PointerEventButton, WindowEvent};
    dispatch_pointer(
        ui,
        WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        },
    );
    dispatch_pointer(
        ui,
        WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        },
    );
}

fn expect_tap(received: &Receiver<RuntimeCommand>) -> Result<(), PreviewError> {
    expect_tap_for(received, "light.living_room_lamp")
}

fn expect_tap_for(
    received: &Receiver<RuntimeCommand>,
    expected_entity: &str,
) -> Result<(), PreviewError> {
    if !matches!(received.try_recv(), Ok(RuntimeCommand::Tap(entity_id)) if entity_id == expected_entity)
    {
        return Err(PreviewError::Interaction(
            "explicit action target did not emit exactly one Tap for its entity",
        ));
    }
    expect_no_command(
        received,
        "explicit action target emitted more than one command",
    )
}

fn expect_no_command(
    received: &Receiver<RuntimeCommand>,
    check: &'static str,
) -> Result<(), PreviewError> {
    if matches!(received.try_recv(), Err(TryRecvError::Empty)) {
        Ok(())
    } else {
        Err(PreviewError::Interaction(check))
    }
}

#[derive(Clone, Copy)]
struct DisplayFixture {
    name: &'static str,
    width: u32,
    height: u32,
    orientation: &'static str,
}

impl DisplayFixture {
    const ALL: [Self; 4] = [
        Self {
            name: "d1001-portrait",
            width: 800,
            height: 1280,
            orientation: "portrait",
        },
        Self {
            name: "d1001-landscape",
            width: 1280,
            height: 800,
            orientation: "landscape",
        },
        Self {
            name: "guition-portrait",
            width: 600,
            height: 1024,
            orientation: "portrait",
        },
        Self {
            name: "guition-landscape",
            width: 1024,
            height: 600,
            orientation: "landscape",
        },
    ];
}

#[derive(Clone, Copy)]
enum PreviewTheme {
    Light,
    Dark,
}

impl PreviewTheme {
    const fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    const fn settings_value(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

#[derive(Clone, Copy)]
enum PreviewContent {
    Rooms,
    LivingRoom,
    Details,
    Favorites,
    FavoriteRooms,
    Mixed,
    Feedback,
    Offline,
    EmptyFavorites,
    BinarySensor,
    Pairing,
    PairingGerman,
    PairingConflict,
    ManyTiles,
    ManyTilesSecondPage,
    RoomsSmallest,
    RoomLarge,
    DetailsLargest,
    PairingLargest,
}

impl PreviewContent {
    const ALL: [Self; 19] = [
        Self::Rooms,
        Self::LivingRoom,
        Self::Details,
        Self::Favorites,
        Self::FavoriteRooms,
        Self::Mixed,
        Self::Feedback,
        Self::Offline,
        Self::EmptyFavorites,
        Self::BinarySensor,
        Self::Pairing,
        Self::PairingGerman,
        Self::PairingConflict,
        Self::ManyTiles,
        Self::ManyTilesSecondPage,
        Self::RoomsSmallest,
        Self::RoomLarge,
        Self::DetailsLargest,
        Self::PairingLargest,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Rooms => "rooms",
            Self::LivingRoom => "room",
            Self::Details => "details",
            Self::Favorites => "favorites",
            Self::FavoriteRooms => "favorite-rooms",
            Self::Mixed => "mixed",
            Self::Feedback => "feedback",
            Self::Offline => "offline",
            Self::EmptyFavorites => "empty-favorites",
            Self::BinarySensor => "binary-sensor",
            Self::Pairing => "pairing",
            Self::PairingGerman => "pairing-de",
            Self::PairingConflict => "pairing-conflict",
            Self::ManyTiles => "many-tiles",
            Self::ManyTilesSecondPage => "many-tiles-page-2",
            Self::RoomsSmallest => "rooms-75",
            Self::RoomLarge => "room-150",
            Self::DetailsLargest => "details-200",
            Self::PairingLargest => "pairing-200",
        }
    }
}

fn preview_dashboard() -> Result<Dashboard, njorun_app_core::CoreError> {
    Ok(demo_core()?.dashboard().clone())
}

/// Populate the real application core with the interactive demo's local rooms and entities.
///
/// # Errors
///
/// Returns a validation error if the built-in fixture is inconsistent.
pub fn demo_core() -> Result<AppCore, njorun_app_core::CoreError> {
    let areas = [
        AreaRegistryEntry {
            area_id: "living_room".into(),
            name: "Living room".into(),
            floor_id: Some("ground_floor".into()),
        },
        AreaRegistryEntry {
            area_id: "kitchen".into(),
            name: "Kitchen".into(),
            floor_id: Some("ground_floor".into()),
        },
        AreaRegistryEntry {
            area_id: "hallway".into(),
            name: "Hallway".into(),
            floor_id: Some("ground_floor".into()),
        },
    ];
    let entities = [
        preview_entity("light.living_room_lamp", Some("living_room")),
        preview_entity("switch.reading_light", Some("living_room")),
        preview_entity("scene.movie_time", Some("living_room")),
        preview_entity("sensor.living_room_temperature", Some("living_room")),
        preview_entity("binary_sensor.hallway_motion", Some("hallway")),
        preview_entity("light.kitchen_main", Some("kitchen")),
        preview_entity("sensor.outdoor_temperature", None),
    ];
    let states = [
        preview_state(
            "light.living_room_lamp",
            "on",
            "Living room lamp",
            "mdi:lightbulb",
            Some(153),
            None,
        ),
        preview_state(
            "switch.reading_light",
            "off",
            "Reading light",
            "mdi:power",
            None,
            None,
        ),
        preview_state(
            "scene.movie_time",
            "2026-11-05T12:00:00+00:00",
            "Movie time",
            "mdi:play",
            None,
            None,
        ),
        preview_state(
            "sensor.living_room_temperature",
            "21.4",
            "Living room temperature",
            "mdi:thermometer",
            None,
            Some("°C"),
        ),
        preview_state(
            "binary_sensor.hallway_motion",
            "on",
            "Hallway motion",
            "mdi:checkbox-blank-circle-outline",
            None,
            None,
        ),
        preview_state(
            "light.kitchen_main",
            "off",
            "Kitchen ceiling light",
            "mdi:lightbulb",
            None,
            None,
        ),
        preview_state(
            "sensor.outdoor_temperature",
            "16.2",
            "Outdoor temperature",
            "mdi:thermometer",
            None,
            Some("°C"),
        ),
    ];

    let mut core = AppCore::new();
    core.refresh_from_ha(&areas, &[], &entities, &states, &DashboardOrder::default())?;
    core.set_connection(ConnectionStatus::Ready);
    Ok(core)
}

fn preview_entity(entity_id: &str, area_id: Option<&str>) -> EntityRegistryEntry {
    EntityRegistryEntry {
        entity_id: entity_id.into(),
        area_id: area_id.map(str::to_owned),
        device_id: None,
        disabled_by: None,
        hidden_by: None,
        entity_category: None,
        name: None,
        icon: None,
    }
}

fn preview_state(
    entity_id: &str,
    value: &str,
    friendly_name: &str,
    icon: &str,
    brightness: Option<u64>,
    unit: Option<&str>,
) -> HaEntityState {
    let mut attributes = BTreeMap::from([
        ("friendly_name".to_owned(), serde_json::json!(friendly_name)),
        ("icon".to_owned(), serde_json::json!(icon)),
    ]);
    if entity_id == "binary_sensor.hallway_motion" {
        attributes.insert("device_class".into(), serde_json::json!("motion"));
    }
    if let Some(brightness) = brightness {
        attributes.insert("brightness".to_owned(), serde_json::json!(brightness));
    }
    if let Some(unit) = unit {
        attributes.insert("unit_of_measurement".to_owned(), serde_json::json!(unit));
    }
    HaEntityState {
        entity_id: entity_id.into(),
        state: value.into(),
        attributes,
        last_updated: Some("2026-11-05T12:00:00+00:00".into()),
    }
}

fn preview_snapshot(dashboard: &Dashboard, content: PreviewContent) -> Dashboard {
    let mut snapshot = dashboard.clone();
    if matches!(
        content,
        PreviewContent::ManyTiles | PreviewContent::ManyTilesSecondPage
    ) {
        // A room with more tiles than one screen holds, so that the pages and the indicator show.
        for room in &mut snapshot.rooms {
            if room.id == RoomId::Area("living_room".into()) {
                let template = room.entities[1].clone();
                for number in 1..=16 {
                    let mut tile = template.clone();
                    tile.entity_id = format!("light.extra_{number}");
                    tile.name = format!("Extra light {number}");
                    room.entities.push(tile);
                }
            }
        }
    }
    if matches!(content, PreviewContent::Offline) {
        snapshot.connection = ConnectionStatus::Offline;
        snapshot.is_stale = true;
        for tile in snapshot
            .rooms
            .iter_mut()
            .flat_map(|room| &mut room.entities)
        {
            tile.is_stale = true;
            tile.command_enabled = false;
        }
    } else if matches!(content, PreviewContent::Feedback) {
        for tile in snapshot
            .rooms
            .iter_mut()
            .flat_map(|room| &mut room.entities)
        {
            match tile.domain {
                EntityDomain::Light => {
                    tile.action_feedback = Some(ActionFeedback::AcceptedAwaitingState {
                        target: TargetState::Off,
                    });
                }
                EntityDomain::Switch => {
                    tile.action_feedback = Some(ActionFeedback::Rejected {
                        reason: "Rejected by Home Assistant".into(),
                    });
                }
                EntityDomain::Scene => tile.action_feedback = Some(ActionFeedback::SceneRequested),
                EntityDomain::Sensor => {
                    tile.state = None;
                    tile.command_enabled = false;
                }
                _ => {}
            }
        }
    }
    snapshot
}

/// The settings a preview frame is rendered with.
fn preview_settings(
    display: DisplayFixture,
    theme: PreviewTheme,
    content: PreviewContent,
    color_scheme: &str,
    increased_contrast: bool,
    reduced_motion: bool,
) -> Settings {
    let (light_color_scheme, dark_color_scheme) = match theme {
        PreviewTheme::Light => (color_scheme, "graphite"),
        PreviewTheme::Dark => ("warm", color_scheme),
    };
    let mut settings = Settings {
        orientation: display.orientation.into(),
        theme: theme.settings_value().into(),
        // Keep the legacy field populated for old-peer serialization checks. The active mode's
        // new field deliberately carries the requested family and the inactive one differs.
        color_scheme: "njorun".into(),
        light_color_scheme: light_color_scheme.into(),
        dark_color_scheme: dark_color_scheme.into(),
        increased_contrast,
        reduced_motion,
        brightness: 100,
        ..Settings::default()
    };
    if matches!(
        content,
        PreviewContent::Feedback
            | PreviewContent::Offline
            | PreviewContent::BinarySensor
            | PreviewContent::PairingGerman
    ) {
        settings.language = "de".into();
    }
    match content {
        PreviewContent::Favorites | PreviewContent::FavoriteRooms => {
            settings.home_view = "favorites".into();
            settings.favorites = vec![
                "light.living_room_lamp".into(),
                "sensor.living_room_temperature".into(),
                "scene.movie_time".into(),
            ];
        }
        PreviewContent::Mixed => {
            settings.home_view = "mixed".into();
            settings.favorites = vec!["light.living_room_lamp".into(), "scene.movie_time".into()];
        }
        PreviewContent::EmptyFavorites => settings.home_view = "favorites".into(),
        _ => {}
    }
    settings.ui_scale = match content {
        PreviewContent::RoomsSmallest => 75,
        PreviewContent::RoomLarge => 150,
        PreviewContent::DetailsLargest | PreviewContent::PairingLargest => 200,
        _ => 100,
    };
    settings
}

#[derive(Clone, Copy)]
struct PreviewAppearance {
    scheme: &'static str,
    increased_contrast: bool,
    reduced_motion: bool,
}

impl PreviewAppearance {
    fn frame_name(
        self,
        display: DisplayFixture,
        theme: PreviewTheme,
        content: PreviewContent,
    ) -> String {
        format!(
            "{}-{}-{}-{}-{}-{}",
            display.name,
            theme.name(),
            self.scheme,
            if self.increased_contrast {
                "contrast-high"
            } else {
                "contrast-normal"
            },
            if self.reduced_motion {
                "motion-reduced"
            } else {
                "motion-full"
            },
            content.name()
        )
    }
}

fn render_preview_frame(
    ui: &MainWindow,
    window: &MinimalSoftwareWindow,
    dashboard: &Dashboard,
    display: DisplayFixture,
    theme: PreviewTheme,
    content: PreviewContent,
    appearance: Option<PreviewAppearance>,
) -> Result<PreviewFrame, PreviewError> {
    let name = appearance.map_or_else(
        || format!("{}-{}-{}", display.name, theme.name(), content.name()),
        |appearance| appearance.frame_name(display, theme, content),
    );
    let PreviewAppearance {
        scheme,
        increased_contrast,
        reduced_motion,
    } = appearance.unwrap_or(PreviewAppearance {
        scheme: "njorun",
        increased_contrast: false,
        reduced_motion: false,
    });
    let settings = preview_settings(
        display,
        theme,
        content,
        scheme,
        increased_contrast,
        reduced_motion,
    );
    let expected_color_scheme = settings
        .effective_color_scheme(matches!(theme, PreviewTheme::Dark))
        .to_owned();
    let mut state = ViewState::with_settings(AppliedSettings {
        revision: 0,
        settings,
    });
    state.dashboard = Some(preview_snapshot(dashboard, content));
    state.selected_room = match content {
        PreviewContent::LivingRoom
        | PreviewContent::ManyTiles
        | PreviewContent::ManyTilesSecondPage
        | PreviewContent::RoomLarge
        | PreviewContent::DetailsLargest
        | PreviewContent::Details
        | PreviewContent::Feedback
        | PreviewContent::Offline => Some(RoomId::Area("living_room".into())),
        PreviewContent::BinarySensor => Some(RoomId::Area("hallway".into())),
        _ => None,
    };
    state.selected_entity = matches!(
        content,
        PreviewContent::Details | PreviewContent::DetailsLargest
    )
    .then(|| "light.living_room_lamp".into());
    state.rooms_page = matches!(content, PreviewContent::FavoriteRooms);

    // Fixture telemetry keeps the preview reproducible on hosts without a network.
    ui.set_network_link(
        if display.orientation == "portrait" {
            "wifi3"
        } else {
            "ethernet"
        }
        .into(),
    );
    state.clock_label = "9:41 AM".into();
    let size = PhysicalSize::new(display.width, display.height);
    apply_settings(ui, &state, size);
    render(ui, &state);
    if ui.get_color_scheme().as_str() != expected_color_scheme
        || ui.get_increased_contrast() != increased_contrast
        || ui.get_reduced_motion() != reduced_motion
        || ui.get_display_dim_opacity() != 0.0
    {
        return Err(PreviewError::Interaction(
            "preview settings were not applied before rendering",
        ));
    }
    let (pairing_stage, pairing_code) = match content {
        PreviewContent::Pairing
        | PreviewContent::PairingGerman
        | PreviewContent::PairingLargest => ("approve", "360 971"),
        PreviewContent::PairingConflict => ("conflict", ""),
        _ => ("", ""),
    };
    ui.set_pairing_stage(pairing_stage.into());
    ui.set_pairing_code(pairing_code.into());
    if matches!(content, PreviewContent::ManyTilesSecondPage) {
        // A swipe to the left over the grid, as a finger makes it.
        let dimension = |pixels: u32| f32::from(u16::try_from(pixels).unwrap_or(u16::MAX));
        dispatch_swipe(
            ui,
            dimension(display.width) * 0.8,
            dimension(display.width) * 0.2,
            dimension(display.height) / 2.0,
        );
    }
    draw_preview_frame(window, display, name)
}
