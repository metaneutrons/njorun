# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Home Assistant setup and native profile/device configuration flows."""

from __future__ import annotations

import json
import re
from typing import Any

import probatio

from homeassistant.config_entries import (
    ConfigEntry,
    ConfigFlow,
    ConfigFlowResult,
    OptionsFlow,
)
from homeassistant.core import callback
from homeassistant.helpers.selector import (
    BooleanSelector,
    NumberSelector,
    NumberSelectorConfig,
    NumberSelectorMode,
    SelectOptionDict,
    SelectSelector,
    SelectSelectorConfig,
    SelectSelectorMode,
    TextSelector,
    TextSelectorConfig,
    TextSelectorType,
)

from .const import DOMAIN
from .model import CHOICES, DEFAULTS, RANGES
from .runtime import NjorunRuntime, RuntimeError

_ADVANCED_KEYS = ("favorites", "room_order", "entity_order", "excluded_entities")
_SIMPLE_KEYS = tuple(key for key in DEFAULTS if key not in _ADVANCED_KEYS)
_SETTING_LABELS = {
    "orientation": {
        "portrait": "Portrait",
        "landscape": "Landscape",
        "portrait_flipped": "Portrait, upside down",
        "landscape_flipped": "Landscape, upside down",
    },
    "theme": {"automatic": "Automatic", "light": "Light", "dark": "Dark"},
    "language": {"en": "English", "de": "German"},
    "screensaver": {
        "display_off": "Turn display off",
        "clock": "Clock",
        "disabled": "Disabled",
    },
    "home_view": {"rooms": "Rooms", "favorites": "Favorites", "mixed": "Mixed"},
    "details": {"adaptive": "Adaptive", "fullscreen": "Full screen"},
    "update_policy": {"automatic": "Automatic", "manual": "Manual"},
    "update_channel": {"stable": "Stable", "beta": "Beta"},
}
_OPERATION_LABELS = {
    "approve_tablet": "Approve a waiting tablet",
    "add_profile": "Add profile",
    "edit_profile": "Edit profile",
    "assign": "Assign tablet to profile",
    "set_overrides": "Set tablet overrides",
    "clear_overrides": "Clear tablet overrides",
    "delete_profile": "Delete profile",
    "remove_tablet": "Remove tablet",
}


class NjorunConfigFlow(ConfigFlow, domain=DOMAIN):
    """Create the single integration entry for this Home Assistant instance."""

    VERSION = 1

    async def async_step_user(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Install Njörun; profile and tablet setup follow in the entry UI."""
        await self.async_set_unique_id(DOMAIN)
        self._abort_if_unique_id_configured()
        if user_input is not None:
            return self.async_create_entry(
                title="Njörun",
                data={"initial_profile_name": user_input["profile_name"]},
            )
        return self.async_show_form(
            step_id="user",
            data_schema=probatio.Schema(
                {
                    probatio.Required("profile_name", default="Home"): probatio.All(
                        str, probatio.NonEmpty()
                    )
                }
            ),
        )

    @staticmethod
    @callback
    def async_get_options_flow(config_entry: ConfigEntry) -> NjorunOptionsFlow:
        """Return the native per-entry profile and tablet options flow."""
        return NjorunOptionsFlow()


class NjorunOptionsFlow(OptionsFlow):
    """Manage shared profiles, assignments and per-tablet overrides."""

    async def async_step_init(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Offer one bounded configuration operation per flow."""
        if user_input is not None:
            self._operation = user_input["operation"]
            return await self.async_step_operation()
        return self.async_show_form(
            step_id="init",
            data_schema=probatio.Schema(
                {
                    probatio.Required("operation"): SelectSelector(
                        SelectSelectorConfig(
                            options=list(_OPERATION_LABELS),
                            mode=SelectSelectorMode.DROPDOWN,
                            translation_key="operation",
                        )
                    )
                }
            ),
        )

    async def async_step_operation(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Route the selected operation to its native form."""
        operation = getattr(self, "_operation", None)
        if operation not in _OPERATION_LABELS:
            return self.async_abort(reason="unknown")
        return await getattr(self, f"async_step_{operation}")()

    async def async_step_add_profile(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Create a profile with native choices and bounded values."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        schema = _settings_schema(DEFAULTS, required=True, include_name=True)
        if user_input is None:
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(step_id="add_profile", data_schema=schema)
        try:
            profile_name = user_input.pop("name")
            settings = _collect_settings(user_input, DEFAULTS)
            profile_id = _profile_id(profile_name)
            await runtime.async_configure(
                {
                    "operation": "add_profile",
                    "profile_id": profile_id,
                    "name": profile_name,
                    "settings": settings,
                },
                expected_revision=self._expected_revision,
            )
        except (ValueError, RuntimeError, json.JSONDecodeError):
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(
                step_id="add_profile",
                data_schema=schema,
                errors={"base": "invalid_configuration"},
            )
        return self.async_create_entry(title="", data=self.config_entry.options)

    async def async_step_edit_profile(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Choose the profile by its visible name before editing its settings."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        if not runtime.configuration.profiles:
            return self.async_abort(reason="no_profiles")
        if user_input is not None:
            self._profile_id = user_input["profile_id"]
            return await self.async_step_edit_profile_settings()
        return self.async_show_form(
            step_id="edit_profile",
            data_schema=probatio.Schema(
                {
                    probatio.Required("profile_id"): _record_selector(
                        _profile_options(runtime)
                    )
                }
            ),
        )

    async def async_step_edit_profile_settings(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Edit standard settings for the selected profile."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        profile = runtime.configuration.profiles.get(getattr(self, "_profile_id", ""))
        if profile is None:
            return self.async_abort(reason="unknown")
        schema = _settings_schema(profile["settings"], required=True)
        if user_input is None:
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(
                step_id="edit_profile_settings",
                data_schema=schema,
                description_placeholders={"profile_name": profile["name"]},
            )
        try:
            patch = _collect_settings(user_input, profile["settings"])
            await runtime.async_configure(
                {
                    "operation": "edit_profile",
                    "profile_id": self._profile_id,
                    "patch": patch,
                },
                expected_revision=self._expected_revision,
            )
        except (ValueError, RuntimeError, json.JSONDecodeError):
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(
                step_id="edit_profile_settings",
                data_schema=schema,
                errors={"base": "invalid_configuration"},
                description_placeholders={"profile_name": profile["name"]},
            )
        return self.async_create_entry(title="", data=self.config_entry.options)

    async def async_step_assign(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Assign a tablet using readable labels for both registries."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        profiles = _profile_options(runtime)
        tablets = _tablet_options(runtime)
        if not profiles or not tablets:
            return self.async_abort(reason="no_tablets_or_profiles")
        schema = probatio.Schema(
            {
                probatio.Required("tablet_id"): _record_selector(tablets),
                probatio.Required("profile_id"): _record_selector(profiles),
                probatio.Optional("clear_overrides", default=False): BooleanSelector(),
            }
        )
        if user_input is None:
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(step_id="assign", data_schema=schema)
        try:
            await runtime.async_configure(
                {"operation": "assign", **user_input},
                expected_revision=self._expected_revision,
            )
        except (ValueError, RuntimeError):
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(
                step_id="assign", data_schema=schema, errors={"base": "invalid_configuration"}
            )
        return self.async_create_entry(title="", data=self.config_entry.options)

    async def async_step_set_overrides(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Choose a tablet before editing its desired overrides."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        tablets = _tablet_options(runtime)
        if not tablets:
            return self.async_abort(reason="no_tablets")
        if user_input is not None:
            self._tablet_id = user_input["tablet_id"]
            return await self.async_step_set_override_values()
        return self.async_show_form(
            step_id="set_overrides",
            data_schema=probatio.Schema(
                {probatio.Required("tablet_id"): _record_selector(tablets)}
            ),
        )

    async def async_step_set_override_values(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Set per-tablet overrides with choices matching the shared model."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        tablet = runtime.configuration.tablets.get(getattr(self, "_tablet_id", ""))
        if tablet is None:
            return self.async_abort(reason="unknown")
        existing = tablet["overrides"]
        schema = _settings_schema(existing, required=False)
        if user_input is None:
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(
                step_id="set_override_values",
                data_schema=schema,
                description_placeholders={"tablet_name": tablet["name"]},
            )
        try:
            overrides = _collect_settings(user_input, existing)
            await runtime.async_configure(
                {
                    "operation": "set_overrides",
                    "tablet_id": self._tablet_id,
                    "overrides": overrides,
                },
                expected_revision=self._expected_revision,
            )
        except (ValueError, RuntimeError, json.JSONDecodeError):
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(
                step_id="set_override_values",
                data_schema=schema,
                errors={"base": "invalid_configuration"},
                description_placeholders={"tablet_name": tablet["name"]},
            )
        return self.async_create_entry(title="", data=self.config_entry.options)

    async def async_step_clear_overrides(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Clear all overrides on a selected tablet."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        tablets = _tablet_options(runtime)
        if not tablets:
            return self.async_abort(reason="no_tablets")
        schema = probatio.Schema(
            {probatio.Required("tablet_id"): _record_selector(tablets)}
        )
        if user_input is None:
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(step_id="clear_overrides", data_schema=schema)
        try:
            await runtime.async_configure(
                {
                    "operation": "set_overrides",
                    "tablet_id": user_input["tablet_id"],
                    "overrides": {},
                },
                expected_revision=self._expected_revision,
            )
        except (ValueError, RuntimeError):
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(
                step_id="clear_overrides",
                data_schema=schema,
                errors={"base": "invalid_configuration"},
            )
        return self.async_create_entry(title="", data=self.config_entry.options)

    async def async_step_delete_profile(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Delete an unassigned profile selected by its visible name."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        profiles = _profile_options(runtime)
        if not profiles:
            return self.async_abort(reason="no_profiles")
        schema = probatio.Schema(
            {probatio.Required("profile_id"): _record_selector(profiles)}
        )
        if user_input is None:
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(step_id="delete_profile", data_schema=schema)
        try:
            await runtime.async_configure(
                {"operation": "delete_profile", **user_input},
                expected_revision=self._expected_revision,
            )
        except (ValueError, RuntimeError):
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(
                step_id="delete_profile",
                data_schema=schema,
                errors={"base": "invalid_configuration"},
            )
        return self.async_create_entry(title="", data=self.config_entry.options)

    async def async_step_approve_tablet(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Pair a tablet that announced itself, after comparing its code."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        waiting = runtime.pending_announcements()
        if not waiting:
            return self.async_abort(reason="no_waiting_tablets")
        tablets = [
            SelectOptionDict(
                label=f"{item['name']} · code {item['code'][:3]} {item['code'][3:]}",
                value=item["tablet_id"],
            )
            for item in waiting
        ]
        schema = probatio.Schema(
            {
                probatio.Required("tablet_id"): _record_selector(tablets),
                probatio.Required("profile_id"): _record_selector(_profile_options(runtime)),
                probatio.Required("code_matches", default=False): bool,
            }
        )
        if user_input is None:
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(step_id="approve_tablet", data_schema=schema)
        if not user_input.get("code_matches"):
            return self.async_show_form(
                step_id="approve_tablet",
                data_schema=schema,
                errors={"base": "code_not_confirmed"},
            )
        try:
            await runtime.async_approve(
                tablet_id=user_input["tablet_id"],
                profile_id=user_input["profile_id"],
                expected_revision=self._expected_revision,
            )
        except (ValueError, RuntimeError):
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(
                step_id="approve_tablet",
                data_schema=schema,
                errors={"base": "invalid_configuration"},
            )
        return self.async_create_entry(title="", data=self.config_entry.options)

    async def async_step_remove_tablet(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Remove a tablet selected by its visible name."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        tablets = _tablet_options(runtime)
        if not tablets:
            return self.async_abort(reason="no_tablets")
        schema = probatio.Schema(
            {probatio.Required("tablet_id"): _record_selector(tablets)}
        )
        if user_input is None:
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(step_id="remove_tablet", data_schema=schema)
        try:
            await runtime.async_configure(
                {"operation": "remove_tablet", **user_input},
                expected_revision=self._expected_revision,
            )
        except (ValueError, RuntimeError):
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(
                step_id="remove_tablet",
                data_schema=schema,
                errors={"base": "invalid_configuration"},
            )
        return self.async_create_entry(title="", data=self.config_entry.options)


def _settings_schema(
    current: dict[str, Any], *, required: bool, include_name: bool = False
) -> probatio.Schema:
    """Build native fields from the same choices and ranges as the model."""
    fields: dict[Any, Any] = {}
    if include_name:
        fields[probatio.Required("name")] = TextSelector(
            TextSelectorConfig(type=TextSelectorType.TEXT)
        )
    for key in _SIMPLE_KEYS:
        if required or key in current:
            marker = probatio.Required(key, default=current.get(key, DEFAULTS[key]))
        else:
            marker = probatio.Optional(key)
        if key in CHOICES:
            options = [
                SelectOptionDict(
                    value=value,
                    label=_SETTING_LABELS.get(key, {}).get(value, value.replace("_", " ").title()),
                )
                for value in CHOICES[key]
            ]
            fields[marker] = SelectSelector(
                SelectSelectorConfig(options=options, mode=SelectSelectorMode.DROPDOWN)
            )
        elif key in RANGES:
            low, high = RANGES[key]
            fields[marker] = NumberSelector(
                NumberSelectorConfig(
                    min=low,
                    max=high,
                    step=1,
                    mode=NumberSelectorMode.BOX,
                )
            )
        elif key == "update_time":
            fields[marker] = TextSelector(
                TextSelectorConfig(type=TextSelectorType.TEXT)
            )
    advanced = {key: current[key] for key in _ADVANCED_KEYS if key in current}
    fields[
        probatio.Required("advanced_settings_json", default=json.dumps(advanced))
    ] = TextSelector(TextSelectorConfig(type=TextSelectorType.TEXT))
    return probatio.Schema(fields)


def _collect_settings(user_input: dict[str, Any], current: dict[str, Any]) -> dict[str, Any]:
    """Merge native values with the explicitly advanced ordering fields."""
    result = dict(current)
    for key in _SIMPLE_KEYS:
        if key not in user_input or user_input[key] is None:
            continue
        value = user_input[key]
        if key in RANGES and isinstance(value, float) and value.is_integer():
            value = int(value)
        result[key] = value
    for key in _ADVANCED_KEYS:
        result.pop(key, None)
    advanced = json.loads(user_input.get("advanced_settings_json", "{}"))
    if not isinstance(advanced, dict) or set(advanced) - set(_ADVANCED_KEYS):
        raise ValueError("Invalid advanced settings")
    result.update(advanced)
    return result


def _record_selector(options: list[SelectOptionDict]) -> SelectSelector:
    """Build a labelled selector for existing model records."""
    return SelectSelector(
        SelectSelectorConfig(options=options, mode=SelectSelectorMode.DROPDOWN)
    )


def _profile_options(runtime: NjorunRuntime) -> list[SelectOptionDict]:
    return [
        SelectOptionDict(label=record["name"], value=profile_id)
        for profile_id, record in runtime.configuration.profiles.items()
    ]


def _tablet_options(runtime: NjorunRuntime) -> list[SelectOptionDict]:
    return [
        SelectOptionDict(label=record["name"], value=tablet_id)
        for tablet_id, record in runtime.configuration.tablets.items()
    ]


def _profile_id(profile_name: str) -> str:
    """Generate a stable model id from the human-readable profile name."""
    value = re.sub(r"[^a-z0-9]+", "_", profile_name.casefold()).strip("_")
    if not value:
        raise ValueError("Profile name cannot form an identifier")
    return value[:128]


def _runtime_for_flow(hass: Any, entry: ConfigEntry) -> NjorunRuntime:
    """Resolve the loaded runtime without putting secrets in flow options."""
    try:
        return hass.data[DOMAIN]["entries"][entry.entry_id]
    except KeyError as err:
        raise RuntimeError("Njörun is not loaded") from err
