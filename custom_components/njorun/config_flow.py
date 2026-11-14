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
from homeassistant.helpers import area_registry as ar
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers import entity_registry as er
from homeassistant.helpers.selector import (
    AreaSelector,
    AreaSelectorConfig,
    BooleanSelector,
    EntitySelector,
    EntitySelectorConfig,
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
from .model import CHOICES, DEFAULTS, RANGES, ROOM_ICONS, options_menu
from .network import NetworkError, ip_from_text
from .runtime import NjorunRuntime, RuntimeError

_ADVANCED_KEYS = ("favorites", "room_order", "entity_order", "excluded_entities")
_MAP_KEYS = ("entity_names", "room_names", "room_icons")
# The profile choice that creates a profile for one tablet; no profile ID looks like this.
NEW_PROFILE = "new-profile-for-this-tablet"

_SIMPLE_KEYS = tuple(
    key for key in DEFAULTS if key not in _ADVANCED_KEYS + _MAP_KEYS + ("color_scheme",)
)
_SETTING_LABELS = {
    "orientation": {
        "portrait": "Portrait",
        "landscape": "Landscape",
        "portrait_flipped": "Portrait, upside down",
        "landscape_flipped": "Landscape, upside down",
    },
    "theme": {"automatic": "Automatic", "light": "Light", "dark": "Dark"},
    "light_color_scheme": {
        "njorun": "Njörun Teal",
        "graphite": "Graphite",
        "warm": "Warm",
        "porcelain": "Porcelain",
    },
    "dark_color_scheme": {
        "njorun": "Njörun Teal",
        "graphite": "Graphite",
        "warm": "Ember",
        "porcelain": "Ink",
    },
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
_SETTING_LABELS_DE = {
    "light_color_scheme": {
        "njorun": "Njörun Petrol",
        "graphite": "Graphit",
        "warm": "Warm",
        "porcelain": "Porzellan",
    },
    "dark_color_scheme": {
        "njorun": "Njörun Petrol",
        "graphite": "Graphit",
        "warm": "Glut",
        "porcelain": "Tinte",
    },
}
_ROOM_ICON_LABELS = {
    "home": "Home",
    "sofa": "Sofa",
    "kitchen": "Kitchen",
    "bed": "Bed",
    "desk": "Desk",
    "bath": "Bathroom",
    "garage": "Garage",
    "garden": "Garden",
}
_ROOM_ICON_LABELS_DE = {
    "home": "Zuhause",
    "sofa": "Sofa",
    "kitchen": "Küche",
    "bed": "Bett",
    "desk": "Schreibtisch",
    "bath": "Bad",
    "garage": "Garage",
    "garden": "Garten",
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
        # The name gives the device and the start of its entity IDs, so it is chosen here and
        # not renamed later (a rename does not change entity IDs that exist).
        fields: dict[Any, Any] = {
            probatio.Required("name", default=waiting["name"]): str,
            probatio.Required("profile_id", default=_default_profile_id(runtime)): _record_selector(
                options
            ),
            probatio.Required("theme", default=waiting["theme"] or DEFAULTS["theme"]): _record_selector(
                [
                    SelectOptionDict(label=_SETTING_LABELS["theme"][value], value=value)
                    for value in CHOICES["theme"]
                ]
            ),
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
                name=user_input.get("name"),
                theme=user_input.get("theme"),
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
            menu_options=options_menu(profiles, len(runtime.network_tablets())),
            description_placeholders={"profiles": str(profiles)},
        )

    async def async_step_add_profile(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Create a profile with native choices and bounded values."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        schema = _settings_schema(
            DEFAULTS,
            required=True,
            include_name=True,
            german=runtime.hass.config.language.lower().startswith("de"),
        )
        if user_input is None:
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(step_id="add_profile", data_schema=schema)
        try:
            profile_name = user_input.pop("name")
            settings = _collect_settings(user_input, DEFAULTS, runtime.hass)
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
        schema = _settings_schema(
            profile["settings"],
            required=True,
            german=runtime.hass.config.language.lower().startswith("de"),
        )
        if user_input is None:
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(
                step_id="edit_profile_settings",
                data_schema=schema,
                description_placeholders={"profile_name": profile["name"]},
            )
        try:
            patch = _collect_settings(user_input, profile["settings"], runtime.hass)
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

    async def async_step_edit_entity_names(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Choose the profile whose tablet entity aliases are edited."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        if not runtime.configuration.profiles:
            return self.async_abort(reason="no_profiles")
        if len(runtime.configuration.profiles) == 1:
            self._profile_id = next(iter(runtime.configuration.profiles))
            return await self.async_step_edit_entity_name_select()
        if user_input is not None:
            self._profile_id = user_input["profile_id"]
            return await self.async_step_edit_entity_name_select()
        return self.async_show_form(
            step_id="edit_entity_names",
            data_schema=probatio.Schema(
                {probatio.Required("profile_id"): _record_selector(_profile_options(runtime))}
            ),
        )

    async def async_step_edit_entity_name_select(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Select one entity whose device-dashboard label is customized."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        if getattr(self, "_profile_id", "") not in runtime.configuration.profiles:
            return self.async_abort(reason="unknown")
        if user_input is not None:
            self._entity_id = user_input["entity_id"]
            return await self.async_step_edit_entity_name_value()
        return self.async_show_form(
            step_id="edit_entity_name_select",
            data_schema=probatio.Schema(
                {
                    probatio.Required("entity_id"): EntitySelector(
                        EntitySelectorConfig()
                    )
                }
            ),
        )

    async def async_step_edit_entity_name_value(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Set or clear the selected entity's display name in one profile."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        profile = runtime.configuration.profiles.get(getattr(self, "_profile_id", ""))
        if profile is None:
            return self.async_abort(reason="unknown")
        schema = probatio.Schema(
            {
                probatio.Required(
                    "entity_name",
                    default=profile["settings"]["entity_names"].get(self._entity_id, ""),
                ): TextSelector(TextSelectorConfig(type=TextSelectorType.TEXT))
            }
        )
        if user_input is None:
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(
                step_id="edit_entity_name_value",
                data_schema=schema,
                description_placeholders={"entity_id": self._entity_id},
            )
        entity_names = dict(profile["settings"]["entity_names"])
        value = (user_input.get("entity_name") or "").strip()
        if value:
            entity_names[self._entity_id] = value
        else:
            entity_names.pop(self._entity_id, None)
        try:
            await runtime.async_configure(
                {
                    "operation": "edit_profile",
                    "profile_id": self._profile_id,
                    "patch": {"entity_names": entity_names},
                },
                expected_revision=self._expected_revision,
            )
        except (ValueError, RuntimeError):
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(
                step_id="edit_entity_name_value",
                data_schema=schema,
                errors={"base": "invalid_configuration"},
                description_placeholders={"entity_id": self._entity_id},
            )
        return self.async_create_entry(title="", data=self.config_entry.options)

    async def async_step_edit_room_display(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Choose the profile whose room names and icons are edited."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        if not runtime.configuration.profiles:
            return self.async_abort(reason="no_profiles")
        if len(runtime.configuration.profiles) == 1:
            self._profile_id = next(iter(runtime.configuration.profiles))
            return await self.async_step_edit_room_display_select()
        if user_input is not None:
            self._profile_id = user_input["profile_id"]
            return await self.async_step_edit_room_display_select()
        return self.async_show_form(
            step_id="edit_room_display",
            data_schema=probatio.Schema(
                {probatio.Required("profile_id"): _record_selector(_profile_options(runtime))}
            ),
        )

    async def async_step_edit_room_display_select(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Select an HA area whose device-dashboard name or icon is customized."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        if getattr(self, "_profile_id", "") not in runtime.configuration.profiles:
            return self.async_abort(reason="unknown")
        if not ar.async_get(self.hass).areas:
            return self.async_abort(reason="no_areas")
        if user_input is not None:
            self._area_id = user_input["area_id"]
            return await self.async_step_edit_room_display_settings()
        return self.async_show_form(
            step_id="edit_room_display_select",
            data_schema=probatio.Schema(
                {
                    probatio.Required("area_id"): AreaSelector(AreaSelectorConfig())
                }
            ),
        )

    async def async_step_edit_room_display_settings(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Set a room's display name and one of the supported Njörun icons."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        profile = runtime.configuration.profiles.get(getattr(self, "_profile_id", ""))
        if profile is None:
            return self.async_abort(reason="unknown")
        current = profile["settings"]
        icon_options = _room_icon_options(self.hass.config.language.lower().startswith("de"))
        schema = probatio.Schema(
            {
                probatio.Optional(
                    "room_name", default=current["room_names"].get(self._area_id, "")
                ): TextSelector(TextSelectorConfig(type=TextSelectorType.TEXT)),
                probatio.Required(
                    "room_icon",
                    default=current["room_icons"].get(self._area_id, "default"),
                ): SelectSelector(
                    SelectSelectorConfig(options=icon_options, mode=SelectSelectorMode.DROPDOWN)
                ),
            }
        )
        if user_input is None:
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(
                step_id="edit_room_display_settings",
                data_schema=schema,
                description_placeholders={"area_id": self._area_id},
            )
        room_names = dict(current["room_names"])
        room_icons = dict(current["room_icons"])
        room_name = (user_input.get("room_name") or "").strip()
        if room_name:
            room_names[self._area_id] = room_name
        else:
            room_names.pop(self._area_id, None)
        room_icon = user_input.get("room_icon", "default")
        if room_icon == "default":
            room_icons.pop(self._area_id, None)
        elif room_icon in ROOM_ICONS:
            room_icons[self._area_id] = room_icon
        else:
            return self.async_show_form(
                step_id="edit_room_display_settings",
                data_schema=schema,
                errors={"base": "invalid_configuration"},
                description_placeholders={"area_id": self._area_id},
            )
        try:
            await runtime.async_configure(
                {
                    "operation": "edit_profile",
                    "profile_id": self._profile_id,
                    "patch": {"room_names": room_names, "room_icons": room_icons},
                },
                expected_revision=self._expected_revision,
            )
        except (ValueError, RuntimeError):
            self._expected_revision = runtime.configuration.revision
            return self.async_show_form(
                step_id="edit_room_display_settings",
                data_schema=schema,
                errors={"base": "invalid_configuration"},
                description_placeholders={"area_id": self._area_id},
            )
        return self.async_create_entry(title="", data=self.config_entry.options)

    async def async_step_network_profile(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Choose the profile whose Wi-Fi credentials are set."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        if not runtime.configuration.profiles:
            return self.async_abort(reason="no_profiles")
        if len(runtime.configuration.profiles) == 1:
            self._profile_id = next(iter(runtime.configuration.profiles))
            return await self.async_step_network_profile_wifi()
        if user_input is not None:
            self._profile_id = user_input["profile_id"]
            return await self.async_step_network_profile_wifi()
        return self.async_show_form(
            step_id="network_profile",
            data_schema=probatio.Schema(
                {probatio.Required("profile_id"): _record_selector(_profile_options(runtime))}
            ),
        )

    async def async_step_network_profile_wifi(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Set the Wi-Fi name and password that a profile gives its tablets.

        An empty name removes the credentials. An empty password keeps the stored one when the
        name is unchanged; with another name it means an open network.
        """
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        profile = runtime.configuration.profiles.get(getattr(self, "_profile_id", ""))
        if profile is None:
            return self.async_abort(reason="unknown")
        current = runtime.network_profile_form(self._profile_id)
        placeholders = {
            "profile_name": profile["name"],
            "password_state": "set" if current["password_set"] else "not set",
        }
        if user_input is None:
            return self.async_show_form(
                step_id="network_profile_wifi",
                data_schema=_wifi_schema(current["ssid"]),
                description_placeholders=placeholders,
            )
        errors: dict[str, str] = {}
        try:
            stored = runtime.network_settings.profiles.get(self._profile_id, {}).get("wifi")
            await runtime.async_set_profile_network(
                self._profile_id, _wifi_from_form(user_input, stored)
            )
        except NetworkError as err:
            errors["base"] = _network_error(err)
        except (ValueError, RuntimeError):
            errors["base"] = "invalid_network"
        if errors:
            return self.async_show_form(
                step_id="network_profile_wifi",
                data_schema=_wifi_schema(user_input.get("ssid", "")),
                errors=errors,
                description_placeholders=placeholders,
            )
        return self.async_create_entry(title="", data=self.config_entry.options)

    async def async_step_network_tablet(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Choose the tablet whose network is set."""
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        tablets = runtime.network_tablets()
        if not tablets:
            return self.async_abort(reason="no_network_tablets")
        if len(tablets) == 1:
            self._tablet_id = tablets[0]
            return await self.async_step_network_tablet_settings()
        if user_input is not None:
            self._tablet_id = user_input["tablet_id"]
            return await self.async_step_network_tablet_settings()
        options = [
            SelectOptionDict(value=tablet_id, label=runtime.configuration.tablets[tablet_id]["name"])
            for tablet_id in tablets
        ]
        return self.async_show_form(
            step_id="network_tablet",
            data_schema=probatio.Schema({probatio.Required("tablet_id"): _record_selector(options)}),
        )

    async def async_step_network_tablet_settings(
        self, user_input: dict[str, Any] | None = None
    ) -> ConfigFlowResult:
        """Set a tablet's own Wi-Fi credentials and the IP setup of each of its interfaces.

        An empty Wi-Fi name makes the tablet follow its profile again. DHCP is the default.
        """
        runtime = _runtime_for_flow(self.hass, self.config_entry)
        tablet = runtime.configuration.tablets.get(getattr(self, "_tablet_id", ""))
        if tablet is None or not runtime.network_tablets().count(self._tablet_id):
            return self.async_abort(reason="unknown")
        current = runtime.network_form(self._tablet_id)
        placeholders = {
            "tablet_name": tablet["name"],
            "password_state": "set" if current["password_set"] else "not set",
        }
        schema = _tablet_network_schema(current["interfaces"], current["ssid"], current["ip"])
        if user_input is None:
            return self.async_show_form(
                step_id="network_tablet_settings",
                data_schema=schema,
                description_placeholders=placeholders,
            )
        errors: dict[str, str] = {}
        try:
            stored = runtime.network_settings.tablets.get(self._tablet_id, {}).get("wifi")
            # DHCP that was never set stays unset, so a tablet keeps what was set on it (N4).
            ip = {
                interface: ip_from_text(
                    user_input.get(f"{interface}_mode", "dhcp"),
                    user_input.get(f"{interface}_address", ""),
                    user_input.get(f"{interface}_prefix", ""),
                    user_input.get(f"{interface}_gateway", ""),
                    user_input.get(f"{interface}_dns", ""),
                )
                for interface in current["interfaces"]
                if user_input.get(f"{interface}_mode", "dhcp") == "static"
                or interface in current["ip"]
            }
            await runtime.async_set_tablet_network(
                self._tablet_id, wifi=_wifi_from_form(user_input, stored), ip=ip
            )
        except NetworkError as err:
            errors["base"] = _network_error(err)
        except (ValueError, RuntimeError):
            errors["base"] = "invalid_network"
        if errors:
            return self.async_show_form(
                step_id="network_tablet_settings",
                data_schema=_tablet_network_schema(
                    current["interfaces"], user_input.get("ssid", ""), current["ip"]
                ),
                errors=errors,
                description_placeholders=placeholders,
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


_NETWORK_ERRORS = {
    "ssid": "invalid_ssid",
    "password": "invalid_password",
    "address": "invalid_address",
    "prefix": "invalid_prefix",
    "gateway": "invalid_gateway",
    "dns": "invalid_dns",
    "shape": "static_incomplete",
}


def _network_error(err: NetworkError) -> str:
    """The form error for a rejected value; it never names the value."""
    return _NETWORK_ERRORS.get(err.problem, "invalid_network")


def _wifi_from_form(
    user_input: dict[str, Any], stored: dict[str, str] | None
) -> dict[str, str] | None:
    """The credentials a form means: none without a name, the stored password when the name and
    the password field are unchanged, otherwise what was typed."""
    ssid = (user_input.get("ssid") or "").strip()
    if not ssid:
        return None
    password = user_input.get("password") or ""
    if not password and stored and stored.get("ssid") == ssid:
        password = stored.get("password", "")
    wifi = {"ssid": ssid}
    if password:
        wifi["password"] = password
    return wifi


def _wifi_schema(ssid: str) -> probatio.Schema:
    return probatio.Schema(
        {
            probatio.Optional("ssid", default=ssid): TextSelector(TextSelectorConfig()),
            probatio.Optional("password", default=""): TextSelector(
                TextSelectorConfig(type=TextSelectorType.PASSWORD)
            ),
        }
    )


def _tablet_network_schema(
    interfaces: list[str], ssid: str, ip: dict[str, dict[str, Any]]
) -> probatio.Schema:
    """The tablet's form: its own Wi-Fi credentials, then one IP block per interface."""
    fields: dict[Any, Any] = {
        probatio.Optional("ssid", default=ssid): TextSelector(TextSelectorConfig()),
        probatio.Optional("password", default=""): TextSelector(
            TextSelectorConfig(type=TextSelectorType.PASSWORD)
        ),
    }
    text = TextSelector(TextSelectorConfig())
    for interface in interfaces:
        setup = ip.get(interface, {"mode": "dhcp"})
        fields[probatio.Optional(f"{interface}_mode", default=setup["mode"])] = SelectSelector(
            SelectSelectorConfig(
                options=[
                    SelectOptionDict(value="dhcp", label="DHCP"),
                    SelectOptionDict(value="static", label="Static"),
                ],
                mode=SelectSelectorMode.DROPDOWN,
                translation_key="network_mode",
            )
        )
        fields[probatio.Optional(f"{interface}_address", default=setup.get("address", ""))] = text
        fields[probatio.Optional(f"{interface}_prefix", default=str(setup.get("prefix", "")))] = text
        fields[probatio.Optional(f"{interface}_gateway", default=setup.get("gateway", ""))] = text
        fields[probatio.Optional(f"{interface}_dns", default=" ".join(setup.get("dns", [])))] = text
    return probatio.Schema(fields)


def _settings_schema(
    current: dict[str, Any],
    *,
    required: bool,
    include_name: bool = False,
    german: bool = False,
) -> probatio.Schema:
    """Build native settings and ordered entity/area selectors for a profile."""
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
            labels = _SETTING_LABELS_DE if german else _SETTING_LABELS
            options = [
                SelectOptionDict(
                    value=value,
                    label=labels.get(key, _SETTING_LABELS.get(key, {})).get(
                        value, value.replace("_", " ").title()
                    ),
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
        elif type(DEFAULTS.get(key)) is bool:
            fields[marker] = BooleanSelector()
    for key in ("favorites", "room_order", "entity_order", "excluded_entities"):
        if not required and key not in current:
            continue
        if key == "entity_order":
            # HA's reorderable selector keeps this a readable ordered list. The selected
            # entities are grouped by their current area when settings are saved.
            marker = probatio.Required(
                "entity_order_items",
                default=_ordered_entity_ids(current.get("entity_order", {})),
            )
            fields[marker] = EntitySelector(
                EntitySelectorConfig(multiple=True, reorder=True)
            )
        elif key == "room_order":
            fields[probatio.Required(key, default=current.get(key, []))] = AreaSelector(
                AreaSelectorConfig(multiple=True, reorder=True)
            )
        else:
            fields[probatio.Required(key, default=current.get(key, []))] = EntitySelector(
                EntitySelectorConfig(multiple=True, reorder=True)
            )
    return probatio.Schema(fields)


def _collect_settings(
    user_input: dict[str, Any], current: dict[str, Any], hass: Any | None = None
) -> dict[str, Any]:
    """Merge native settings while accepting old JSON form submissions during upgrade."""
    result = dict(current)
    for key in _SIMPLE_KEYS:
        if key not in user_input or user_input[key] is None:
            continue
        value = user_input[key]
        if key in RANGES and isinstance(value, float) and value.is_integer():
            value = int(value)
        result[key] = value
    if "advanced_settings_json" in user_input:
        # Older open forms submitted one JSON object. Keep accepting that shape for imports
        # and in-flight forms, but no longer expose it in the normal UI.
        advanced = json.loads(user_input.get("advanced_settings_json") or "{}")
        if not isinstance(advanced, dict) or set(advanced) - set(_ADVANCED_KEYS):
            raise ValueError("Invalid advanced settings")
        result.update(advanced)
        return result

    for key in ("favorites", "excluded_entities", "room_order"):
        if key not in user_input:
            continue
        selected = user_input[key] or []
        if not isinstance(selected, list):
            raise ValueError(f"Invalid {key}")
        registry_ids = _selector_registry_ids(hass, key)
        result[key] = _preserve_unavailable(current.get(key, []), selected, registry_ids)
    if "entity_order_items" in user_input:
        selected = user_input["entity_order_items"] or []
        if not isinstance(selected, list):
            raise ValueError("Invalid entity order")
        result["entity_order"] = _collect_entity_order(
            selected, current.get("entity_order", {}), hass
        )
    return result


def _ordered_entity_ids(entity_order: Any) -> list[str]:
    """Flatten per-room entity order for HA's reorderable entity selector."""
    if not isinstance(entity_order, dict):
        return []
    return list(dict.fromkeys(
        entity_id
        for entities in entity_order.values()
        if isinstance(entities, list)
        for entity_id in entities
        if isinstance(entity_id, str)
    ))


def _selector_registry_ids(hass: Any | None, key: str) -> set[str] | None:
    """IDs available to the relevant HA selector; None means no registry was supplied."""
    if hass is None:
        return None
    if key == "room_order":
        return set(ar.async_get(hass).areas)
    return set(er.async_get(hass).entities)


def _preserve_unavailable(
    current: list[str], selected: list[str], registry_ids: set[str] | None
) -> list[str]:
    """Keep stored IDs that HA cannot currently offer, without blocking normal reordering."""
    if registry_ids is None:
        return list(selected)
    result = list(selected)
    for index, value in enumerate(current):
        if value not in registry_ids and value not in result:
            result.insert(min(index, len(result)), value)
    return result


def _collect_entity_order(
    selected: list[str], current: dict[str, list[str]], hass: Any | None
) -> dict[str, list[str]]:
    """Group ordered entities by direct/device area or the unassigned room."""
    if hass is None:
        # Without an HA registry we cannot infer the area for a new selection. Preserve the
        # existing mapping in pure callers and test harnesses.
        return {room: list(entities) for room, entities in current.items()}
    registry = er.async_get(hass).entities
    devices = dr.async_get(hass).devices
    result: dict[str, list[str]] = {}
    for entity_id in selected:
        entry = registry.get(entity_id)
        if entry is not None:
            area_id = _effective_entity_area(entry, devices) or "unassigned"
            result.setdefault(area_id, []).append(entity_id)
    for room, entities in current.items():
        for index, entity_id in enumerate(entities):
            if entity_id not in registry:
                # HA cannot offer a deleted entity in its selector. Keep this stored order
                # entry at its prior position until it becomes selectable or is imported away.
                ordered = result.setdefault(room, [])
                if entity_id not in ordered:
                    ordered.insert(min(index, len(ordered)), entity_id)
    return {room: entities for room, entities in result.items() if entities}


def _effective_entity_area(entity: Any, devices: dict[str, Any]) -> str | None:
    """Use the entity area, then the nearest assigned parent device area."""
    if entity.area_id:
        return entity.area_id
    device_id = entity.device_id
    seen: set[str] = set()
    while device_id is not None and device_id not in seen:
        seen.add(device_id)
        device = devices.get(device_id)
        if device is None:
            return None
        if device.area_id:
            return device.area_id
        device_id = device.via_device_id
    return None


def _record_selector(options: list[SelectOptionDict]) -> SelectSelector:
    """Build a labelled selector for existing model records."""
    return SelectSelector(
        SelectSelectorConfig(options=options, mode=SelectSelectorMode.DROPDOWN)
    )


def _room_icon_options(german: bool) -> list[SelectOptionDict]:
    """Labels for the eight supported room icons, plus a choice to use the default."""
    labels = _ROOM_ICON_LABELS_DE if german else _ROOM_ICON_LABELS
    result = [
        SelectOptionDict(
            label="Standard" if german else "Use default",
            value="default",
        )
    ]
    result.extend(SelectOptionDict(label=labels[icon], value=icon) for icon in ROOM_ICONS)
    return result


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
