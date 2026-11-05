"""Counter-probes for the native-build gate."""
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from plan import main, needs_host


class PlannerTests(unittest.TestCase):
    def test_documentation_only(self):
        self.assertFalse(needs_host(["README.md", "docs/plans/001.md", "LICENSE"]))

    def test_executable_change_mixed_with_docs(self):
        self.assertTrue(needs_host(["README.md", "crates/pairing/src/lib.rs"]))

    def test_dependencies_and_build_configuration(self):
        for path in ["Cargo.lock", "Cargo.toml", "rust-toolchain.toml", "deny.toml",
                     ".github/workflows/ci.yml", "scripts/ci/install-tools.py"]:
            with self.subTest(path=path):
                self.assertTrue(needs_host([path]))

    def test_unknown_input_fails_safe(self):
        self.assertTrue(needs_host(["new-component/input.dat"]))


class PlannerCliTests(unittest.TestCase):
    def run_plan(self, base, result):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            args = ["plan.py", "--base", base, "--output", str(output)]
            with patch("sys.argv", args), patch("plan.subprocess.run", return_value=result):
                main()
            return output.read_text()

    def test_missing_base_requires_host(self):
        self.assertEqual(self.run_plan("", None), "host=true\n")

    def test_unavailable_base_requires_host(self):
        result = subprocess.CompletedProcess([], 128, b"", b"unknown revision")
        self.assertEqual(self.run_plan("missing", result), "host=true\n")

    def test_docs_diff_skips_host(self):
        result = subprocess.CompletedProcess([], 0, b"README.md\0docs/plan.md\0", b"")
        self.assertEqual(self.run_plan("base", result), "host=false\n")


if __name__ == "__main__":
    unittest.main()
