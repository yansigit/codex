import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from native_provider_acceptance import AcceptanceError
from native_provider_multiagent_acceptance import (
    MultiagentServer,
    completed_commands,
    fixture_terminal,
    verify_children,
)


class MultiagentAcceptanceTests(unittest.TestCase):
    def setUp(self):
        self.workspace = Path("/tmp/fixture")
        self.expected = [("one", "model-a"), ("two", "model-b")]
        self.children = [
            {
                "modelProvider": provider,
                "model": model,
                "parentThreadId": "parent",
                "cwd": str(self.workspace),
            }
            for provider, model in self.expected
        ]

    def test_approval_is_bound_to_the_child_and_exact_command(self):
        server = MultiagentServer.__new__(MultiagentServer)
        server.expected_approval_thread = "child"
        server.expected_approval_cwd = self.workspace
        server.expected_approval_command = "printf ACCEPTED_WRITE > child-approval.txt"
        params = {
            "threadId": "child",
            "kind": "command",
            "cwd": str(self.workspace),
            "command": "/bin/bash -lc 'printf ACCEPTED_WRITE > child-approval.txt'",
        }
        self.assertTrue(server._is_expected_approval(params))
        for field, value in [
            ("threadId", "parent"),
            ("command", "other"),
            ("cwd", "/other"),
        ]:
            self.assertFalse(server._is_expected_approval({**params, field: value}))

    def test_terminal_termination_requires_the_exact_fixture(self):
        terminal = {
            "command": "python3 acceptance-cancel.py",
            "cwd": str(self.workspace),
            "processId": "42",
        }
        self.assertEqual(
            fixture_terminal([terminal], terminal["command"], self.workspace), "42"
        )
        for terminals in [
            [],
            [terminal, terminal],
            [{**terminal, "command": "other"}],
            [{**terminal, "cwd": "/other"}],
        ]:
            with self.assertRaisesRegex(AcceptanceError, "refusing termination"):
                fixture_terminal(terminals, terminal["command"], self.workspace)

    def test_provider_and_model_must_both_match(self):
        verify_children(self.children, self.expected, "parent", self.workspace)
        self.children[1]["modelProvider"] = "one"
        with self.assertRaisesRegex(AcceptanceError, "routing mismatch"):
            verify_children(self.children, self.expected, "parent", self.workspace)

    def test_child_ownership_and_workspace_are_required(self):
        for field, value in [("parentThreadId", "foreign"), ("cwd", "/other")]:
            with self.subTest(field=field):
                children = [dict(child) for child in self.children]
                children[0][field] = value
                with self.assertRaisesRegex(AcceptanceError, "ownership or workspace"):
                    verify_children(children, self.expected, "parent", self.workspace)

    def test_tool_evidence_requires_persisted_terminal_command_items(self):
        completed = {"type": "commandExecution", "status": "completed", "exitCode": 7}
        thread = {
            "turns": [
                {
                    "items": [
                        {"type": "commandExecution", "status": "inProgress"},
                        {"type": "agentMessage", "text": "I ran the command"},
                        completed,
                    ]
                }
            ]
        }
        self.assertEqual(completed_commands(thread), [completed])


if __name__ == "__main__":
    unittest.main()
