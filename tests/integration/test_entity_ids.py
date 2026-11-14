"""Entity IDs follow the internal key, not the translated name."""
import importlib.util
from pathlib import Path
import json
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2] / "custom_components/njorun"
SPEC = importlib.util.spec_from_file_location("njorun_entity_ids", ROOT / "entity_ids.py")
ids = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = ids
SPEC.loader.exec_module(ids)

TABLET = "njorun-3fbdd20e737c7b948b9f73d6189bc258"


class SuggestedKeyTests(unittest.TestCase):
    def test_object_key_strips_the_tablet(self):
        self.assertEqual(ids.object_key(f"{TABLET}_ui_scale", TABLET), "ui_scale")
        self.assertEqual(ids.object_key("", TABLET), "")


class TranslationTests(unittest.TestCase):
    def test_no_name_is_hard_coded_over_its_translation(self):
        for module in ("number.py", "select.py", "switch.py"):
            self.assertNotIn("_attr_name", (ROOT / module).read_text(), module)

    def test_every_setting_has_a_group_in_both_languages(self):
        for lang in ("en", "de"):
            names = json.loads((ROOT / f"translations/{lang}.json").read_text(encoding="utf-8"))["entity"]
            for domain, keys in (("select", ["orientation", "theme", "language", "screensaver",
                                             "home_view", "details", "update_policy",
                                             "update_channel", "light_color_scheme",
                                             "dark_color_scheme"]),
                                 ("number", ["brightness", "idle_brightness", "ui_scale", "idle_timeout",
                                             "camera_motion_sensitivity"]),
                                 ("switch", ["increased_contrast", "reduced_motion",
                                             "camera_motion_enabled"])):
                for key in keys:
                    self.assertIn(" · ", names[domain][key]["name"], f"{lang} {domain} {key}")


if __name__ == "__main__":
    unittest.main()
