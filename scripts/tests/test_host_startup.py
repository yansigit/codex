"""Credential-free executable checks for ordinary and hosted startup."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

BINARY = os.environ.get("NYACODE_TEST_BINARY")


@unittest.skipUnless(BINARY, "requires the candidate CLI and companion binaries")
class HostStartupTests(unittest.TestCase):
    def startup(self, settings, policy=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            if policy is not None:
                (root / "policy.json").write_text(json.dumps(policy))
                settings = {**settings, "CODEX_HOST_TOOL_POLICY": str(root / "policy.json")}
            env = {"PATH": os.environ["PATH"], "HOME": directory, "CODEX_HOME": directory,
                   "NYACODE_HOME": directory, **settings}
            request = json.dumps({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "startup-fixture", "version": "1.0.0"}, "capabilities": {"experimentalApi": True}}}) + "\n"
            result = subprocess.run([BINARY, "app-server", "--stdio"], input=request, text=True,
                                    capture_output=True, env=env, timeout=30)
            messages = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
            return result, any(m.get("id") == 1 and "result" in m for m in messages)

    def test_ordinary_startup_needs_no_host_settings(self):
        result, initialized = self.startup({})
        self.assertTrue(initialized, result.stderr)

    def test_required_partial_malformed_and_relaxed_settings_fail_closed(self):
        cases = [({"CODEX_REQUIRE_HOST_POLICY": "1"}, None),
                 ({"CODEX_HOST_MEMORY_MAINTENANCE": "1"}, None),
                 ({"CODEX_REQUIRE_HOST_POLICY": "0", "CODEX_HOST_CHILD_TOOL_POLICY": "/missing"}, None),
                 ({"CODEX_REQUIRE_HOST_POLICY": "invalid"}, None),
                 ({"CODEX_REQUIRE_HOST_POLICY": "1"}, {"version": 2, "allowed_tools": ["*"]})]
        for settings, policy in cases:
            with self.subTest(settings=settings):
                result, initialized = self.startup(settings, policy)
                self.assertFalse(initialized)
                self.assertNotEqual(result.returncode, 0)

    def test_valid_required_policy_initializes(self):
        result, initialized = self.startup({"CODEX_REQUIRE_HOST_POLICY": "1"}, {"version": 2, "allowed_tools": []})
        self.assertTrue(initialized, result.stderr)
