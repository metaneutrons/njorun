# Entity controls and support boundaries

This page describes the shared entity control catalog in
[`entity_catalog.rs`](../crates/app-core/src/entity_catalog.rs). The catalog builds typed
controls from the current entity snapshot and validates the same capability, state and
input constraints again before producing a Home Assistant service call. It does not
provide a raw service picker or a free-form JSON editor. Home Assistant remains the place
to configure integrations, devices, automations and reusable scripts.

Actuator operations require an available, current and command-enabled entity. Sensor and
binary-sensor history remains queryable while the current value is unknown or unavailable,
provided the panel is connected.
Feature flags and option lists are checked where the entity model exposes them. For legacy
entities without an explicit feature mask, the catalog uses documented attributes only
for the specific compatibility paths listed below. HA can still reject a call if the
integration has changed or its device is unreachable.

## Catalog actions

| Entity domain | Catalog operations | Capability and input limits |
| --- | --- | --- |
| `light` | Turn on/off; brightness; color temperature; RGB, HS, XY, RGBW, RGBWW and white; effects; transition; flash | Plain power buttons follow known on/off state; the brightness and color controls remain available alongside them. Color controls follow `supported_color_modes`; effect uses `effect_list`; effect, transition and flash require their HA feature bits. Temperature uses the entity's Kelvin limits. Numeric channels and timing are bounded. |
| `fan` | Turn on/off; percentage; preset; direction; oscillation | Plain power buttons follow known on/off state; explicit masks require `TURN_ON`/`TURN_OFF` bits (32/16). Percentage and preset use feature bits when `supported_features` is present; old snapshots without a mask may use the corresponding percentage/preset attributes. Direction and oscillation require their feature bits. Presets are restricted to `preset_modes`. |
| `lock` | Lock, unlock and open latch | Lock/unlock are disabled during `locking`/`unlocking`; settled locked/unlocked states suppress the redundant operation. When `code_format` is exposed, codes use a password-masked string field so leading zeroes survive. Numeric format accepts digits only. Open requires the lock open feature and uses the same code handling. |
| `alarm_control_panel` | Supported arm modes, disarm and trigger | Arm modes and trigger follow the alarm feature mask. `code_arm_required` requires a code when arming; a declared code format requires one for disarm. Codes stay masked strings; numeric format accepts digits only. Extra form fields are rejected. |
| `number`, `input_number` | Set value with a slider or exact numeric entry | Both paths enforce min, max and step. Exact entry preserves the decimal text in action confirmation. |
| `select`, `input_select` | Select option | Only current `options` are accepted. |
| `switch`, `input_boolean` | Turn on/off | Plain power buttons follow known on/off state and use the entity's own service domain. |
| `scene` | Activate | Calls `scene.turn_on`. |
| `button`, `input_button` | Press | Calls the entity domain's `press` action. |
| `script` | Run; stop while the state is `on`; typed declared parameters | The host injects a bounded `njorun_script_fields` schema from HA's script service registry (at most 16 fields). Supported selectors are text, number, boolean, select and entity. Parameters are remapped to the script's declared variable names. Unknown selector types, overflowed required fields and undeclared input keys are rejected. If schema metadata is missing or fields were omitted by the transport bound, Run is disabled and the control explains that HA needs a wrapper script. |
| `automation` | Enable, disable, run | Run has an explicit `skip_condition` boolean, defaulting to false. |
| `climate` | HVAC mode, target or range temperature, humidity, fan mode, preset, swing and horizontal swing, on/off | Every operation follows the matching climate feature bit and the exposed mode lists, temperature limits and steps. A range requires low < high. |
| `cover` | Open, close, stop, position, tilt and tilt open/close/stop | Each operation follows its cover feature bit. Position and tilt are limited to 0–100%. |
| `humidifier` | On/off; target humidity when exposed; mode | On/off are available through the humidifier toggle model. Target humidity is offered only when the entity exposes a numeric target; values are integer and bounded by min/max. Mode requires the `MODES` bit and a non-empty `available_modes` list. |
| `water_heater` | On/off, target temperature, operation mode and away mode | Each operation follows the water-heater feature mask. Temperature uses exposed limits/step; modes use `operation_list`; away mode calls `set_away_mode` with a boolean. |
| `valve` | Open, close, stop and position | Each operation follows its valve feature bit; position is limited to 0–100%. |
| `vacuum` | Start, pause, stop, return home, locate, spot clean, clean mapped HA areas and fan speed | Operations follow the vacuum feature mask; fan speed is restricted to `fan_speed_list`. Area cleaning accepts bounded HA area IDs and uses the mappings configured on the vacuum entity in HA. Integration-specific `send_command` payloads are not exposed because the command schema is provider-defined. |
| `lawn_mower` | Start, pause, stop and dock | Each operation follows the lawn mower feature bit (`DOCK`=4, `STOP`=8). |
| `siren` | Turn on/off, with optional tone, volume and duration | Each optional field is exposed and validated only when its feature is present; tone is limited to `available_tones`. |
| `remote` | Turn on/off/toggle, start a listed activity, send, learn and delete commands | Learn, delete and activity operations follow their feature bits. Command, device, repeat count, delay, hold duration, command type, alternative flag and timeout use typed fields. Provider-specific command names are not guessed. |
| `camera` | Turn on/off, motion detection, snapshot, record and play stream | On/off and stream use camera feature bits. Motion controls require the `motion_detection` attribute. Snapshot and record require a destination filename; recording duration/lookback are bounded. Play stream requires a `media_player.*` entity. |
| `group` | Turn on/off/toggle | Offered only when every listed member is in the catalog's targetable on/off domain set. Calls `homeassistant.turn_on/off/toggle` against the group entity. |
| `text`, `input_text` | Set text | Text length is bounded by the entity's maximum where exposed, otherwise by the catalog's 255-character limit. |
| `date`, `time`, `datetime`, `input_datetime` | Set date, time or date/time | Date/time syntax is checked; input-datetime fields follow `has_date` and `has_time`. Datetime values accept ISO date/time with an optional UTC offset. |
| `counter` | Increment, decrement, reset, set value | Set value has slider and exact-number paths, bounded by exposed min/max and step. |
| `timer` | Start, pause, change, cancel and finish | Pause/change appear while active; cancel/finish appear while active or paused. Duration accepts seconds or `HH:MM:SS`; change permits signed duration. |
| `update` | Install, install a selected version, install with backup, skip, clear skipped, release notes | Install controls appear only while the update is available and when the matching update feature is set. Skip/clear use HA's generic actions. Release notes use the `update/release_notes` WebSocket command and its `RELEASE_NOTES` feature bit; HA requires an administrator connection. |
| `weather` | Get daily, hourly or twice-daily forecasts | Query types are restricted to the forecast feature bits. The result requires HA's response-producing service call path. |
| `calendar` | Get events and create timed or all-day events | Create requires the calendar create feature. The form accepts either a start/end datetime pair or a start/end date pair, never a mix. The generic HA action list currently documents `calendar.get_events` and `calendar.create_event`; there is no generic `calendar.delete_event` or `calendar.update_event` action in this catalog. |
| `todo` | Get items, add, update, move, remove and remove completed items | Mutations follow the todo feature bits. Move uses HA's `todo/item/move` WebSocket command with the item UID and optional previous UID. Due date, due datetime and description fields follow their own bits; date and datetime are mutually exclusive. Updates require at least one changed field. |
| `notify` | Send a message, optionally with title | Title is included only when the notification entity advertises the title feature. |
| `tts` | Speak a message on a media player | Requires a `media_player.*` target and non-empty message. |
| `schedule` | Get schedule | Uses the response-producing `schedule.get_schedule` action. The catalog does not invent schedule mutation operations. |
| `image_processing` | Scan | Calls the documented entity-targeted scan action without integration-specific data. |

## Media player

The media detail view supports play/pause/stop, previous/next, power, volume and mute,
seeking with elapsed/duration display, artwork, source, sound mode, shuffle and repeat.
Media browsing and search display bounded results; a browsable, playable album has one
row with a separate play button. Play-media supports provider content IDs, enqueue modes
and announcements; enqueue and announcement parameters are mutually exclusive under
HA's schema. Clear queue, joining players and unjoining are also supported. Every action
requires its matching feature bit. Playback controls remain available while viewing a
library result. Source, sound mode and repeat use expandable choices.

Position advances locally only while the authoritative state is playing, based on HA's
position timestamp and bounded by duration. A submitted action is pending until HA
acknowledges it. Where HA exposes a reliable target state or attribute, confirmation also
waits for that observed value; transient/provider-specific operations report receipt only. Browsing,
search and other response-producing services run asynchronously so state subscriptions
and heartbeats continue. Timeouts are not retried or replayed after reconnection.

Artwork uses HA's authenticated same-origin media-player proxy. Media playback runs on
the selected HA player; this implementation does not turn the wall tablet into an audio
output device. HA exposes no universal playback-queue enumeration API, so the panel
provides supported queue actions rather than inventing a queue list.

## Response-producing providers

| Domain | Panel interaction | Boundary |
| --- | --- | --- |
| `conversation` | Send text, optional language and conversation ID; show response | Targets the selected agent; agent setup remains in HA. |
| `assist_satellite` | Announce, start a conversation, ask a question | Capability-gated; audio is produced/captured by the selected HA satellite. |
| `ai_task` | Generate data, generate an image, evaluate a question | Capability-gated typed forms; optional media attachment; bounded response display. Generated-image responses are presented as returned resource data, not as a local image gallery. |
| `stt`, `wake_word` | Provider state and explanation | Requires an HA Assist pipeline and audio source; no microphone pipeline is implemented on the panel. |
| `infrared`, `radio_frequency` | Provider state and explanation | Device-specific commands run through configured HA remotes or scripts. There is no generic public send service on these provider entity domains. |

Sensor and binary-sensor details include a last-24-hours history query with at most 200
recent samples shown as values and timestamps. This is a history list, not a graph.
Calendar events, forecasts, to-do items and schedule responses use the same bounded
result view.

## State-only and provider-managed entities

The catalog returns no service controls for sensor, binary sensor, device tracker, air
quality, event, image, geolocation, person, zone, sun and other read-only entities. Their
state and retained attributes are presented by the shared entity-detail model. Weather
also has a forecast query listed above. A raw state or an entity's presence in the registry
is not treated as an action capability.

Speech-to-text and wake-word entities are Assist-pipeline providers configured in HA.
Infrared and radio-frequency entities are provider hardware; command definitions belong
to HA integrations and remote entities. The provider explanation view directs the user to
those HA-owned paths. AI task, conversation and Assist satellite operations are handled by
the separate typed provider module; their schemas and limits are described there rather
than guessed in this catalog.

## Media and camera boundary

This catalog sends `camera.snapshot`, `camera.record` and `camera.play_stream` service
calls; it does not put video bytes in entity state. A snapshot is written at a path on the
HA host. Reading and displaying that file requires the separate image transport. Live
playback depends on camera stream support, HA's stream integration and the selected media
player. A snapshot preview or an entity-picture poll is not live video playback.

Camera and image entity previews are periodically refreshed JPEG/PNG images with loading,
error and age labels. A failed fetch clears the previous image so it cannot appear current.
The simulator and ESP32-P4 share the control descriptors, retained attribute inventory,
entity selection and script field bounds. At most 512 selected entities are retained;
favorites and HA-configured order take precedence, and the UI reports omissions.

## Known limits

- Integration-defined command payloads, provider-specific service fields and device setup
  remain in Home Assistant. Unsupported required script selectors disable script start.
- Calendar update/delete operations are not offered without generic HA actions for those
  operations. A particular integration may expose its own differently named actions, but
  the catalog does not guess them.
- `vacuum.send_command` is omitted because its command and parameter schema is
  integration-specific. The legacy vacuum `TURN_ON`/`TURN_OFF` flags are deprecated and
  do nothing for `StateVacuumEntity`; this catalog offers start/stop operations instead.
- Catalog tests validate schemas, capability gating and service mappings with fixtures;
  they do not prove that an installed integration or physical device accepts a request.

## Home Assistant references

- [Available actions](https://www.home-assistant.io/actions/) — the generic action list,
  including calendar, remote, climate and helper actions.
- [Camera integration actions](https://www.home-assistant.io/integrations/camera/) and
  [camera entity contract](https://developers.home-assistant.io/docs/core/entity/camera/).
- [Remote action reference](https://www.home-assistant.io/actions/remote.send_command/) and
  [remote entity feature contract](https://developers.home-assistant.io/docs/core/entity/remote/).
- [To-do entity contract](https://developers.home-assistant.io/docs/core/entity/todo/) and
  [humidifier entity contract](https://developers.home-assistant.io/docs/core/entity/humidifier/).
- [Lock open action](https://www.home-assistant.io/actions/lock.open) and
  [vacuum entity contract](https://developers.home-assistant.io/docs/core/entity/vacuum/).
- [Update actions](https://www.home-assistant.io/actions/update.skip/) and
  [update entity contract](https://developers.home-assistant.io/docs/core/entity/update/).
- [Weather forecast actions](https://www.home-assistant.io/integrations/weather/),
  [calendar actions](https://www.home-assistant.io/integrations/calendar/), and
  [schedule actions](https://www.home-assistant.io/integrations/schedule/).
