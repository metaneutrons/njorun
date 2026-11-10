"""Counter-probes for the OTA image size gate."""
import importlib.util
import pathlib
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "check_image_size", pathlib.Path(__file__).with_name("check-image-size.py"))
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

TABLE = """# comment
nvs, data, nvs, 0x9000, 0x6000,
ota_0, app, ota_0, 0x20000, 0x800000,
ota_1, app, ota_1, 0x820000, 0x700000,
storage, data, fat, 0x1020000, 0xfe0000,
"""


class BudgetTests(unittest.TestCase):
    def table(self, text):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = pathlib.Path(directory.name) / "p.csv"
        path.write_text(text)
        return path

    def test_budget_is_smallest_ota_slot(self):
        budget, sizes = check.slot_budget(self.table(TABLE))
        self.assertEqual(budget, 0x700000)
        self.assertEqual(sizes, {"ota_0": 0x800000, "ota_1": 0x700000})

    def test_missing_slots_fail(self):
        with self.assertRaises(SystemExit):
            check.slot_budget(self.table("nvs, data, nvs, 0x9000, 0x6000,\n"))

    def test_repository_table_parses(self):
        budget, _ = check.slot_budget(check.ROOT / check.DEFAULT_TABLE)
        self.assertEqual(budget, 0x800000)

    def test_limits(self):
        self.assertTrue(check.evaluate(100, 100)[0])
        self.assertFalse(check.evaluate(101, 100)[0])
        self.assertEqual(check.evaluate(90, 100)[1], 10)


if __name__ == "__main__":
    unittest.main()
