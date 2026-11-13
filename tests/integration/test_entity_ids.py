"""Entity IDs follow the internal key, whatever language the entity was first named in."""
import importlib.util
from pathlib import Path
import json
import re
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2] / "custom_components/njorun"
SPEC = importlib.util.spec_from_file_location("njorun_entity_ids", ROOT / "entity_ids.py")
ids = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = ids
SPEC.loader.exec_module(ids)

TABLET = "njorun-3fbdd20e737c7b948b9f73d6189bc258"
PREFIX = "d1001_3fbdd2"


def slugify(text):
    """Enough of Home Assistant's slugify for these names."""
    text = text.lower().replace("ö", "o").replace("ß", "ss").replace("ü", "u").replace("ä", "a")
    return re.sub(r"[^a-z0-9]+", "_", text).strip("_")


def canonical(entity_id, key, original_name):
    return ids.canonical_entity_id(entity_id, f"{TABLET}_{key}", TABLET, original_name, slugify)


class CanonicalIdTests(unittest.TestCase):
    def test_a_german_name_becomes_the_key(self):
        self.assertEqual(
            canonical(f"light.{PREFIX}_hintergrundbeleuchtung", "backlight", "Hintergrundbeleuchtung"),
            f"light.{PREFIX}_backlight",
        )
        self.assertEqual(
            canonical(f"select.{PREFIX}_profil", "profile", "Profil"), f"select.{PREFIX}_profile")

    def test_an_english_name_that_differs_from_the_key_is_renamed(self):
        self.assertEqual(
            canonical(f"update.{PREFIX}_firmware", "firmware_update", "Firmware"),
            f"update.{PREFIX}_firmware_update",
        )

    def test_an_id_that_is_already_right_stays(self):
        self.assertIsNone(canonical(f"select.{PREFIX}_orientation", "orientation", "Orientation"))
        self.assertIsNone(
            canonical(f"number.{PREFIX}_idle_brightness", "idle_brightness", "Idle brightness"))

    def test_a_renamed_device_keeps_its_own_prefix(self):
        self.assertEqual(
            canonical("sensor.wohnzimmer_laufzeit", "uptime", "Laufzeit"), "sensor.wohnzimmer_uptime")

    def test_an_id_the_name_does_not_explain_is_left_alone(self):
        self.assertIsNone(canonical(f"sensor.{PREFIX}_custom", "uptime", "Laufzeit"))
        self.assertIsNone(canonical(f"sensor.{PREFIX}_laufzeit", "uptime", None))

    def test_the_registered_ids_of_the_household_tablet(self):
        wanted = {
            "light.d1001_3fbdd2_hintergrundbeleuchtung": ("backlight", "Hintergrundbeleuchtung"),
            "select.d1001_3fbdd2_profil": ("profile", "Profil"),
            "button.d1001_3fbdd2_profilwerte_verwenden": ("use_profile_values", "Profilwerte verwenden"),
            "button.d1001_3fbdd2_jetzt_aufwecken": ("wake", "Jetzt aufwecken"),
            "sensor.d1001_3fbdd2_laufzeit": ("uptime", "Laufzeit"),
            "sensor.d1001_3fbdd2_wlan_signal": ("wifi_signal", "WLAN-Signal"),
            "sensor.d1001_3fbdd2_freier_speicher": ("free_heap", "Freier Speicher"),
            "sensor.d1001_3fbdd2_letzter_neustartgrund": ("reset_reason", "Letzter Neustartgrund"),
            "sensor.d1001_3fbdd2_netzwerkeinstellungen": ("network_settings", "Netzwerkeinstellungen"),
        }
        for entity_id, (key, name) in wanted.items():
            domain = entity_id.split(".")[0]
            self.assertEqual(canonical(entity_id, key, name), f"{domain}.{PREFIX}_{key}")


class SuggestedKeyTests(unittest.TestCase):
    def test_object_key_strips_the_tablet(self):
        self.assertEqual(ids.object_key(f"{TABLET}_ui_scale", TABLET), "ui_scale")
        self.assertEqual(ids.object_key("", TABLET), "")


class TranslationTests(unittest.TestCase):
    def test_no_name_is_hard_coded_over_its_translation(self):
        for module in ("number.py", "select.py"):
            self.assertNotIn("_attr_name", (ROOT / module).read_text(), module)

    def test_every_setting_has_a_group_in_both_languages(self):
        for lang in ("en", "de"):
            names = json.loads((ROOT / f"translations/{lang}.json").read_text(encoding="utf-8"))["entity"]
            for domain, keys in (("select", ["orientation", "theme", "language", "screensaver",
                                             "home_view", "details", "update_policy",
                                             "update_channel"]),
                                 ("number", ["brightness", "idle_brightness", "ui_scale", "idle_timeout"])):
                for key in keys:
                    self.assertIn(" · ", names[domain][key]["name"], f"{lang} {domain} {key}")


if __name__ == "__main__":
    unittest.main()
