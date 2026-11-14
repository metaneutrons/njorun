# Panel UX overhaul: palettes, accessible interactions and room identity

## Baseline

The simulator and tablet share the native Slint dashboard. The dashboard currently has one
paired color scheme, with `high_contrast` represented as a separate legacy scheme. A short tap on
an entity tile activates its entity; a small information target opens details. Room shortcuts use
the same house icon. The commissioning screens have a separate palette path. There is no
reduced-motion preference, and the native preview set does not qualify the new light and dark
palette choices independently.

## Authorized outcome

This plan records the authorized full panel UX overhaul. The connected dashboard and first-run
commissioning use one palette system and the same accessibility preferences. Home Assistant can
choose a light palette and a dark palette independently from `njorun`, `graphite`, `warm` and
`porcelain`; it can also enable increased contrast and reduced motion. The existing `color_scheme`
setting remains on the wire and in storage while older settings are migrated.

Entity interaction is explicit. Tapping the main area of a control, status or measurement opens
its details. A scene activates from its main area. A separate 56 × 56 target activates an
available control or scene; for status and measurement tiles it is a chevron that opens details.
Disabled or stale controls cannot enqueue an action. Room shortcuts show a small curated icon,
with a house as the fallback. User-defined room and entity display names remain local settings.

This UX work adds no new usage tracking, analytics or external reporting. Existing product
telemetry and logging stay governed by the existing product and protocol plans. Local deterministic
fixtures and screenshots qualify the new interface behavior.

## Migration

- When `light_color_scheme` or `dark_color_scheme` is absent, use the legacy `color_scheme` for
  that mode. New records default both fields to `njorun`.
- Map the legacy `high_contrast` scheme to the `njorun` palette with increased contrast enabled.
  Keep the legacy field available during migration, but coordinate the integration and firmware
  upgrade before sending the expanded settings object: a strict old peer rejects unknown fields.
- Explicit new mode-specific palettes and `increased_contrast` take precedence over the legacy
  value. Reject unknown palette names through the existing settings validation path.
- Keep `reduced_motion` false by default. Older records and messages without the field therefore
  retain the current motion behavior.
- Validate any persisted room icons against the bundled icon allowlist. Missing mappings use a
  name-based suggestion or the house fallback; invalid configured values are rejected. No remote
  icon assets are loaded.

## Decisions

- **P1 Palette selection is per mode.** Home Assistant stores the light and dark choices
  separately. The panel resolves the effective palette from the active mode and applies the same
  tokens to the dashboard, detail sheets, setup, status surfaces and controls.
- **P2 Contrast is independent of palette.** Increased contrast strengthens text, borders and
  focus/selection indicators without silently changing the selected palette.
- **P3 Reduced motion is a system-wide preference.** Page, selection and feedback transitions
  become immediate or short and nonessential animation is suppressed. State and touch feedback
  remain visible.
- **P4 Tile action targets are visible and reachable.** The right-side action target is at least
  56 × 56 UI pixels. Main-body and action-target callbacks are qualified separately so a details
  tap cannot issue a service call.
- **P5 Read-only semantics follow the domain.** Status and measurement cards expose details from
  either their body or trailing chevron. Scenes use their body and trailing action target to run.
  Controls use the body for details and trailing target for their action.
- **P6 Room identity uses bundled icons.** Area IDs and names can suggest a built-in room icon;
  an explicit valid choice takes priority. Unknown rooms remain understandable with the house
  fallback.
- **P7 Qualification is reproducible and bounded.** The preview example writes each frame as soon
  as it is rendered. The full state/display baseline is kept separate from the smaller palette,
  contrast and reduced-motion combination matrix to avoid retaining a multi-gigabyte image set.

## Slices

1. **Settings and migration.** Add per-mode palette, contrast and motion settings, retain the
   legacy field, validate values, and pin old/new settings fixtures on the Rust and Home Assistant
   sides.
2. **Shared palette system.** Add the four palette families and contrast tokens, bind the active
   preferences to the main and commissioning windows, and use the shared tokens across surfaces.
3. **Dashboard identity.** Bind curated room icons and locally configured room/entity names with
   stable-ID lookup and safe fallbacks.
4. **Tile semantics.** Split body and trailing actions, supply `kind` and `action-enabled` to each
   tile, and qualify controls, scenes, statuses, measurements, unavailable states and stale states
   through the real pointer path.
5. **Native visual qualification.** Render all existing dashboard and setup states on each
   supported display and orientation. Render the four palette families with both modes, both
   contrast settings and both motion settings on representative portrait and landscape displays.
   Include 75%, 150% and 200% scale fixtures and existing domain detail fixtures, writing PNGs
   incrementally.
6. **macOS simulator qualification.** Run the native simulator and its real software-renderer
   qualification on the requested macOS environment. Physical-panel review is a separate future
   release gate, not a dependency of this authorized overhaul.

## Measurable qualification criteria

- The shared settings fixtures cover a legacy-only message, legacy `high_contrast`, explicit
  light/dark palettes, explicit contrast and motion values, defaults, and invalid choices.
- Native pointer qualification proves that a control body opens details without enqueuing a
  command, its trailing target emits exactly one activation, a scene body emits exactly one
  activation, and status/measurement body and chevron open details without a command.
- Unavailable and stale controls expose details where applicable but never enqueue actions. A
  disabled action target stays disabled; queue-full and disconnected paths remain bounded.
- Preview output includes every existing dashboard state at 75%, 100%, 150% and 200% where that
  state has a scale fixture; both orientations and all four supported physical display fixtures;
  all commissioning stages; all palette/mode/contrast/motion combinations on representative
  portrait and landscape fixtures; and available light, sensor, binary-sensor and media-player
  detail fixtures.
- The ordinary dashboard reference is rendered at 100% brightness. The native settings
  interaction separately verifies that 40% brightness produces the expected 60% dim overlay.
- The preview executable retains at most the frame currently being encoded, plus renderer working
  storage. It does not accumulate the palette-by-state matrix in memory.
- The 56 × 56 target remains usable at 75% scale, and the 150%/200% fixtures show responsive
  reflow without clipped actions or unreachable details.
- The requested macOS native simulator run passes the same software-renderer and pointer
  qualification as the host test suite. Physical-panel touch, contrast and ambient-light checks
  remain a separate future gate.

## Layout and readability metrics

- Bundle Inter regular, medium and semibold weights so the dashboard and commissioning screens
  use the same measured font metrics on each host.
- Keep the standard header at 76 px. On short screens it contracts to 56 px below 440 logical px
  and is omitted below 360 px; portrait navigation tabs also disappear below 440 px.
- Use 20 px section labels, 26 px room names (20 px in compact room shortcuts), 24 px control
  names, 20 px measurement names and 32 px measurement values. Entity names remain limited to two
  lines in cards; details can wrap and scroll instead of truncating the configured full name.
- Center the 38 px room icon in its 52 px shortcut box. Keep explicit entity actions at 56 × 56 px,
  the details title at 28 px, and its close target at 60 × 60 px.
- Inset card actions 16 px from the right edge, matching the leading icon inset, and reserve
  12 px between text and action. Center the action vertically within the card.
- Render the principal dimmer value at 44 px. At 150% and 200%, reflow the grid and scroll long
  detail content so the brightness control remains reachable; test the actual slider action after
  scrolling at 200%.
- The final bounded preview run wrote 477 PNGs incrementally: 128 commissioning
  baseline, 64 commissioning palette, 8 commissioning error/keyboard, 152 dashboard baseline,
  64 dashboard palette, 2 automatic day/night, 48 domain details, 3 stale/unavailable/rejected
  detail states, and 8 settings/reset overlay frames. The emitted count was confirmed.

A follow-up review of the palette comparison found that trailing action buttons had only a 4 px
right inset despite the 16 px leading icon inset. Corrected the right inset to 16 px, retained the
56 × 56 px target and vertical centering, and reserved a 12 px text gap. The native interaction
qualification passed after the correction; the live probe now targets the new button center.

## Verified and not verified

The Rust simulator and real Slint software-renderer tests provide deterministic host coverage. The
preview baseline renders all nineteen existing dashboard state fixtures on four display fixtures,
in both orientations and theme modes. Palette, contrast and motion combinations cover all four
  families on D1001 portrait and landscape for the dashboard and setup screens. Setup previews cover
  all sixteen stages, password masking, errors and the symbol keyboard. Detail frames
use the presentation model for lights, switches, fans, numbers, selects, locks, alarms, media
players, cameras, automations, scripts and sensors, plus stale, unavailable and rejected light
states. The camera fixture shows the no-picture/loading affordance; it has no captured camera image.
Simulator camera refresh stays disabled because the simulator has no camera-fetch command route;
the P4 path may enable it only for a fresh, available camera.
Automatic day/night frames are produced after actual `Daylight` runtime events and verify each
independent palette while preserving contrast and reduced motion. A separate pointer test scrolls
the 200% detail sheet and commits a brightness change. Settings previews include reset confirmation.

The requested macOS simulator run belongs to this qualification. Physical panel runs are a
separate future gate; software rendering cannot qualify touch ergonomics, ambient-light readability
or reduced-motion perception on device.

## Qualification results — 2026-11-14

- Rust: 318 tests passed with all workspace features enabled; Clippy passed with warnings denied.
  The pinned ESP32-P4 build also passed. Formatting and whitespace checks passed.
- Home Assistant: 257 tests and 241 subtests passed; Ruff and translation consistency checks passed.
  The shared settings fixtures cover omitted, explicit and invalid accessibility preferences,
  legacy palette migration and independent light/dark choices.
- Rendering: reviewed all 477 frames using 43 contact sheets and full-size inspection of details,
  feedback, keyboard and settings cases. The matrix is bounded as described above, not every
  possible state × palette × scale combination. Review found and resolved feedback-text overflow
  and keyboard clipping on the short landscape display.
- Native macOS interaction: verified explicit power targets, card-body details, brightness changes,
  paging and scene-request feedback. Automated native pointer coverage also verifies disabled
  controls and a brightness change after scrolling the detail sheet at 200% scale.
- Contrast: computed from the actual palette tokens for every family, mode and contrast setting.
  Tested text combinations meet 4.5:1; tested essential icon combinations meet 3:1. These are
  software color calculations, not measurements of a physical panel.
- Disposable real Home Assistant: verified the new selects, switches, profile naming, room icons
  and ordering workflows. A targeted live simulator check switched a template light from off to
  on and observed the persisted state/settings acknowledgement. Its sanitized evidence is
  `target/qualification/live-ui.json` and `target/qualification/live-ui.png`.
  The aggregate vertical-slice artifact still records a failed run at the earlier UI probe, which
  used the old center-tap geometry. The probe was corrected and the targeted check passed; the
  entire aggregate scenario was not rerun after that correction. This is not a clean full
  end-to-end scenario result.

The local visual evidence is in `target/ux-overhaul-final/index.html`; the four-family comparison
is `target/ux-overhaul-final/palette-overview.jpg`. These generated artifacts are intentionally
outside version control. The screenshot source is the shared native Slint UI, not a mockup.

## Hardware gate (future)

Before a hardware release, record the same interaction and rendering criteria on each available
D1001 and Guition panel. Missing hardware remains explicitly pending; it does not block the
requested macOS simulator overhaul.

## Risks

- A mode-specific palette migration can look correct in stored settings but select the wrong
  palette after automatic day/night changes; tests must exercise both explicit modes and the
  automatic mode's resolved day/night states.
- Adding a nested action target changes touch hit-testing and hold handling. Pointer qualification
  must check body taps, trailing taps, long presses, movement cancellation and list reordering.
- The curated palettes and contrast settings need real panel review for color reproduction and
  glare. Software screenshots alone cannot qualify those properties.
- Very large scale factors reduce the number of columns and visible rows. The paged-grid and
  detail-sheet tests must remain active at 150% and 200%.
