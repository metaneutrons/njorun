"""Native configuration selectors for the Home Assistant profile editor."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import types
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
INTEGRATION = ROOT / "custom_components" / "njorun"
PACKAGE_NAME = "njorun_config_flow_test"
package = types.ModuleType(PACKAGE_NAME)
package.__path__ = [str(INTEGRATION)]
sys.modules[PACKAGE_NAME] = package


class _Marker:
    def __init__(self, key, default=None):
        self.key = key
        self.default = default


class _Schema:
    def __init__(self, fields):
        self.fields = fields


class _SelectorConfig:
    def __init__(self, **values):
        self.__dict__.update(values)


class _Selector:
    def __init__(self, config=None):
        self.config = config


class _Flow:
    def __init_subclass__(cls, *, domain=None, **kwargs):
        super().__init_subclass__(**kwargs)


class _Store:
    def __class_getitem__(cls, _item):
        return cls


def _load_config_flow():
    """Load the flow with small HA selector stubs; the workstation has no HA package."""
    ha = types.ModuleType("homeassistant")
    ha.__path__ = []
    config_entries = types.ModuleType("homeassistant.config_entries")
    config_entries.ConfigEntry = type("ConfigEntry", (), {})
    config_entries.ConfigFlow = _Flow
    config_entries.ConfigFlowResult = object
    config_entries.OptionsFlow = type("OptionsFlow", (), {})
    core = types.ModuleType("homeassistant.core")
    core.HomeAssistant = type("HomeAssistant", (), {})
    core.callback = lambda function: function
    helpers = types.ModuleType("homeassistant.helpers")
    helpers.__path__ = []
    storage = types.ModuleType("homeassistant.helpers.storage")
    storage.Store = _Store
    selector = types.ModuleType("homeassistant.helpers.selector")
    for name in (
        "AreaSelector",
        "BooleanSelector",
        "EntitySelector",
        "NumberSelector",
        "SelectSelector",
        "TextSelector",
    ):
        setattr(selector, name, type(name, (_Selector,), {}))
    for name in (
        "AreaSelectorConfig",
        "EntitySelectorConfig",
        "NumberSelectorConfig",
        "SelectSelectorConfig",
        "TextSelectorConfig",
    ):
        setattr(selector, name, type(name, (_SelectorConfig,), {}))
    selector.NumberSelectorMode = types.SimpleNamespace(BOX="box")
    selector.SelectSelectorMode = types.SimpleNamespace(DROPDOWN="dropdown")
    selector.SelectOptionDict = lambda **values: values
    selector.TextSelectorType = types.SimpleNamespace(TEXT="text", URL="url", PASSWORD="password")
    area_registry = types.ModuleType("homeassistant.helpers.area_registry")
    device_registry = types.ModuleType("homeassistant.helpers.device_registry")
    entity_registry = types.ModuleType("homeassistant.helpers.entity_registry")
    helpers.area_registry = area_registry
    helpers.device_registry = device_registry
    helpers.entity_registry = entity_registry

    probatio = types.ModuleType("probatio")
    probatio.Required = lambda key, default=None: _Marker(key, default)
    probatio.Optional = lambda key, default=None: _Marker(key, default)
    probatio.Schema = _Schema
    stub_modules = {
        "probatio": probatio,
        "homeassistant": ha,
        "homeassistant.config_entries": config_entries,
        "homeassistant.core": core,
        "homeassistant.helpers": helpers,
        "homeassistant.helpers.area_registry": area_registry,
        "homeassistant.helpers.device_registry": device_registry,
        "homeassistant.helpers.entity_registry": entity_registry,
        "homeassistant.helpers.selector": selector,
        "homeassistant.helpers.storage": storage,
    }
    spec = importlib.util.spec_from_file_location(
        f"{PACKAGE_NAME}.config_flow", INTEGRATION / "config_flow.py"
    )
    module = importlib.util.module_from_spec(spec)
    with patch.dict(sys.modules, stub_modules):
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
    return module, selector


config_flow, selector_stubs = _load_config_flow()


class ProfileSelectorTests(unittest.TestCase):
    def test_profile_editor_uses_native_ordered_selectors_without_json(self):
        schema = config_flow._settings_schema(config_flow.DEFAULTS, required=True)
        fields = {marker.key: field for marker, field in schema.fields.items()}

        self.assertNotIn("advanced_settings_json", fields)
        self.assertNotIn("color_scheme", fields)
        self.assertIn("light_color_scheme", fields)
        self.assertIn("dark_color_scheme", fields)
        for field in ("favorites", "excluded_entities", "entity_order_items"):
            config = fields[field].config
            self.assertTrue(config.multiple)
            self.assertTrue(config.reorder)
            self.assertIsInstance(fields[field], selector_stubs.EntitySelector)
        self.assertTrue(fields["room_order"].config.multiple)
        self.assertTrue(fields["room_order"].config.reorder)
        self.assertIsInstance(fields["room_order"], selector_stubs.AreaSelector)
        self.assertIsInstance(fields["increased_contrast"], selector_stubs.BooleanSelector)
        self.assertIsInstance(fields["reduced_motion"], selector_stubs.BooleanSelector)

    def test_new_palette_selectors_expose_only_the_four_supported_choices(self):
        schema = config_flow._settings_schema(config_flow.DEFAULTS, required=True)
        fields = {marker.key: field for marker, field in schema.fields.items()}
        for key in ("light_color_scheme", "dark_color_scheme"):
            with self.subTest(key=key):
                options = fields[key].config.options
                self.assertEqual(
                    {option["value"] for option in options},
                    {"njorun", "graphite", "warm", "porcelain"},
                )

        german = config_flow._settings_schema(
            config_flow.DEFAULTS, required=True, german=True
        )
        fields = {marker.key: field for marker, field in german.fields.items()}
        light_labels = {
            option["value"]: option["label"]
            for option in fields["light_color_scheme"].config.options
        }
        dark_labels = {
            option["value"]: option["label"]
            for option in fields["dark_color_scheme"].config.options
        }
        self.assertEqual(light_labels["porcelain"], "Porzellan")
        self.assertEqual(dark_labels["warm"], "Glut")
        self.assertEqual(dark_labels["porcelain"], "Tinte")

    def test_room_icon_editor_offers_only_supported_icons_and_a_default(self):
        options = config_flow._room_icon_options(german=False)
        self.assertEqual(
            {option["value"] for option in options},
            {"default", "home", "sofa", "kitchen", "bed", "desk", "bath", "garage", "garden"},
        )
        german = config_flow._room_icon_options(german=True)
        labels = {option["value"]: option["label"] for option in german}
        self.assertEqual(labels["default"], "Standard")
        self.assertEqual(labels["garden"], "Garten")

    def test_legacy_json_payload_is_still_importable(self):
        settings = config_flow._collect_settings(
            {"advanced_settings_json": '{"favorites":["light.kitchen"]}'},
            config_flow.DEFAULTS,
        )
        self.assertEqual(settings["favorites"], ["light.kitchen"])
        self.assertEqual(settings["room_order"], [])

    def test_ordered_entities_use_entity_device_and_unassigned_rooms(self):
        config_flow.er.async_get = lambda hass: hass
        config_flow.ar.async_get = lambda hass: hass
        config_flow.dr.async_get = lambda hass: hass
        hass = types.SimpleNamespace(
            entities={
                "light.living_room": types.SimpleNamespace(
                    area_id="living_room", device_id=None
                ),
                "switch.kitchen": types.SimpleNamespace(
                    area_id=None, device_id="kitchen_child"
                ),
                "sensor.kitchen_temperature": types.SimpleNamespace(
                    area_id=None, device_id="kitchen_appliance"
                ),
                "light.unassigned": types.SimpleNamespace(area_id=None, device_id=None),
            },
            devices={
                "kitchen_child": types.SimpleNamespace(
                    area_id=None, via_device_id="kitchen_appliance"
                ),
                "kitchen_appliance": types.SimpleNamespace(
                    area_id="kitchen", via_device_id=None
                ),
            },
            areas={"living_room": object(), "kitchen": object()},
        )
        current = {
            **config_flow.DEFAULTS,
            "entity_order": {"legacy": ["light.removed"]},
        }
        settings = config_flow._collect_settings(
            {
                "entity_order_items": [
                    "switch.kitchen",
                    "sensor.kitchen_temperature",
                    "light.living_room",
                    "light.unassigned",
                ],
                "room_order": ["kitchen", "living_room"],
                "favorites": ["light.living_room"],
                "excluded_entities": [],
            },
            current,
            hass,
        )
        self.assertEqual(
            settings["entity_order"],
            {
                "kitchen": ["switch.kitchen", "sensor.kitchen_temperature"],
                "living_room": ["light.living_room"],
                "unassigned": ["light.unassigned"],
                "legacy": ["light.removed"],
            },
        )
        self.assertEqual(settings["room_order"], ["kitchen", "living_room"])
        self.assertEqual(settings["favorites"], ["light.living_room"])


if __name__ == "__main__":
    unittest.main()
