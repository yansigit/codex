"""Run-order regressions for the native provider CI driver."""

import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import native_provider_checks as checks


class NativeProviderChecksTest(unittest.TestCase):
    def test_fresh_checkout_builds_cli_before_integration_tests_spawn_it(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            manifest = root / "docs/native-providers/integration-manifest.json"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(
                json.dumps(
                    {
                        "subsystem_packages": [],
                        "adapter_package": "adapter",
                        "single_provider_features": [],
                        "focused_test_groups": [
                            {
                                "name": "core-integration",
                                "packages": ["codex-core"],
                                "test_target": "all",
                                "minimum_tests": 1,
                            }
                        ],
                    }
                )
            )
            helper = root / "target/debug/codex"

            def run(command, **kwargs):
                if command[:2] == ["git", "rev-parse"]:
                    return subprocess.CompletedProcess(command, 0, "a" * 40)
                log = kwargs["stdout"]
                if command[:2] == ["cargo", "build"]:
                    self.assertIn("codex-cli", command)
                    self.assertIn("codex", command)
                    self.assertIn("nyacode", command)
                    helper.parent.mkdir(parents=True)
                    helper.touch()
                if command[:2] == ["cargo", "test"]:
                    if "codex-core" in command and not helper.is_file():
                        log.write("CARGO_BIN_EXE_codex is unset\n")
                        return subprocess.CompletedProcess(command, 101)
                    log.write("test result: ok. 1 passed; 0 failed;\n")
                return subprocess.CompletedProcess(command, 0)

            with (
                patch.object(checks.subprocess, "run", side_effect=run),
                patch.dict(checks.os.environ, {}, clear=True),
                contextlib.redirect_stdout(io.StringIO()),
            ):
                stages = checks.run_checks(root, root / "logs")

            self.assertTrue(helper.is_file())
            self.assertTrue(all(stage["exit_code"] == 0 for stage in stages))
            summary = json.loads((root / "logs/summary.json").read_text())
            self.assertEqual(summary["result"], "passed")


if __name__ == "__main__":
    unittest.main()
