"""Configuration invariants independent of the Home Assistant installation."""
import json
import importlib.util
from pathlib import Path
import sys
import unittest

SPEC = importlib.util.spec_from_file_location(
    "njorun_model", Path(__file__).resolve().parents[2] / "custom_components/njorun/model.py")
model = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = model
SPEC.loader.exec_module(model)
Configuration = model.Configuration
ConfigurationError = model.ConfigurationError

CAPABILITIES = {"orientations": ["portrait", "landscape"], "brightness": True}


class ProfileTests(unittest.TestCase):
    def setUp(self):
        self.config = Configuration()
        self.config.add_profile("shared", "Shared", {}, 0)
        self.config.add_profile("other", "Other", {"brightness": 40}, 1)
        for tablet_id, profile in (("one", "shared"), ("two", "shared"), ("three", "other")):
            self.config.add_tablet(tablet_id, tablet_id, profile, CAPABILITIES, self.config.revision)

    def test_shared_settings_validation_cases(self):
        fixture = Path(__file__).resolve().parents[2] / "protocol/fixtures/settings-cases.json"
        for case in json.loads(fixture.read_text()):
            with self.subTest(case=case["name"]):
                settings = dict(model.DEFAULTS, **case["patch"])
                if case["valid"]:
                    self.assertEqual(model.validate_settings(settings), settings)
                else:
                    with self.assertRaises(ConfigurationError):
                        model.validate_settings(settings)

    def test_defaults_match_shared_protocol_fixture(self):
        fixture = Path(__file__).resolve().parents[2] / "protocol/fixtures/default-settings.json"
        self.assertEqual(model.DEFAULTS, json.loads(fixture.read_text()))

    def test_profile_change_fans_out_without_claiming_applied_state(self):
        unchanged = self.config.snapshot("three")
        self.config.edit_profile("shared", {"brightness": 55}, self.config.revision)
        self.assertEqual(self.config.snapshot("one").settings["brightness"], 55)
        self.assertEqual(self.config.snapshot("two").settings["brightness"], 55)
        self.assertEqual(self.config.snapshot("three"), unchanged)
        self.assertNotIn("applied_revision", self.config.dump()["tablets"]["one"])

    def test_overrides_survive_profile_edit_and_clear_uses_latest(self):
        self.config.set_overrides("one", {"brightness": 25, "orientation": "landscape"}, self.config.revision)
        self.config.edit_profile("shared", {"brightness": 60, "theme": "dark"}, self.config.revision)
        first = self.config.snapshot("one")
        self.assertEqual(first.settings["brightness"], 25)
        self.assertEqual(first.settings["theme"], "dark")
        self.assertEqual(self.config.snapshot("two").settings["brightness"], 60)
        self.config.set_overrides("one", {}, self.config.revision)
        self.assertEqual(self.config.snapshot("one").settings["brightness"], 60)

    def test_reassignment_preserves_manual_update_override_and_preview_warns_on_clear(self):
        self.config.set_overrides("one", {"update_policy": "manual"}, self.config.revision)
        self.config.assign("one", "other", self.config.revision)
        self.assertEqual(self.config.snapshot("one").settings["update_policy"], "manual")
        before = self.config.dump()
        preview = self.config.preview_assignment("one", "shared", clear=True)
        self.assertTrue(preview["automatic_updates_enabled"])
        self.assertEqual(self.config.dump(), before)
        self.config.assign("one", "shared", self.config.revision, clear=True)
        self.assertEqual(self.config.snapshot("one").settings["update_policy"], "automatic")

    def test_assigned_profile_deletion_blocked_even_without_connections(self):
        before = self.config.dump()
        with self.assertRaisesRegex(ConfigurationError, "assigned"):
            self.config.delete_profile("shared", self.config.revision)
        self.assertEqual(self.config.dump(), before)
        self.config.assign("one", "other", self.config.revision)
        self.config.assign("two", "other", self.config.revision)
        self.config.delete_profile("shared", self.config.revision)
        self.assertNotIn("shared", self.config.profiles)

    def test_stale_revision_and_unsupported_profile_change_leave_every_tablet_untouched(self):
        self.config.add_tablet("portrait", "Portrait", "shared",
                               {"orientations": ["portrait"], "brightness": True}, self.config.revision)
        before = self.config.dump()
        with self.assertRaisesRegex(ConfigurationError, "changed"):
            self.config.edit_profile("shared", {"brightness": 42}, self.config.revision - 1)
        with self.assertRaisesRegex(ConfigurationError, "Orientation"):
            self.config.edit_profile("shared", {"orientation": "landscape"}, self.config.revision)
        self.assertEqual(self.config.dump(), before)

    def test_incompatible_reassignment_is_atomic(self):
        self.config.edit_profile("other", {"orientation": "landscape"}, self.config.revision)
        self.config.add_tablet("portrait", "Portrait", "shared",
                               {"orientations": ["portrait"], "brightness": False}, self.config.revision)
        before = self.config.dump()
        with self.assertRaises(ConfigurationError):
            self.config.assign("portrait", "other", self.config.revision)
        self.assertEqual(self.config.dump(), before)

    def test_roundtrip_preserves_profiles_assignments_overrides_and_revisions(self):
        self.config.set_overrides("two", {"theme": "dark", "update_policy": "manual"}, self.config.revision)
        state = self.config.dump()
        restored = Configuration.restore(state)
        self.assertEqual(restored.dump(), state)
        state["profiles"]["shared"]["settings"]["brightness"] = 1
        self.assertEqual(restored.snapshot("one").settings["brightness"], 80)

    def test_invalid_settings_are_rejected_atomically(self):
        invalid = [{"brightness": True}, {"brightness": 101}, {"idle_timeout": 0},
                   {"theme": "pink"}, {"unknown": 1}, {"update_time": "25:01"},
                   {"update_time": "02:60"}, {"favorites": ["light.a", "light.a"]},
                   {"entity_order": {"room": "not-a-list"}}, {"room_order": [""]}]
        before = self.config.dump()
        for patch in invalid:
            with self.subTest(patch=patch), self.assertRaises(ConfigurationError):
                self.config.edit_profile("shared", patch, self.config.revision)
            self.assertEqual(self.config.dump(), before)

    def test_duplicate_identity_does_not_replace_assignment(self):
        before = self.config.dump()
        with self.assertRaises(ConfigurationError):
            self.config.add_tablet("one", "Replacement", "other", CAPABILITIES, self.config.revision)
        self.assertEqual(self.config.dump(), before)

    def test_removing_tablet_preserves_shared_profile_and_other_tablets(self):
        other = self.config.snapshot("two")
        self.config.remove_tablet("one", self.config.revision)
        self.assertEqual(self.config.snapshot("two"), other)
        self.assertIn("shared", self.config.profiles)

    def test_corrupt_backup_rejected(self):
        for mutation in (lambda data: data.update(version=99),
                         lambda data: data["tablets"]["one"].update(profile_id="missing"),
                         lambda data: data["tablets"]["one"].update(revision=999),
                         lambda data: data["profiles"]["shared"]["settings"].update(brightness=200)):
            data = self.config.dump()
            mutation(data)
            with self.assertRaises(ConfigurationError):
                Configuration.restore(data)


if __name__ == "__main__":
    unittest.main()


class OrientationChoiceTests(unittest.TestCase):
    def test_the_four_rotations_are_settings_and_capabilities_gate_them(self):
        for orientation in ("portrait", "landscape", "portrait_flipped", "landscape_flipped"):
            settings = dict(model.DEFAULTS, orientation=orientation)
            model.validate_settings(settings)
        four = {"orientations": list(model.CHOICES["orientation"]), "brightness": True}
        model.validate_capabilities(four)
        model.validate_effective(
            dict(model.DEFAULTS, orientation="landscape_flipped"), four
        )
        with self.assertRaises(model.ConfigurationError):
            model.validate_effective(
                dict(model.DEFAULTS, orientation="landscape_flipped"), CAPABILITIES
            )
        with self.assertRaises(model.ConfigurationError):
            model.validate_settings(dict(model.DEFAULTS, orientation="diagonal"))

class LogSinkTests(unittest.TestCase):
    def setUp(self):
        self.config = Configuration()
        self.config.add_profile("shared", "Shared", {}, 0)
        for tablet_id in ("one", "two"):
            self.config.add_tablet(tablet_id, tablet_id, "shared", CAPABILITIES, self.config.revision)

    def test_off_by_default_and_every_tablet_gets_the_same_sink(self):
        self.assertEqual(self.config.snapshot("one").log_sink, "")
        self.config.set_log_sink(" https://loki.example.org ", self.config.revision)
        for tablet_id in ("one", "two"):
            self.assertEqual(self.config.snapshot(tablet_id).log_sink, "https://loki.example.org")
            self.assertEqual(self.config.snapshot(tablet_id).revision, self.config.revision)

    def test_setting_the_same_sink_changes_nothing(self):
        revision = self.config.revision
        self.config.set_log_sink("", revision)
        self.assertEqual(self.config.revision, revision)

    def test_a_stale_revision_is_refused(self):
        with self.assertRaises(ConfigurationError):
            self.config.set_log_sink("http://192.168.2.5:3100", self.config.revision - 1)

    def test_only_urls_the_tablet_accepts_are_valid(self):
        for good in ("", "  ", "http://192.168.2.5:3100", "https://logs.example/loki/api/v1/push",
                     "https://[::1]:3100"):
            with self.subTest(good=good):
                self.assertEqual(model.validate_log_sink(good), good.strip())
        for bad in ("off", "ftp://host", "host:3100", "http://", "http://user:pw@host",
                    "http://host/?q=1", "http://host/#x", "http://ho st", "http://h\u00f6st",
                    "https://" + "a" * 250, 5, None):
            with self.subTest(bad=bad), self.assertRaises(ConfigurationError):
                model.validate_log_sink(bad)

    def test_the_sink_is_stored_and_older_state_without_it_still_loads(self):
        self.config.set_log_sink("https://loki.example.org", self.config.revision)
        restored = Configuration.restore(self.config.dump())
        self.assertEqual(restored.log_sink, "https://loki.example.org")
        older = self.config.dump()
        del older["log_sink"]
        self.assertEqual(Configuration.restore(older).log_sink, "")
        older["log_sink"] = "http://user:pw@host"
        with self.assertRaises(ConfigurationError):
            Configuration.restore(older)

    def test_state_stored_before_the_interface_size_existed_loads_with_100_percent(self):
        older = self.config.dump()
        for profile in older["profiles"].values():
            del profile["settings"]["ui_scale"]
        restored = Configuration.restore(older)
        self.assertEqual(restored.snapshot("one").settings["ui_scale"], 100)
        # A value that is stored stays.
        older["profiles"]["shared"]["settings"]["ui_scale"] = 150
        self.assertEqual(Configuration.restore(older).snapshot("one").settings["ui_scale"], 150)
        older["profiles"]["shared"]["settings"]["ui_scale"] = 74
        with self.assertRaises(ConfigurationError):
            Configuration.restore(older)


class OptionsMenuTests(unittest.TestCase):
    def test_one_profile_offers_editing_creating_and_logging(self):
        self.assertEqual(
            model.options_menu(profiles=1), ["edit_profile", "add_profile", "remote_logging"]
        )

    def test_deleting_needs_a_profile_to_keep(self):
        entries = model.options_menu(profiles=2)
        self.assertEqual(
            entries, ["edit_profile", "add_profile", "delete_profile", "remote_logging"]
        )

    def test_network_entries_need_a_tablet_with_network_interfaces(self):
        self.assertNotIn("network_tablet", model.options_menu(profiles=1))
        entries = model.options_menu(profiles=1, network_tablets=1)
        self.assertEqual(
            entries,
            ["edit_profile", "add_profile", "network_profile", "network_tablet", "remote_logging"],
        )

    def test_tablet_tasks_are_not_in_the_options(self):
        entries = model.options_menu(profiles=3)
        for gone in ("approve_tablet", "set_overrides", "clear_overrides", "assign", "remove_tablet"):
            self.assertNotIn(gone, entries)
