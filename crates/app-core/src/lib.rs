//! Board-neutral Home Assistant entity and dashboard semantics for Njörun.
//!
//! Network code supplies ordered Home Assistant registry/state snapshots and applies returned
//! service calls. This crate owns filtering, area resolution, ordering, stale-state flags, and
//! the distinction between command acknowledgements and authoritative state updates.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod actions;
pub mod display;
pub mod identity;
pub mod install;
pub mod live;
pub mod logging;
pub mod orientation;
pub mod presentation;
pub mod problems;
pub mod settings;

pub use actions::{AlarmMode, DetailAction, ServiceCall};

pub const MAX_ENTITY_COUNT: usize = 512;
pub const MAX_AREA_COUNT: usize = 128;
pub const MAX_DEVICE_COUNT: usize = 1024;
pub const MAX_ENTITY_REGISTRY_COUNT: usize = 4096;
pub const MAX_STATE_COUNT: usize = 4096;
pub const MAX_NAME_CHARS: usize = 128;
pub const MAX_IDENTIFIER_CHARS: usize = 255;
pub const MAX_STATE_CHARS: usize = 256;
pub const MAX_ATTRIBUTE_COUNT: usize = 64;
pub const MAX_ATTRIBUTE_KEY_CHARS: usize = 64;
pub const MAX_ACTION_REASON_CHARS: usize = 256;
pub const MAX_LAST_UPDATED_CHARS: usize = 64;

/// A raw state received from Home Assistant's state API or state change event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HaEntityState {
    pub entity_id: String,
    pub state: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, Value>,
    #[serde(default)]
    pub last_updated: Option<String>,
}

/// An area registry row. The input slice order is Home Assistant room order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AreaRegistryEntry {
    pub area_id: String,
    pub name: String,
    #[serde(default)]
    pub floor_id: Option<String>,
}

/// A device registry row used as an entity area's fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRegistryEntry {
    pub id: String,
    #[serde(default)]
    pub area_id: Option<String>,
    #[serde(default)]
    pub via_device_id: Option<String>,
}

/// An entity registry row relevant to automatic room population.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityRegistryEntry {
    pub entity_id: String,
    #[serde(default)]
    pub area_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub disabled_by: Option<String>,
    #[serde(default)]
    pub hidden_by: Option<String>,
    #[serde(default)]
    pub entity_category: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
}

/// Stable identity for an automatically generated room.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "area_id", rename_all = "snake_case")]
pub enum RoomId {
    Area(String),
    Unassigned,
}

/// Explicit per-panel room and tile order.
///
/// Registry/discovery order is only the fallback. A caller may supply the Home overview order
/// after applying the supported Home Assistant frontend strategy for its version. New items
/// append in the supplied fallback order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardOrder {
    #[serde(default)]
    pub area_ids: Option<Vec<String>>,
    #[serde(default)]
    pub entity_ids_by_room: BTreeMap<RoomId, Vec<String>>,
}

/// Connection readiness for dashboard state and commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    Offline,
    Synchronizing,
    Ready,
}

impl ConnectionStatus {
    #[must_use]
    pub const fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }

    #[must_use]
    pub const fn is_offline(self) -> bool {
        matches!(self, Self::Offline)
    }
}

/// Entity domain shown on the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityDomain {
    Light,
    Switch,
    Scene,
    Sensor,
    BinarySensor,
    Fan,
    Lock,
    MediaPlayer,
    Camera,
    AlarmControlPanel,
    Script,
    Automation,
    InputBoolean,
    Button,
    InputButton,
    Number,
    InputNumber,
    Select,
    InputSelect,
}

/// What a plain tap on a tile of this domain does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapBehavior {
    /// Switch between on and off.
    Toggle,
    /// Run or press, with no state to wait for.
    Press,
    /// Pause or resume playback.
    PlayPause,
    /// Open the details sheet, which holds the controls.
    Inspect,
}

impl EntityDomain {
    fn from_entity_id(entity_id: &str) -> Option<Self> {
        let (domain, object_id) = entity_id.split_once('.')?;
        if object_id.is_empty() || object_id.contains('.') {
            return None;
        }
        match domain {
            "light" => Some(Self::Light),
            "switch" => Some(Self::Switch),
            "scene" => Some(Self::Scene),
            "sensor" => Some(Self::Sensor),
            "binary_sensor" => Some(Self::BinarySensor),
            "fan" => Some(Self::Fan),
            "lock" => Some(Self::Lock),
            "media_player" => Some(Self::MediaPlayer),
            "camera" => Some(Self::Camera),
            "alarm_control_panel" => Some(Self::AlarmControlPanel),
            "script" => Some(Self::Script),
            "automation" => Some(Self::Automation),
            "input_boolean" => Some(Self::InputBoolean),
            "button" => Some(Self::Button),
            "input_button" => Some(Self::InputButton),
            "number" => Some(Self::Number),
            "input_number" => Some(Self::InputNumber),
            "select" => Some(Self::Select),
            "input_select" => Some(Self::InputSelect),
            _ => None,
        }
    }

    /// The Home Assistant service domain, which equals the entity ID prefix.
    #[must_use]
    pub const fn service_domain(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Switch => "switch",
            Self::Scene => "scene",
            Self::Sensor => "sensor",
            Self::BinarySensor => "binary_sensor",
            Self::Fan => "fan",
            Self::Lock => "lock",
            Self::MediaPlayer => "media_player",
            Self::Camera => "camera",
            Self::AlarmControlPanel => "alarm_control_panel",
            Self::Script => "script",
            Self::Automation => "automation",
            Self::InputBoolean => "input_boolean",
            Self::Button => "button",
            Self::InputButton => "input_button",
            Self::Number => "number",
            Self::InputNumber => "input_number",
            Self::Select => "select",
            Self::InputSelect => "input_select",
        }
    }

    #[must_use]
    pub const fn tap_behavior(self) -> TapBehavior {
        match self {
            Self::Light | Self::Switch | Self::Fan | Self::InputBoolean | Self::Automation => {
                TapBehavior::Toggle
            }
            Self::Scene | Self::Script | Self::Button | Self::InputButton => TapBehavior::Press,
            Self::MediaPlayer => TapBehavior::PlayPause,
            Self::Sensor
            | Self::BinarySensor
            | Self::Camera
            | Self::Lock
            | Self::AlarmControlPanel
            | Self::Number
            | Self::InputNumber
            | Self::Select
            | Self::InputSelect => TapBehavior::Inspect,
        }
    }

    const fn supports_action(self) -> bool {
        !matches!(self, Self::Sensor | Self::BinarySensor | Self::Camera)
    }
}

/// Lock state a lock action waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockTarget {
    Locked,
    Unlocked,
}

/// Playback state a play/pause action waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackTarget {
    Playing,
    Paused,
}

/// Target state inferred from a light or switch's current HA state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetState {
    On,
    Off,
}

impl TargetState {
    fn from_toggle_state(state: &str) -> Option<Self> {
        match state {
            "on" => Some(Self::Off),
            "off" => Some(Self::On),
            _ => None,
        }
    }

    fn matches(self, state: &str) -> bool {
        matches!((self, state), (Self::On, "on") | (Self::Off, "off"))
    }
}

/// Home Assistant service exposed by a primary dashboard tap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeAssistantService {
    LightToggle,
    SwitchToggle,
    SceneTurnOn,
    FanToggle,
    InputBooleanToggle,
    AutomationToggle,
    ScriptTurnOn,
    ButtonPress,
    InputButtonPress,
    MediaPlayPause,
}

impl HomeAssistantService {
    /// The call to send; the front end adds the entity as target.
    #[must_use]
    pub fn call(self) -> ServiceCall {
        let (domain, service) = match self {
            Self::LightToggle => ("light", "toggle"),
            Self::SwitchToggle => ("switch", "toggle"),
            Self::SceneTurnOn => ("scene", "turn_on"),
            Self::FanToggle => ("fan", "toggle"),
            Self::InputBooleanToggle => ("input_boolean", "toggle"),
            Self::AutomationToggle => ("automation", "toggle"),
            Self::ScriptTurnOn => ("script", "turn_on"),
            Self::ButtonPress => ("button", "press"),
            Self::InputButtonPress => ("input_button", "press"),
            Self::MediaPlayPause => ("media_player", "media_play_pause"),
        };
        ServiceCall {
            domain,
            service,
            data: Value::Object(serde_json::Map::new()),
        }
    }
}

/// Result of a primary tile tap: open details or send a single HA service call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrimaryTap {
    Inspect {
        entity_id: String,
        unavailable: bool,
    },
    CallService {
        entity_id: String,
        service: HomeAssistantService,
    },
    /// A call built from a details-sheet control.
    Call {
        entity_id: String,
        call: ServiceCall,
    },
}

impl PrimaryTap {
    /// The entity and call to send, for taps that call a service.
    #[must_use]
    pub fn into_call(self) -> Option<(String, ServiceCall)> {
        match self {
            Self::Inspect { .. } => None,
            Self::CallService { entity_id, service } => Some((entity_id, service.call())),
            Self::Call { entity_id, call } => Some((entity_id, call)),
        }
    }
}

/// Kind of in-flight Home Assistant action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "target", rename_all = "snake_case")]
pub enum ActionKind {
    Toggle(TargetState),
    /// Run a scene, script or button, or step a track: nothing to wait for.
    RunScene,
    SetBrightness(u8),
    SetFanSpeed(u8),
    SetVolume(u8),
    /// Target value in thousandths of a unit.
    SetNumber(i64),
    SelectOption(String),
    Lock(LockTarget),
    Playback(PlaybackTarget),
    /// Alarm panel state that confirms the action, such as `armed_away` or `disarmed`.
    Alarm(String),
}

const LEVEL_TOLERANCE: i64 = 3;

impl ActionKind {
    /// Whether a fresh state proves the action took effect.
    fn is_satisfied_by(&self, state: &HaEntityState) -> bool {
        let level = |key: &str, scale: f64| {
            state
                .attributes
                .get(key)
                .and_then(Value::as_f64)
                .map(|value| (value * scale).round())
        };
        let near = |value: Option<f64>, target: u8| {
            #[allow(clippy::cast_possible_truncation)]
            value.is_some_and(|value| (value as i64 - i64::from(target)).abs() <= LEVEL_TOLERANCE)
        };
        match self {
            Self::Toggle(target) => target.matches(&state.state),
            Self::RunScene => false,
            Self::SetBrightness(percent) => {
                state.state == "on" && near(level("brightness", 100.0 / 255.0), *percent)
            }
            Self::SetFanSpeed(percent) => {
                if *percent == 0 {
                    state.state == "off" || near(level("percentage", 1.0), 0)
                } else {
                    state.state == "on" && near(level("percentage", 1.0), *percent)
                }
            }
            Self::SetVolume(percent) => near(level("volume_level", 100.0), *percent),
            Self::SetNumber(target) => state
                .state
                .parse::<f64>()
                .is_ok_and(|value| (actions::milli(value) - target).abs() <= 50),
            Self::SelectOption(option) => &state.state == option,
            Self::Lock(LockTarget::Locked) => state.state == "locked",
            Self::Lock(LockTarget::Unlocked) => state.state == "unlocked",
            Self::Playback(PlaybackTarget::Playing) => state.state == "playing",
            Self::Playback(PlaybackTarget::Paused) => state.state == "paused",
            Self::Alarm(target) => &state.state == target,
        }
    }

    /// Feedback to show once the action is confirmed.
    const fn confirmed_feedback(&self) -> Option<ActionFeedback> {
        match self {
            Self::Toggle(state) => Some(ActionFeedback::Confirmed { state: *state }),
            _ => None,
        }
    }
}

/// Feedback that distinguishes HA receipt from observed entity state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ActionFeedback {
    AwaitingAcknowledgement {
        action: ActionKind,
    },
    AcceptedAwaitingState {
        target: TargetState,
    },
    /// HA accepted a level, lock, media, select or alarm action; the new state is awaited.
    AcceptedAwaitingConfirmation {
        action: ActionKind,
    },
    /// HA accepted a scene request; this is not confirmation of physical outcome.
    SceneRequested,
    /// A fresh HA state matched the requested light or switch state.
    Confirmed {
        state: TargetState,
    },
    Rejected {
        reason: String,
    },
    Unconfirmed {
        action: ActionKind,
    },
}

/// A visible entity tile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityTile {
    pub entity_id: String,
    pub name: String,
    pub icon: Option<String>,
    pub domain: EntityDomain,
    pub state: Option<HaEntityState>,
    pub is_stale: bool,
    pub command_enabled: bool,
    pub action_feedback: Option<ActionFeedback>,
}

impl EntityTile {
    #[must_use]
    pub fn is_available(&self) -> bool {
        // Scenes, scripts and buttons report `unknown` until first used; they still work.
        let may_be_unknown = matches!(
            self.domain,
            EntityDomain::Scene
                | EntityDomain::Script
                | EntityDomain::Button
                | EntityDomain::InputButton
        );
        self.state.as_ref().is_some_and(|state| {
            state.state != "unavailable" && (may_be_unknown || state.state != "unknown")
        })
    }

    #[must_use]
    pub fn state_value(&self) -> Option<&str> {
        self.state.as_ref().map(|state| state.state.as_str())
    }
}

/// A room and its ordered tiles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Room {
    pub id: RoomId,
    pub name: String,
    pub entities: Vec<EntityTile>,
}

/// Dashboard model built from HA's ordered registry and state data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dashboard {
    pub connection: ConnectionStatus,
    pub is_stale: bool,
    pub rooms: Vec<Room>,
}

impl Dashboard {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rooms.is_empty()
    }

    #[must_use]
    pub fn entity(&self, entity_id: &str) -> Option<&EntityTile> {
        self.rooms
            .iter()
            .flat_map(|room| &room.entities)
            .find(|tile| tile.entity_id == entity_id)
    }

    fn entity_mut(&mut self, entity_id: &str) -> Option<&mut EntityTile> {
        self.rooms
            .iter_mut()
            .flat_map(|room| &mut room.entities)
            .find(|tile| tile.entity_id == entity_id)
    }

    fn set_connection(&mut self, connection: ConnectionStatus) {
        self.connection = connection;
        self.is_stale = !connection.is_ready();
        for tile in self.rooms.iter_mut().flat_map(|room| &mut room.entities) {
            tile.is_stale = self.is_stale;
            tile.command_enabled =
                !self.is_stale && tile.is_available() && tile.domain.supports_action();
        }
    }
}

/// Errors found while building a bounded dashboard snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreError {
    TooManyAreas,
    TooManyDevices,
    TooManyEntityRegistryEntries,
    TooManyEligibleEntities,
    TooManyStates,
    TooManyAttributes,
    IdentifierTooLong,
    NameTooLong,
    StateTooLong,
    AttributeKeyTooLong,
    DuplicateAreaId,
    DuplicateDeviceId,
    DuplicateEntityId,
    InvalidEntityId,
    ActionReasonTooLong,
}

impl Display for CoreError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::TooManyAreas => "Home Assistant area count exceeds the panel limit",
            Self::TooManyDevices => "Home Assistant device count exceeds the panel limit",
            Self::TooManyEntityRegistryEntries => {
                "Home Assistant entity registry count exceeds the panel limit"
            }
            Self::TooManyEligibleEntities => "eligible entity count exceeds the panel limit",
            Self::TooManyStates => "Home Assistant state count exceeds the panel limit",
            Self::TooManyAttributes => "entity attribute count exceeds the panel limit",
            Self::IdentifierTooLong => "Home Assistant identifier exceeds the panel limit",
            Self::NameTooLong => "Home Assistant name exceeds the panel limit",
            Self::StateTooLong => "Home Assistant state exceeds the panel limit",
            Self::AttributeKeyTooLong => "entity attribute key exceeds the panel limit",
            Self::DuplicateAreaId => "Home Assistant area IDs must be unique",
            Self::DuplicateDeviceId => "Home Assistant device IDs must be unique",
            Self::DuplicateEntityId => "Home Assistant entity IDs must be unique",
            Self::InvalidEntityId => "Home Assistant entity ID is malformed",
            Self::ActionReasonTooLong => "Home Assistant action reason exceeds the panel limit",
        };
        formatter.write_str(message)
    }
}

impl Error for CoreError {}

/// Errors returned when a primary tap cannot be handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionError {
    Offline,
    Synchronizing,
    UnknownEntity,
    AlreadyPending,
    Unsupported,
    Unavailable,
    NoPendingAction,
    InvalidReason,
    InvalidValue,
}

impl Display for ActionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Offline => "Home Assistant is offline",
            Self::Synchronizing => "Home Assistant state is synchronizing",
            Self::UnknownEntity => "entity is not present in the current dashboard",
            Self::AlreadyPending => "an action for this entity is already pending",
            Self::Unsupported => "entity has no supported primary action",
            Self::Unavailable => "entity state is unavailable",
            Self::NoPendingAction => "entity has no pending action",
            Self::InvalidReason => "Home Assistant action reason exceeds the panel limit",
            Self::InvalidValue => "the requested value is not valid for this entity",
        };
        formatter.write_str(message)
    }
}

impl Error for ActionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingAction {
    kind: ActionKind,
}

/// Caller request to refresh an entity after an action timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRefreshRequest {
    pub entity_id: String,
}

/// Home Assistant's response to a service request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionAcknowledgement {
    Accepted,
    Rejected { reason: String },
}

/// Stateful core model used by the UI and simulator.
#[derive(Debug, Clone)]
pub struct AppCore {
    dashboard: Dashboard,
    pending_actions: HashMap<String, PendingAction>,
}

impl Default for AppCore {
    fn default() -> Self {
        Self::new()
    }
}

impl AppCore {
    /// Create a panel core with an empty, offline dashboard.
    #[must_use]
    pub fn new() -> Self {
        Self {
            dashboard: Dashboard {
                connection: ConnectionStatus::Offline,
                is_stale: true,
                rooms: Vec::new(),
            },
            pending_actions: HashMap::new(),
        }
    }

    /// Read the current dashboard model.
    #[must_use]
    pub const fn dashboard(&self) -> &Dashboard {
        &self.dashboard
    }

    /// Change connection state and update stale/action flags.
    ///
    /// Disconnecting marks in-flight requests unconfirmed and drops them. The core never queues
    /// or replays a request after reconnection.
    pub fn set_connection(&mut self, connection: ConnectionStatus) {
        if connection == ConnectionStatus::Offline {
            let pending = std::mem::take(&mut self.pending_actions);
            for (entity_id, action) in pending {
                if let Some(tile) = self.dashboard.entity_mut(&entity_id) {
                    tile.action_feedback = Some(ActionFeedback::Unconfirmed {
                        action: action.kind,
                    });
                }
            }
        }
        self.dashboard.set_connection(connection);
    }

    /// Build or replace the dashboard from an ordered HA registry/state snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot exceeds a configured bound or contains duplicate
    /// registry identities.
    pub fn refresh_from_ha(
        &mut self,
        areas: &[AreaRegistryEntry],
        devices: &[DeviceRegistryEntry],
        entities: &[EntityRegistryEntry],
        states: &[HaEntityState],
        order: &DashboardOrder,
    ) -> Result<(), CoreError> {
        let mut dashboard = build_dashboard(
            areas,
            devices,
            entities,
            states,
            order,
            self.dashboard.connection,
        )?;

        let previous_feedback: HashMap<_, _> = self
            .dashboard
            .rooms
            .iter()
            .flat_map(|room| &room.entities)
            .filter_map(|tile| {
                tile.action_feedback
                    .clone()
                    .map(|feedback| (tile.entity_id.clone(), feedback))
            })
            .collect();

        self.pending_actions
            .retain(|entity_id, _| dashboard.entity(entity_id).is_some());

        for tile in dashboard
            .rooms
            .iter_mut()
            .flat_map(|room| &mut room.entities)
        {
            if let Some(feedback) = previous_feedback.get(&tile.entity_id) {
                // A fresh snapshot clears an unconfirmed marker once it supplies a current
                // value. This displays that value; it does not claim the action caused it.
                if !matches!(feedback, ActionFeedback::Unconfirmed { .. }) || tile.state.is_none() {
                    tile.action_feedback = Some(feedback.clone());
                }
            }

            if let Some(pending) = self.pending_actions.get(&tile.entity_id)
                && tile
                    .state
                    .as_ref()
                    .is_some_and(|state| pending.kind.is_satisfied_by(state))
            {
                tile.action_feedback = pending.kind.confirmed_feedback();
                self.pending_actions.remove(&tile.entity_id);
            }
        }

        self.dashboard = dashboard;
        Ok(())
    }

    /// Apply one fresh, authoritative HA state update.
    ///
    /// # Errors
    ///
    /// Returns an error if the state exceeds an attribute, name, identifier, or value bound.
    pub fn apply_state(&mut self, state: HaEntityState) -> Result<(), CoreError> {
        validate_state(&state)?;
        let entity_id = state.entity_id.clone();
        let is_ready = self.dashboard.connection.is_ready();
        let Some(tile) = self.dashboard.entity_mut(&entity_id) else {
            return Ok(());
        };

        tile.name = display_name(&state, &tile.name)?;
        tile.icon = icon_from(&state).or_else(|| tile.icon.clone());
        tile.state = Some(state);
        tile.is_stale = !is_ready;
        tile.command_enabled = is_ready && tile.is_available() && tile.domain.supports_action();

        if matches!(
            tile.action_feedback,
            Some(ActionFeedback::Unconfirmed { .. })
        ) {
            tile.action_feedback = None;
        }

        if let Some(pending) = self.pending_actions.get(&entity_id)
            && tile
                .state
                .as_ref()
                .is_some_and(|ha_state| pending.kind.is_satisfied_by(ha_state))
        {
            tile.action_feedback = pending.kind.confirmed_feedback();
            self.pending_actions.remove(&entity_id);
        }
        Ok(())
    }

    /// Handle a primary tile tap.
    ///
    /// Read-only sensors return Inspect even while offline. Commands are returned only while
    /// the connection is ready; no offline command is queued.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown entity, an ineligible connection state, or an
    /// already-pending action.
    pub fn primary_tap(&mut self, entity_id: &str) -> Result<PrimaryTap, ActionError> {
        let Some(tile) = self.dashboard.entity(entity_id) else {
            return Err(ActionError::UnknownEntity);
        };
        let domain = tile.domain;
        let available = tile.is_available();
        let current_state = tile.state.as_ref().map(|state| state.state.clone());
        let behavior = domain.tap_behavior();
        if behavior == TapBehavior::Inspect || !available {
            return Ok(PrimaryTap::Inspect {
                entity_id: entity_id.to_owned(),
                unavailable: !available,
            });
        }
        self.ensure_ready_for_action(entity_id)?;

        let (service, kind) = match behavior {
            TapBehavior::Toggle => {
                let Some(target) = current_state
                    .as_deref()
                    .and_then(TargetState::from_toggle_state)
                else {
                    return Err(ActionError::Unavailable);
                };
                let service = match domain {
                    EntityDomain::Light => HomeAssistantService::LightToggle,
                    EntityDomain::Switch => HomeAssistantService::SwitchToggle,
                    EntityDomain::Fan => HomeAssistantService::FanToggle,
                    EntityDomain::InputBoolean => HomeAssistantService::InputBooleanToggle,
                    EntityDomain::Automation => HomeAssistantService::AutomationToggle,
                    _ => return Err(ActionError::Unsupported),
                };
                (service, ActionKind::Toggle(target))
            }
            TapBehavior::Press => {
                let service = match domain {
                    EntityDomain::Scene => HomeAssistantService::SceneTurnOn,
                    EntityDomain::Script => HomeAssistantService::ScriptTurnOn,
                    EntityDomain::Button => HomeAssistantService::ButtonPress,
                    EntityDomain::InputButton => HomeAssistantService::InputButtonPress,
                    _ => return Err(ActionError::Unsupported),
                };
                (service, ActionKind::RunScene)
            }
            TapBehavior::PlayPause => {
                let target = match current_state.as_deref() {
                    Some("playing") => PlaybackTarget::Paused,
                    Some("paused" | "idle" | "on" | "standby" | "buffering") => {
                        PlaybackTarget::Playing
                    }
                    // Off players cannot resume; the details sheet explains why.
                    _ => {
                        return Ok(PrimaryTap::Inspect {
                            entity_id: entity_id.to_owned(),
                            unavailable: false,
                        });
                    }
                };
                (
                    HomeAssistantService::MediaPlayPause,
                    ActionKind::Playback(target),
                )
            }
            TapBehavior::Inspect => return Err(ActionError::Unsupported),
        };

        self.begin_action(entity_id, kind);
        Ok(PrimaryTap::CallService {
            entity_id: entity_id.to_owned(),
            service,
        })
    }

    /// Start an action chosen in the details sheet.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown or unavailable entity, an action the entity's domain
    /// does not offer, an invalid value, an offline connection or an already-pending action.
    pub fn request_action(
        &mut self,
        entity_id: &str,
        action: &DetailAction,
    ) -> Result<PrimaryTap, ActionError> {
        let Some(tile) = self.dashboard.entity(entity_id) else {
            return Err(ActionError::UnknownEntity);
        };
        if !tile.is_available() {
            return Err(ActionError::Unavailable);
        }
        let (kind, call) = action.plan(tile)?;
        self.ensure_ready_for_action(entity_id)?;
        self.begin_action(entity_id, kind);
        Ok(PrimaryTap::Call {
            entity_id: entity_id.to_owned(),
            call,
        })
    }

    fn ensure_ready_for_action(&self, entity_id: &str) -> Result<(), ActionError> {
        match self.dashboard.connection {
            ConnectionStatus::Offline => return Err(ActionError::Offline),
            ConnectionStatus::Synchronizing => return Err(ActionError::Synchronizing),
            ConnectionStatus::Ready => {}
        }
        if self.pending_actions.contains_key(entity_id) {
            return Err(ActionError::AlreadyPending);
        }
        Ok(())
    }

    fn begin_action(&mut self, entity_id: &str, kind: ActionKind) {
        self.pending_actions
            .insert(entity_id.to_owned(), PendingAction { kind: kind.clone() });
        if let Some(tile) = self.dashboard.entity_mut(entity_id) {
            tile.action_feedback = Some(ActionFeedback::AwaitingAcknowledgement { action: kind });
        }
    }

    /// Apply a HA acknowledgement for a request returned by the primary tap method.
    ///
    /// Accepted lights/switches remain pending until fresh state matches the requested value.
    /// A scene acknowledgement yields a requested state, which confirms receipt only.
    ///
    /// # Errors
    ///
    /// Returns an error if the entity has no pending action or the rejection reason is longer
    /// than the configured bound.
    pub fn acknowledge(
        &mut self,
        entity_id: &str,
        acknowledgement: ActionAcknowledgement,
    ) -> Result<(), ActionError> {
        let Some(pending) = self.pending_actions.get(entity_id).cloned() else {
            return Err(ActionError::NoPendingAction);
        };
        match acknowledgement {
            ActionAcknowledgement::Accepted => match pending.kind {
                ActionKind::RunScene => {
                    self.pending_actions.remove(entity_id);
                    if let Some(tile) = self.dashboard.entity_mut(entity_id) {
                        tile.action_feedback = Some(ActionFeedback::SceneRequested);
                    }
                }
                kind => {
                    // Home Assistant sends no state change when the target already holds, so
                    // check the current state before waiting for one.
                    let satisfied = self
                        .dashboard
                        .entity(entity_id)
                        .and_then(|tile| tile.state.as_ref())
                        .is_some_and(|state| kind.is_satisfied_by(state));
                    if satisfied {
                        self.pending_actions.remove(entity_id);
                    }
                    if let Some(tile) = self.dashboard.entity_mut(entity_id) {
                        tile.action_feedback = if satisfied {
                            kind.confirmed_feedback()
                        } else if let ActionKind::Toggle(target) = kind {
                            Some(ActionFeedback::AcceptedAwaitingState { target })
                        } else {
                            Some(ActionFeedback::AcceptedAwaitingConfirmation { action: kind })
                        };
                    }
                }
            },
            ActionAcknowledgement::Rejected { reason } => {
                validate_reason(&reason).map_err(|_| ActionError::InvalidReason)?;
                self.pending_actions.remove(entity_id);
                if let Some(tile) = self.dashboard.entity_mut(entity_id) {
                    tile.action_feedback = Some(ActionFeedback::Rejected { reason });
                }
            }
        }
        Ok(())
    }

    /// Mark a request unconfirmed after timeout and ask the caller to refresh state.
    ///
    /// This never retries the service call.
    ///
    /// # Errors
    ///
    /// Returns an error if the entity has no pending action.
    pub fn mark_unconfirmed(
        &mut self,
        entity_id: &str,
    ) -> Result<StateRefreshRequest, ActionError> {
        let Some(pending) = self.pending_actions.remove(entity_id) else {
            return Err(ActionError::NoPendingAction);
        };
        if let Some(tile) = self.dashboard.entity_mut(entity_id) {
            tile.action_feedback = Some(ActionFeedback::Unconfirmed {
                action: pending.kind,
            });
        }
        Ok(StateRefreshRequest {
            entity_id: entity_id.to_owned(),
        })
    }

    /// Clear transient feedback, such as a scene's brief Requested display.
    ///
    /// # Errors
    ///
    /// Returns an error if the entity is no longer in the dashboard.
    pub fn clear_action_feedback(&mut self, entity_id: &str) -> Result<(), ActionError> {
        let Some(tile) = self.dashboard.entity_mut(entity_id) else {
            return Err(ActionError::UnknownEntity);
        };
        tile.action_feedback = None;
        Ok(())
    }
}

/// Build a dashboard with HA input order and optional explicit panel ordering.
///
/// # Errors
///
/// Returns an error when input sizes exceed configured bounds, IDs are duplicated or
/// malformed, or a displayed name/attribute exceeds its limit.
pub fn build_dashboard(
    areas: &[AreaRegistryEntry],
    devices: &[DeviceRegistryEntry],
    entities: &[EntityRegistryEntry],
    states: &[HaEntityState],
    order: &DashboardOrder,
    connection: ConnectionStatus,
) -> Result<Dashboard, CoreError> {
    validate_snapshot_sizes(areas, devices, entities, states)?;
    validate_order(order)?;
    let device_index = device_area_index(devices)?;
    let state_index = state_index(states)?;
    let EntityGroups {
        base_rooms,
        room_names,
        groups,
        unassigned,
    } = build_entity_groups(areas, entities, &device_index, &state_index, connection)?;
    let rooms = assemble_rooms(&base_rooms, &room_names, groups, unassigned, order);

    Ok(Dashboard {
        connection,
        is_stale: !connection.is_ready(),
        rooms,
    })
}

type DeviceIndex = HashMap<String, (Option<String>, Option<String>)>;
type StateIndex<'a> = HashMap<&'a str, &'a HaEntityState>;
type TileGroups = HashMap<RoomId, Vec<EntityTile>>;

struct EntityGroups {
    base_rooms: Vec<RoomId>,
    room_names: HashMap<String, String>,
    groups: TileGroups,
    unassigned: Vec<EntityTile>,
}

fn area_name_index(areas: &[AreaRegistryEntry]) -> Result<HashMap<String, String>, CoreError> {
    let mut names = HashMap::with_capacity(areas.len());
    for area in areas {
        validate_identifier(&area.area_id)?;
        validate_name(&area.name)?;
        if names
            .insert(area.area_id.clone(), area.name.clone())
            .is_some()
        {
            return Err(CoreError::DuplicateAreaId);
        }
    }
    Ok(names)
}

fn device_area_index(devices: &[DeviceRegistryEntry]) -> Result<DeviceIndex, CoreError> {
    let mut index = HashMap::with_capacity(devices.len());
    for device in devices {
        validate_identifier(&device.id)?;
        if let Some(area_id) = &device.area_id {
            validate_identifier(area_id)?;
        }
        if let Some(parent_id) = &device.via_device_id {
            validate_identifier(parent_id)?;
        }
        if index
            .insert(
                device.id.clone(),
                (device.area_id.clone(), device.via_device_id.clone()),
            )
            .is_some()
        {
            return Err(CoreError::DuplicateDeviceId);
        }
    }
    Ok(index)
}

fn state_index(states: &[HaEntityState]) -> Result<StateIndex<'_>, CoreError> {
    let mut index = HashMap::with_capacity(states.len());
    for state in states {
        validate_state(state)?;
        if index.insert(state.entity_id.as_str(), state).is_some() {
            return Err(CoreError::DuplicateEntityId);
        }
    }
    Ok(index)
}

fn build_entity_groups(
    areas: &[AreaRegistryEntry],
    entities: &[EntityRegistryEntry],
    devices: &DeviceIndex,
    states: &StateIndex<'_>,
    connection: ConnectionStatus,
) -> Result<EntityGroups, CoreError> {
    let mut room_names = area_name_index(areas)?;
    let mut base_rooms: Vec<RoomId> = areas
        .iter()
        .map(|area| RoomId::Area(area.area_id.clone()))
        .collect();
    let mut groups = TileGroups::new();
    for area in areas {
        groups
            .entry(RoomId::Area(area.area_id.clone()))
            .or_default();
    }

    let mut entity_ids = HashSet::with_capacity(entities.len());
    let mut eligible_count = 0;
    let mut unassigned = Vec::new();
    for entity in entities {
        validate_identifier(&entity.entity_id)?;
        if !entity_ids.insert(entity.entity_id.as_str()) {
            return Err(CoreError::DuplicateEntityId);
        }
        validate_entity_references(entity)?;
        let Some(tile) = build_tile(entity, states, connection)? else {
            continue;
        };
        eligible_count += 1;
        if eligible_count > MAX_ENTITY_COUNT {
            return Err(CoreError::TooManyEligibleEntities);
        }

        if let Some(area_id) = effective_area(entity, devices) {
            let room_id = RoomId::Area(area_id.to_owned());
            if !groups.contains_key(&room_id) {
                groups.insert(room_id.clone(), Vec::new());
                room_names.insert(area_id.to_owned(), area_id.to_owned());
                base_rooms.push(room_id.clone());
            }
            groups.entry(room_id).or_default().push(tile);
        } else {
            unassigned.push(tile);
        }
    }
    Ok(EntityGroups {
        base_rooms,
        room_names,
        groups,
        unassigned,
    })
}

fn validate_entity_references(entity: &EntityRegistryEntry) -> Result<(), CoreError> {
    if let Some(area_id) = &entity.area_id {
        validate_identifier(area_id)?;
    }
    if let Some(device_id) = &entity.device_id {
        validate_identifier(device_id)?;
    }
    if let Some(name) = &entity.name {
        validate_name(name)?;
    }
    if let Some(icon) = &entity.icon {
        validate_name(icon)?;
    }
    Ok(())
}

fn build_tile(
    entity: &EntityRegistryEntry,
    states: &StateIndex<'_>,
    connection: ConnectionStatus,
) -> Result<Option<EntityTile>, CoreError> {
    let Some(domain) = parse_entity_domain(&entity.entity_id)? else {
        return Ok(None);
    };
    if entity.disabled_by.is_some()
        || entity.hidden_by.is_some()
        || entity.entity_category.as_deref() == Some("diagnostic")
    {
        return Ok(None);
    }

    let state = states
        .get(entity.entity_id.as_str())
        .map(|state| (*state).clone());
    let fallback_name = entity.name.as_deref().unwrap_or(&entity.entity_id);
    let name = match state.as_ref() {
        Some(state) => display_name(state, fallback_name)?,
        None => fallback_name.to_owned(),
    };
    let icon = state
        .as_ref()
        .and_then(icon_from)
        .or_else(|| entity.icon.clone());
    let command_enabled =
        connection.is_ready() && domain.supports_action() && state_is_available(state.as_ref());
    Ok(Some(EntityTile {
        entity_id: entity.entity_id.clone(),
        name,
        icon,
        domain,
        state,
        is_stale: !connection.is_ready(),
        command_enabled,
        action_feedback: None,
    }))
}

fn effective_area<'a>(
    entity: &'a EntityRegistryEntry,
    devices: &'a DeviceIndex,
) -> Option<&'a str> {
    entity.area_id.as_deref().or_else(|| {
        entity
            .device_id
            .as_deref()
            .and_then(|device_id| resolve_device_area(device_id, devices))
    })
}

fn assemble_rooms(
    base_rooms: &[RoomId],
    room_names: &HashMap<String, String>,
    mut groups: TileGroups,
    mut unassigned: Vec<EntityTile>,
    order: &DashboardOrder,
) -> Vec<Room> {
    let room_order = ordered_rooms(base_rooms, order.area_ids.as_deref());
    let mut rooms = Vec::with_capacity(room_order.len() + usize::from(!unassigned.is_empty()));
    for room_id in room_order {
        let mut room_entities = groups.remove(&room_id).unwrap_or_default();
        order_entities(
            &mut room_entities,
            order.entity_ids_by_room.get(&room_id).map(Vec::as_slice),
        );
        let name = match &room_id {
            RoomId::Area(area_id) => room_names
                .get(area_id)
                .cloned()
                .unwrap_or_else(|| area_id.clone()),
            RoomId::Unassigned => "Unassigned".to_owned(),
        };
        rooms.push(Room {
            id: room_id,
            name,
            entities: room_entities,
        });
    }
    if !unassigned.is_empty() {
        order_entities(
            &mut unassigned,
            order
                .entity_ids_by_room
                .get(&RoomId::Unassigned)
                .map(Vec::as_slice),
        );
        rooms.push(Room {
            id: RoomId::Unassigned,
            name: "Unassigned".to_owned(),
            entities: unassigned,
        });
    }
    rooms
}

const fn validate_snapshot_sizes(
    areas: &[AreaRegistryEntry],
    devices: &[DeviceRegistryEntry],
    entities: &[EntityRegistryEntry],
    states: &[HaEntityState],
) -> Result<(), CoreError> {
    if areas.len() > MAX_AREA_COUNT {
        return Err(CoreError::TooManyAreas);
    }
    if devices.len() > MAX_DEVICE_COUNT {
        return Err(CoreError::TooManyDevices);
    }
    if entities.len() > MAX_ENTITY_REGISTRY_COUNT {
        return Err(CoreError::TooManyEntityRegistryEntries);
    }
    if states.len() > MAX_STATE_COUNT {
        return Err(CoreError::TooManyStates);
    }
    Ok(())
}

fn validate_order(order: &DashboardOrder) -> Result<(), CoreError> {
    if let Some(area_ids) = &order.area_ids {
        if area_ids.len() > MAX_AREA_COUNT {
            return Err(CoreError::TooManyAreas);
        }
        for area_id in area_ids {
            validate_identifier(area_id)?;
        }
    }
    if order.entity_ids_by_room.len() > MAX_AREA_COUNT + 1 {
        return Err(CoreError::TooManyEligibleEntities);
    }
    for (room_id, entity_ids) in &order.entity_ids_by_room {
        if let RoomId::Area(area_id) = room_id {
            validate_identifier(area_id)?;
        }
        if entity_ids.len() > MAX_ENTITY_COUNT {
            return Err(CoreError::TooManyEligibleEntities);
        }
        for entity_id in entity_ids {
            validate_identifier(entity_id)?;
        }
    }
    Ok(())
}

fn validate_state(state: &HaEntityState) -> Result<(), CoreError> {
    validate_identifier(&state.entity_id)?;
    if state.state.chars().count() > MAX_STATE_CHARS {
        return Err(CoreError::StateTooLong);
    }
    if state
        .last_updated
        .as_ref()
        .is_some_and(|last_updated| last_updated.chars().count() > MAX_LAST_UPDATED_CHARS)
    {
        return Err(CoreError::StateTooLong);
    }
    if state.attributes.len() > MAX_ATTRIBUTE_COUNT {
        return Err(CoreError::TooManyAttributes);
    }
    for key in state.attributes.keys() {
        if key.chars().count() > MAX_ATTRIBUTE_KEY_CHARS {
            return Err(CoreError::AttributeKeyTooLong);
        }
    }
    if let Some(icon) = state.attributes.get("icon").and_then(Value::as_str) {
        validate_name(icon)?;
    }
    Ok(())
}

fn parse_entity_domain(entity_id: &str) -> Result<Option<EntityDomain>, CoreError> {
    let Some((domain, object_id)) = entity_id.split_once('.') else {
        return Err(CoreError::InvalidEntityId);
    };
    if domain.is_empty() || object_id.is_empty() || object_id.contains('.') {
        return Err(CoreError::InvalidEntityId);
    }
    Ok(EntityDomain::from_entity_id(entity_id))
}

fn validate_identifier(value: &str) -> Result<(), CoreError> {
    if value.is_empty() || value.chars().count() > MAX_IDENTIFIER_CHARS {
        return Err(CoreError::IdentifierTooLong);
    }
    Ok(())
}

fn validate_name(value: &str) -> Result<(), CoreError> {
    if value.chars().count() > MAX_NAME_CHARS {
        return Err(CoreError::NameTooLong);
    }
    Ok(())
}

fn validate_reason(reason: &str) -> Result<(), CoreError> {
    if reason.chars().count() > MAX_ACTION_REASON_CHARS {
        return Err(CoreError::ActionReasonTooLong);
    }
    Ok(())
}

fn display_name(state: &HaEntityState, fallback: &str) -> Result<String, CoreError> {
    let name = state
        .attributes
        .get("friendly_name")
        .and_then(Value::as_str)
        .unwrap_or(fallback);
    validate_name(name)?;
    Ok(name.to_owned())
}

fn icon_from(state: &HaEntityState) -> Option<String> {
    state
        .attributes
        .get("icon")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn state_is_available(state: Option<&HaEntityState>) -> bool {
    state.is_some_and(|state| state.state != "unavailable" && state.state != "unknown")
}

fn resolve_device_area<'a>(
    device_id: &str,
    devices: &'a HashMap<String, (Option<String>, Option<String>)>,
) -> Option<&'a str> {
    let mut current_id = Some(device_id);
    let mut seen = HashSet::new();
    while let Some(id) = current_id {
        if !seen.insert(id) {
            return None;
        }
        let (area_id, parent_id) = devices.get(id)?;
        if let Some(area_id) = area_id.as_deref() {
            return Some(area_id);
        }
        current_id = parent_id.as_deref();
    }
    None
}

fn ordered_rooms(base_rooms: &[RoomId], explicit_area_ids: Option<&[String]>) -> Vec<RoomId> {
    let Some(explicit_area_ids) = explicit_area_ids else {
        return base_rooms.to_vec();
    };
    let available: HashSet<&str> = base_rooms
        .iter()
        .filter_map(|room_id| match room_id {
            RoomId::Area(area_id) => Some(area_id.as_str()),
            RoomId::Unassigned => None,
        })
        .collect();
    let mut ordered = Vec::with_capacity(base_rooms.len());
    let mut seen = HashSet::with_capacity(base_rooms.len());
    for area_id in explicit_area_ids {
        if available.contains(area_id.as_str()) && seen.insert(area_id.as_str()) {
            ordered.push(RoomId::Area(area_id.clone()));
        }
    }
    for room_id in base_rooms {
        if let RoomId::Area(area_id) = room_id
            && seen.insert(area_id.as_str())
        {
            ordered.push(room_id.clone());
        }
    }
    ordered
}

fn order_entities(entities: &mut Vec<EntityTile>, explicit_order: Option<&[String]>) {
    let Some(explicit_order) = explicit_order else {
        return;
    };
    let mut slots: Vec<Option<EntityTile>> = entities.drain(..).map(Some).collect();
    let indices: HashMap<String, usize> = slots
        .iter()
        .enumerate()
        .filter_map(|(index, tile)| tile.as_ref().map(|tile| (tile.entity_id.clone(), index)))
        .collect();
    let mut ordered = Vec::with_capacity(slots.len());
    for entity_id in explicit_order {
        if let Some(index) = indices.get(entity_id)
            && let Some(tile) = slots[*index].take()
        {
            ordered.push(tile);
        }
    }
    ordered.extend(slots.into_iter().flatten());
    *entities = ordered;
}
