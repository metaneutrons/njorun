# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: Apache-2.0

"""Home Assistant setup, tablet approval and shared-profile configuration flows."""

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
from .model import CHOICES, DEFAULTS, RANGES, options_menu
from .runtime import NjorunRuntime, RuntimeError

_ADVANCED_KEYS = ("favorites", "room_order", "entity_order", "excluded_entities")
# The profile choice that creates a profile for one tablet; no profile ID looks like this.
NEW_PROFILE = "new-profile-for-this-tablet"

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
class NjorunConfigFlow(ConfigFlow, domain=DOMAIN):
    """Create the single integration entry for this Home Assistant instance."""

    VERSION = 1

    async def async_step_user(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Install Njörun in one step; tablets are approved afterwards.

        Profiles are an advanced concept (settings several tablets share), so setup asks
        nothing: a default profile is created and tablets start in it.
        """
        await self.async_set_unique_id(DOMAIN)
        self._abort_if_unique_id_configured()
        german = self.hass.config.language.lower().startswith("de")
        return self.async_create_entry(
            title="Njörun",
            data={"initial_profile_name": "Standard" if german else "Default"},
        )

    async def async_step_integration_discovery(
        self, discovery_info: dict[str, Any]
    ) -> ConfigFlowResult:
        """A tablet asks to be paired; offer it as a Discovered card."""
        tablet_id = discovery_info["tablet_id"]
        await self.async_set_unique_id(tablet_id)
        self._abort_if_unique_id_configured()
        runtime = _loaded_runtime(self.hass)
        waiting = runtime.announcement(tablet_id) if runtime else None
        if waiting is None:
            return self.async_abort(reason="tablet_not_waiting")
        self._tablet_id = tablet_id
        self.context["title_placeholders"] = {"name": waiting["name"]}
        return await self.async_step_confirm()

    async def async_step_confirm(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Approve the tablet after comparing the code; ask for a profile only if there is a choice."""
        runtime = _loaded_runtime(self.hass)
        waiting = runtime.announcement(self._tablet_id) if runtime else None
        if runtime is None or waiting is None:
            return self.async_abort(reason="tablet_not_waiting")
        # A shared profile or one for this tablet only (plan 004, decision P7).
        german = self.hass.config.language.lower().startswith("de")
        options = [
            *_profile_options(runtime),
            SelectOptionDict(
                label="Neues Profil nur für dieses Tablet" if german else "New profile for this tablet only",
                value=NEW_PROFILE,
            ),
        ]
        fields: dict[Any, Any] = {
            probatio.Required("profile_id", default=_default_profile_id(runtime)): _record_selector(
                options
            )
        }
        placeholders = {
            "name": waiting["name"],
            "code": f"{waiting['code'][:3]} {waiting['code'][3:]}",
        }
        if user_input is None:
            return self.async_show_form(
                step_id="confirm",
                data_schema=probatio.Schema(fields),
                description_placeholders=placeholders,
            )
        choice = user_input.get("profile_id") or _default_profile_id(runtime)
        try:
            await runtime.async_approve(
                tablet_id=self._tablet_id,
                profile_id=None if choice == NEW_PROFILE else choice,
                new_profile=choice == NEW_PROFILE,
                expected_revision=runtime.configuration.revision,
            )
        except RuntimeError:
            return self.async_abort(reason="approval_failed")
        return self.async_abort(reason="tablet_approved")

    @staticmethod
    @callback
    def async_get_options_flow(config_entry: ConfigEntry) -> NjorunOptionsFlow:
        """Return the native per-entry profile and tablet options flow."""
        return NjorunOptionsFlow()


class NjorunOptionsFlow(OptionsFlow):
    """Manage the profiles that tablets share."""

    async def async_step_init(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Offer what makes sense now as a menu; the options manage shared profiles only."""
        try:
            runtime = _runtime_for_flow(self.hass, self.config_entry)
        except RuntimeError:
            return self.async_abort(reason="unknown")
        profiles = len(runtime.configuration.profiles)
        return self.async_show_menu(
            step_id="init",
            menu_options=options_menu(profiles),
            description_placeholders={"profiles": str(profiles)},
        )

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
        if len(runtime.configuration.profiles) == 1:
            self._profile_id = next(iter(runtime.configuration.profiles))
            return await self.async_step_edit_profile_settings()
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

    async def async_step_remote_logging(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Set where every tablet ships its log records: a Loki URL, or empty for off."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        schema = probatio.Schema(
            {
                probatio.Optional(
                    "log_sink", default=runtime.configuration.log_sink
                ): TextSelector(TextSelectorConfig(type=TextSelectorType.URL))
            }
        )
        if user_input is None:
            return self.async_show_form(step_id="remote_logging", data_schema=schema)
        try:
            await runtime.async_set_log_sink(user_input.get("log_sink") or "")
        except (ValueError, RuntimeError):
            return self.async_show_form(
                step_id="remote_logging",
                data_schema=schema,
                errors={"base": "invalid_log_sink"},
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


def _profile_id(profile_name: str) -> str:
    """Generate a stable model id from the human-readable profile name."""
    value = re.sub(r"[^a-z0-9]+", "_", profile_name.casefold()).strip("_")
    if not value:
        raise ValueError("Profile name cannot form an identifier")
    return value[:128]


def _loaded_runtime(hass: Any) -> NjorunRuntime | None:
    """The single loaded runtime, or None while Njörun is not set up."""
    entries = hass.data.get(DOMAIN, {}).get("entries", {})
    return next(iter(entries.values())) if len(entries) == 1 else None


def _default_profile_id(runtime: NjorunRuntime) -> str:
    """The profile a new tablet joins: the first one created, "default" at setup.

    Without any profile the only choice is a new one for the tablet.
    """
    profiles = runtime.configuration.profiles
    if not profiles:
        return NEW_PROFILE
    return "default" if "default" in profiles else next(iter(profiles))


def _runtime_for_flow(hass: Any, entry: ConfigEntry) -> NjorunRuntime:
    """Resolve the loaded runtime without putting secrets in flow options."""
    try:
        return hass.data[DOMAIN]["entries"][entry.entry_id]
    except KeyError as err:
        raise RuntimeError("Njörun is not loaded") from err
