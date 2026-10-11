#!/usr/bin/env python3
"""Opt-in live parent/child acceptance with real providers and bounded fixture tools.

Uses an existing private home; credentials and raw protocol payloads are never
printed. Fixture shell operations target a unique temporary workspace.
No quota failures are injected by this product-level runner.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shlex
import tempfile
import time
from pathlib import Path
from typing import Any

from native_provider_acceptance import AcceptanceError, AppServer


def fixture_command(workspace: Path, name: str, source: str) -> str:
    """Keep model-selected commands simple without weakening execution evidence."""
    script = workspace / f"acceptance-{name}.py"
    script.write_text(source + "\n", encoding="utf-8")
    return "python3 " + shlex.quote(script.name)


def completed_commands(thread: dict[str, Any]) -> list[dict[str, Any]]:
    """Use persisted items; app-server clients need not subscribe to child events."""
    return [
        item
        for turn in thread.get("turns", [])
        for item in turn.get("items", [])
        if item.get("type") == "commandExecution"
        and item.get("status") in {"completed", "failed", "declined"}
    ]


def verify_children(
    children: list[dict[str, Any]],
    expected: list[tuple[str, str]],
    parent: str,
    workspace: Path,
) -> None:
    if len(children) != len(expected):
        raise AcceptanceError("expected exactly two child threads")
    actual = sorted(
        (child.get("modelProvider"), child.get("model")) for child in children
    )
    if actual != sorted(expected):
        raise AcceptanceError("child provider/model routing mismatch")
    if any(
        child.get("parentThreadId") != parent or child.get("cwd") != str(workspace)
        for child in children
    ):
        raise AcceptanceError("child ownership or workspace mismatch")


class MultiagentServer(AppServer):
    expected_approval_thread: str | None = None

    def _is_expected_approval(self, params: dict[str, Any]) -> bool:
        return (
            self.expected_approval_thread is not None
            and params.get("threadId") == self.expected_approval_thread
            and super()._is_expected_approval(params)
        )


def fixture_terminal(terminals: list[dict[str, Any]], cmd: str, workspace: Path) -> str:
    if (
        len(terminals) != 1
        or terminals[0].get("command") != cmd
        or terminals[0].get("cwd") != str(workspace)
    ):
        raise AcceptanceError("unexpected background terminal; refusing termination")
    return terminals[0]["processId"]


def child_lifecycle(
    server: MultiagentServer,
    parent: str,
    children: list[dict[str, Any]],
    workspace: Path,
) -> list[str]:
    """Exercise host approval and cancellation through the parent's collaboration tools."""
    by_name = {
        child["source"]["subAgent"]["thread_spawn"]["agent_path"].rsplit("/", 1)[
            -1
        ]: child["id"]
        for child in children
    }
    target = shlex.quote(str(workspace / "child-approval.txt"))
    for decision, marker in [("decline", "DENIED_WRITE"), ("accept", "ACCEPTED_WRITE")]:
        cmd = f"printf {marker} > {target}"
        server.approval_decision = decision
        server.expected_approval_command = cmd
        server.expected_approval_cwd = workspace
        server.expected_approval_thread = by_name["left"]
        before = server.call(
            "thread/read", {"threadId": by_name["left"], "includeTurns": True}
        )["thread"]
        previous_commands = len(completed_commands(before))
        count = len(server.approvals)
        server.complete_turn(
            parent,
            "Use followup_task on left. Ask it to call exec_command exactly once with "
            f"cmd={json.dumps(cmd)}, workdir={json.dumps(str(workspace))}, "
            "sandbox_permissions=require_escalated, and a short justification for this "
            "temporary acceptance write. It must stop after the result even if denied. "
            "Wait for the child to finish and report the result. Do not run the command yourself.",
        )
        if server.approvals[count:] != [decision]:
            raise AcceptanceError("child did not request exactly one expected approval")
        path = workspace / "child-approval.txt"
        if (decision == "decline" and path.exists()) or (
            decision == "accept" and (not path.is_file() or path.read_text() != marker)
        ):
            raise AcceptanceError("child approval result did not match the fixture")
        child = server.call(
            "thread/read", {"threadId": by_name["left"], "includeTurns": True}
        )["thread"]
        commands = completed_commands(child)[previous_commands:]
        if child["turns"][-1]["status"] != "completed":
            raise AcceptanceError("child approval turn did not finish")
        if decision == "decline":
            # Unified exec refuses before creating a process. It can persist only
            # the tool rejection, without a commandExecution item. Require the
            # exact child-scoped approval RPC, no completed process and no file.
            if any(item["status"] != "declined" for item in commands):
                raise AcceptanceError("declined child command was executed")
        elif (
            len(commands) != 1
            or commands[0]["status"] != "completed"
            or commands[0]["exitCode"] != 0
        ):
            raise AcceptanceError("accepted child approval lacks command evidence")

    cmd = fixture_command(
        workspace,
        "cancel",
        "from pathlib import Path; import time; Path('child-started').write_text('START'); time.sleep(45); Path('child-finished').write_text('UNEXPECTED')",
    )
    server.complete_turn(
        parent,
        "Use followup_task on right, asking it to execute exactly once with exec_command "
        f"cmd={json.dumps(cmd)}, workdir={json.dumps(str(workspace))}, yield_time_ms=1000. "
        "The child should poll the returned command session until completion. "
        "After sending the task, return STARTED immediately without waiting for the child. "
        "Do not execute its command yourself.",
    )
    deadline = time.monotonic() + 60
    while not (workspace / "child-started").exists():
        if time.monotonic() > deadline:
            raise AcceptanceError("child cancellation fixture did not start")
        time.sleep(0.1)
    observed_start = time.monotonic()
    server.complete_turn(
        parent,
        "Call interrupt_agent on right now. Report the result. Do not send it another task, "
        "restart it, or execute shell commands.",
    )
    child = server.call(
        "thread/read", {"threadId": by_name["right"], "includeTurns": True}
    )["thread"]
    if not any(turn.get("status") == "interrupted" for turn in child.get("turns", [])):
        raise AcceptanceError("child interruption was not recorded")
    # Resumable exec_command terminals deliberately outlive turn interruption.
    # Stop this exact child-owned terminal through the existing host operation.
    terminals = server.call(
        "thread/backgroundTerminals/list", {"threadId": by_name["right"]}
    )
    if terminals.get("nextCursor"):
        raise AcceptanceError("unexpected additional background terminals")
    process_id = fixture_terminal(terminals["data"], cmd, workspace)
    stopped = server.call(
        "thread/backgroundTerminals/terminate",
        {"threadId": by_name["right"], "processId": process_id},
    )
    if not stopped.get("terminated"):
        raise AcceptanceError("child background terminal was not terminated")
    if server.call("thread/backgroundTerminals/list", {"threadId": by_name["right"]})[
        "data"
    ]:
        raise AcceptanceError("child background terminal is still running")
    while time.monotonic() < observed_start + 46:
        if (workspace / "child-finished").exists():
            raise AcceptanceError(
                "terminated child command completed its delayed write"
            )
        time.sleep(0.1)
    parent_data = server.call(
        "thread/read", {"threadId": parent, "includeTurns": True}
    )["thread"]
    if len(completed_commands(parent_data)) != 1:
        raise AcceptanceError("parent executed a child lifecycle command")
    return [
        "child_approval_decline",
        "child_approval_accept",
        "child_turn_interruption",
        "child_background_terminal_termination",
    ]


def run(args: argparse.Namespace) -> dict[str, Any]:
    workspace = Path(tempfile.mkdtemp(prefix="nyacode-multiagent-"))
    (workspace / "input.json").write_text("[2, 3, 5, 11]\n", encoding="utf-8")
    left_one = fixture_command(
        workspace,
        "left_one",
        "from pathlib import Path; import json; "
        "v=json.loads(Path('input.json').read_text()); "
        "Path('left-intermediate.txt').write_text(str(sum(v))); "
        "print('INTERMEDIATE',sum(v))",
    )
    left_two = fixture_command(
        workspace,
        "left_two",
        "from pathlib import Path; "
        "v=int(Path('left-intermediate.txt').read_text()); "
        "Path('left.txt').write_text(str(v*2)); "
        "Path('left-audit.txt').open('a').write('INITIAL\\n'); print(v*2)",
    )
    right_one = fixture_command(
        workspace, "right_one", "print('EXPECTED_FAILURE'); raise SystemExit(7)"
    )
    right_two = fixture_command(
        workspace,
        "right_two",
        "from pathlib import Path; import json; "
        "v=json.loads(Path('input.json').read_text()); "
        "Path('right.txt').write_text(str(sum(v))); "
        "Path('right-audit.txt').open('a').write('RECOVERED\\n'); print(sum(v))",
    )
    followup = fixture_command(
        workspace,
        "followup",
        "from pathlib import Path; assert Path('left.txt').read_text()=='42'; "
        "Path('left-audit.txt').open('a').write('FOLLOWUP\\n'); print('FOLLOWUP_DONE')",
    )
    aggregate = fixture_command(
        workspace,
        "aggregate",
        "from pathlib import Path; "
        "assert Path('left.txt').read_text()=='42'; "
        "assert Path('right.txt').read_text()=='21'; "
        "assert Path('left-audit.txt').read_text()=='INITIAL\\nFOLLOWUP\\n'; "
        "assert Path('right-audit.txt').read_text()=='RECOVERED\\n'; "
        "Path('parent.txt').write_text('63'); print('ACCEPTED_63')",
    )
    child_specs = []
    for name, route, commands in [
        ("left", args.child[0], [left_one, left_two]),
        ("right", args.child[1], [right_one, right_two]),
    ]:
        provider, model = route
        task = (
            f"Work in {workspace}. Do not spawn agents. Execute these two commands "
            "as separate exec_command calls in exactly this order. Wait for the first "
            "command result before requesting the second; do not issue both in one "
            "response or in parallel. Continue after either "
            "exit 0 or the deliberate exit 7. Use workdir set to this workspace. "
            "Do not inspect or change the fixture scripts or input.json. Do not retry "
            "either command or run any other command. Report both results. Commands: "
            + json.dumps(commands)
        )
        child_specs.append(
            {
                "task_name": name,
                "model_provider": provider,
                "model": model,
                "fork_turns": "none",
                "message": task,
            }
        )
    prompt = (
        "Run this multiagent acceptance check exactly. You are explicitly authorized "
        "to select the requested providers/models. Spawn exactly two fresh children "
        "using spawn_agent with these arguments: " + json.dumps(child_specs) + ". "
        "Do not add reasoning_effort or agent_type; each selected model uses its own defaults. "
        "Launch both before waiting for results. The children must run their own tools; "
        "do not execute their commands yourself. The input.json and acceptance scripts "
        "already exist; do not modify them or run exploratory commands. Wait for both "
        "to finish. A wait_agent "
        "return is only a mailbox wake-up, not evidence both children finished. Use "
        "list_agents and require both left and right to have completed status before "
        "sending any follow-up. If either is still running, keep waiting. Then use "
        "followup_task on left, asking it to execute exactly once: "
        + json.dumps(followup)
        + ". Wait and check list_agents until left's follow-up is completed. Finally use exec_command (with this workspace as workdir) to run this command yourself once: "
        + json.dumps(aggregate)
        + ". Reply ACCEPTED_63 only after it succeeds. "
        "After successful aggregation, late worker messages are acknowledgements only; do not repeat any command. "
        "If spawning or a required step fails, report the failure and stop. "
        "Use wait_agent timeout_ms=10000 when waiting. Do not close the children."
    )
    server = MultiagentServer(args.codex, args.codex_home, args.provider)
    parent = None
    try:
        server.initialize()
        result = server.call(
            "thread/start",
            {
                "modelProvider": args.provider,
                "model": args.model,
                "cwd": str(workspace),
                "approvalPolicy": "on-request" if args.lifecycle else "never",
                "approvalsReviewer": "user",
                "sandbox": "workspace-write",
                "baseInstructions": "Follow the user's acceptance check exactly. Use the requested tools and models. Be concise.",
                "config": {
                    "features.multi_agent": True,
                    "features.multi_agent_v2": True,
                    "features.apps": args.with_apps,
                    "features.plugins": args.with_apps,
                    "agents.max_threads": 4,
                    "agents.max_depth": 2,
                },
            },
        )
        parent = result.get("thread", {}).get("id")
        if not isinstance(parent, str):
            raise AcceptanceError("parent thread did not start")
        turn = server.turn_start(parent, prompt)
        completed = server.wait_turn_event(parent, turn, args.timeout)
        outcome = completed.get("params", {}).get("turn", {})
        if outcome.get("status") != "completed":
            error = outcome.get("error") or {}
            # Native transports sanitize their diagnostics before emitting this event.
            detail = str(error.get("message", "no diagnostic"))[:600]
            raise AcceptanceError(f"parent turn {outcome.get('status')}: {detail}")
        listing = server.call(
            "thread/list",
            {
                "ancestorThreadId": parent,
                "modelProviders": [],
                "sourceKinds": ["subAgentThreadSpawn"],
                "limit": 10,
            },
        )
        if listing.get("nextCursor"):
            raise AcceptanceError("unexpected additional child threads")
        children = [
            server.call("thread/read", {"threadId": child["id"], "includeTurns": True})[
                "thread"
            ]
            for child in listing["data"]
        ]
        verify_children(children, args.child, parent, workspace)
        for child in children:
            for turn in child.get("turns", []):
                if turn.get("status") != "completed":
                    detail = str(
                        (turn.get("error") or {}).get("message", "no diagnostic")
                    )[:600]
                    raise AcceptanceError(
                        f"child {child['modelProvider']}/{child['model']}: {detail}"
                    )
        expected_files = {
            "input.json": "[2, 3, 5, 11]\n",
            "left.txt": "42",
            "right.txt": "21",
            "parent.txt": "63",
            "left-audit.txt": "INITIAL\nFOLLOWUP\n",
            "right-audit.txt": "RECOVERED\n",
        }
        for name, expected in expected_files.items():
            if (
                not (workspace / name).is_file()
                or (workspace / name).read_text() != expected
            ):
                raise AcceptanceError(
                    "fixture output or exactly-once audit mismatch: " + name
                )
        child_commands = [completed_commands(child) for child in children]
        if sorted(map(len, child_commands)) != [2, 3]:
            raise AcceptanceError(
                "child command counts did not match chained tools and follow-up"
            )
        if (
            sum(item.get("exitCode") == 7 for items in child_commands for item in items)
            != 1
        ):
            raise AcceptanceError(
                "deliberate child tool failure was not recorded exactly once"
            )
        parent_data = server.call(
            "thread/read", {"threadId": parent, "includeTurns": True}
        )["thread"]
        if len(completed_commands(parent_data)) != 1:
            raise AcceptanceError("parent executed unexpected commands")
        accounts = []
        for thread in [parent_data, *children]:
            pool = server.call(
                "provider/accounts/list",
                {"provider": thread["modelProvider"], "threadId": thread["id"]},
            )
            if (
                not pool["enabled"]
                or pool["configuredToken"]
                or not pool["effectiveAccount"]
            ):
                raise AcceptanceError("thread did not use a managed account pool")
            accounts.append(
                {
                    "provider": thread["modelProvider"],
                    "model": thread["model"],
                    "account": pool["effectiveAccount"],
                }
            )
        lifecycle_checks = (
            child_lifecycle(server, parent, children, workspace)
            if args.lifecycle
            else []
        )
        return {
            "result": "passed",
            "apps_enabled": args.with_apps,
            "parent_provider": args.provider,
            "parent_model": args.model,
            "children": [
                {"provider": c["modelProvider"], "model": c["model"]} for c in children
            ],
            "accounts": accounts,
            "checks": [
                "provider_model_routing",
                "fresh_children",
                "workspace_inheritance",
                "chained_tools",
                "failed_tool_recovery",
                "followup_task",
                "parent_aggregation",
                "exactly_once_fixture_writes",
            ]
            + lifecycle_checks,
            "workspace": str(workspace),
            "thread": parent,
        }
    except AcceptanceError as error:
        return {
            "result": "failed",
            "apps_enabled": args.with_apps,
            "parent_provider": args.provider,
            "parent_model": args.model,
            "reason": str(error),
            "workspace": str(workspace),
            "thread": parent,
        }
    finally:
        server.close()


def route(value: str) -> tuple[str, str]:
    provider, separator, model = value.partition(":")
    if not separator or not provider or not model:
        raise argparse.ArgumentTypeError("use PROVIDER:MODEL")
    return provider, model


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--codex-home", type=Path, required=True)
    parser.add_argument("--provider", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--child", type=route, action="append", required=True)
    parser.add_argument("--timeout", type=int, default=300)
    parser.add_argument(
        "--with-apps",
        action="store_true",
        help="also advertise configured connected apps and plugins; requires compatible schemas",
    )
    parser.add_argument(
        "--lifecycle",
        action="store_true",
        help="also test child approvals, turn interruption, and explicit terminal termination",
    )
    parser.add_argument("--receipt", type=Path, required=True)
    args = parser.parse_args()
    if len(args.child) != 2:
        parser.error("provide exactly two --child routes")
    if not args.codex.is_absolute() or not args.codex.is_file():
        parser.error("--codex must name an existing absolute executable path")
    if not args.codex_home.is_absolute() or not args.codex_home.is_dir():
        parser.error("--codex-home must name an existing absolute directory")
    with args.codex.open("rb") as executable:
        binary_digest = hashlib.sha256()
        for chunk in iter(lambda: executable.read(1024 * 1024), b""):
            binary_digest.update(chunk)
        binary_sha256 = binary_digest.hexdigest()
    try:
        receipt = run(args)
    except AcceptanceError as error:
        receipt = {
            "result": "failed",
            "parent_provider": args.provider,
            "parent_model": args.model,
            "reason": str(error),
        }
    except Exception as error:  # noqa: BLE001
        # Do not print unexpected payloads from an external process.
        receipt = {
            "result": "failed",
            "parent_provider": args.provider,
            "parent_model": args.model,
            "reason": type(error).__name__,
        }
    receipt["binary_sha256"] = binary_sha256
    args.receipt.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(args.receipt, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as output:
        json.dump(receipt, output, indent=2)
        output.write("\n")
    print(json.dumps(receipt))
    return 0 if receipt["result"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
