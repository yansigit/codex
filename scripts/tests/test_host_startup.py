"""Credential-free executable checks for ordinary and hosted startup."""

import http.server
import json
import os
import selectors
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path

BINARY = os.environ.get("NYACODE_TEST_BINARY")


@unittest.skipUnless(BINARY, "requires the candidate CLI and companion binaries")
class HostStartupTests(unittest.TestCase):
    def startup(self, settings, policy=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            if policy is not None:
                (root / "policy.json").write_text(json.dumps(policy))
                settings = {
                    **settings,
                    "CODEX_HOST_TOOL_POLICY": str(root / "policy.json"),
                }
            env = {
                "PATH": os.environ["PATH"],
                "HOME": directory,
                "CODEX_HOME": directory,
                "NYACODE_HOME": directory,
                **settings,
            }
            request = (
                json.dumps(
                    {
                        "id": 1,
                        "method": "initialize",
                        "params": {
                            "clientInfo": {
                                "name": "startup-fixture",
                                "version": "1.0.0",
                            },
                            "capabilities": {"experimentalApi": True},
                        },
                    }
                )
                + "\n"
            )
            command = [BINARY, "app-server", "--stdio"]
            with tempfile.TemporaryFile() as stderr:
                process = subprocess.Popen(
                    command,
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=stderr,
                    env=env,
                )
                output = b""
                try:
                    try:
                        process.stdin.write(request.encode())
                        process.stdin.flush()
                    except BrokenPipeError:
                        # Invalid host settings may terminate before the write.
                        pass
                    # EOF requests shutdown. Keep stdin open until initialization
                    # replies or the process rejects startup, including on busy CI.
                    with selectors.DefaultSelector() as selector:
                        selector.register(process.stdout, selectors.EVENT_READ)
                        deadline = time.monotonic() + 30
                        while time.monotonic() < deadline:
                            if not selector.select(max(0, deadline - time.monotonic())):
                                self.fail("app-server startup did not reply or exit")
                            chunk = os.read(process.stdout.fileno(), 65536)
                            if not chunk:
                                break
                            output += chunk
                            replies = [
                                json.loads(line)
                                for line in output.split(b"\n")[:-1]
                                if line.startswith(b"{")
                            ]
                            if any(reply.get("id") == 1 for reply in replies):
                                break
                    remaining, _ = process.communicate(timeout=30)
                    output += remaining
                    stderr.seek(0)
                    result = subprocess.CompletedProcess(
                        command,
                        process.returncode,
                        output.decode(),
                        stderr.read().decode(),
                    )
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.communicate()
            messages = [
                json.loads(line)
                for line in result.stdout.splitlines()
                if line.startswith("{")
            ]
            return result, any(m.get("id") == 1 and "result" in m for m in messages)

    def test_ordinary_startup_needs_no_host_settings(self):
        result, initialized = self.startup({})
        self.assertTrue(initialized, result.stderr)

    def test_required_partial_malformed_and_relaxed_settings_fail_closed(self):
        cases = [
            ({"CODEX_REQUIRE_HOST_POLICY": "1"}, None),
            ({"CODEX_HOST_MEMORY_MAINTENANCE": "1"}, None),
            (
                {
                    "CODEX_REQUIRE_HOST_POLICY": "0",
                    "CODEX_HOST_CHILD_TOOL_POLICY": "/missing",
                },
                None,
            ),
            ({"CODEX_REQUIRE_HOST_POLICY": "invalid"}, None),
            (
                {"CODEX_REQUIRE_HOST_POLICY": "1"},
                {"version": 2, "allowed_tools": ["*"]},
            ),
        ]
        for settings, policy in cases:
            with self.subTest(settings=settings):
                result, initialized = self.startup(settings, policy)
                self.assertFalse(initialized)
                self.assertNotEqual(result.returncode, 0)

    def test_valid_required_policy_initializes(self):
        result, initialized = self.startup(
            {"CODEX_REQUIRE_HOST_POLICY": "1"}, {"version": 2, "allowed_tools": []}
        )
        self.assertTrue(initialized, result.stderr)

    def test_ordinary_responses_turn_uses_the_configured_native_api(self):
        requests = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                requests.append(
                    (
                        self.path,
                        json.loads(
                            self.rfile.read(int(self.headers["Content-Length"]))
                        ),
                    )
                )
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                for event in [
                    {
                        "type": "response.created",
                        "response": {"id": "fixture-response"},
                    },
                    {
                        "type": "response.output_item.done",
                        "item": {
                            "type": "message",
                            "role": "assistant",
                            "id": "fixture-message",
                            "content": [
                                {
                                    "type": "output_text",
                                    "text": "ordinary responses verified",
                                }
                            ],
                        },
                    },
                    {
                        "type": "response.completed",
                        "response": {
                            "id": "fixture-response",
                            "usage": {
                                "input_tokens": 0,
                                "output_tokens": 0,
                                "total_tokens": 0,
                            },
                        },
                    },
                ]:
                    self.wfile.write(
                        f"event: {event['type']}\ndata: {json.dumps(event)}\n\n".encode()
                    )

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as directory:
                config = f'model="fixture-model"\nmodel_provider="fixture"\n[features]\nmemories=false\nchatgpt_chat=false\ncode_mode=false\n[model_providers.fixture]\nname="OpenAI fixture"\nbase_url="http://127.0.0.1:{server.server_port}/v1"\nwire_api="responses"\nrequires_openai_auth=false\nsupports_websockets=false\nrequest_max_retries=0\nstream_max_retries=0\n'
                (Path(directory) / "config.toml").write_text(config)
                env = {
                    "PATH": os.environ["PATH"],
                    "HOME": directory,
                    "CODEX_HOME": directory,
                    "NYACODE_HOME": directory,
                }
                with tempfile.TemporaryFile() as stderr:
                    process = subprocess.Popen(
                        [BINARY, "app-server", "--stdio"],
                        stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE,
                        stderr=stderr,
                        env=env,
                    )
                    selector = selectors.DefaultSelector()
                    selector.register(process.stdout, selectors.EVENT_READ)
                    received = []
                    buffered = b""

                    def send(value):
                        process.stdin.write((json.dumps(value) + "\n").encode())
                        process.stdin.flush()

                    def until(predicate):
                        nonlocal buffered
                        deadline = time.monotonic() + 30
                        while time.monotonic() < deadline:
                            for message in received:
                                if predicate(message):
                                    return message
                            if not selector.select(max(0, deadline - time.monotonic())):
                                break
                            chunk = os.read(process.stdout.fileno(), 65536)
                            if not chunk:
                                break
                            buffered += chunk
                            while b"\n" in buffered:
                                line, buffered = buffered.split(b"\n", 1)
                                received.append(json.loads(line))
                        stderr.seek(0)
                        self.fail(
                            "ordinary Responses fixture did not complete: "
                            + stderr.read().decode(errors="replace")
                        )

                    try:
                        send(
                            {
                                "id": 1,
                                "method": "initialize",
                                "params": {
                                    "clientInfo": {
                                        "name": "responses-fixture",
                                        "version": "1.0.0",
                                    },
                                    "capabilities": {"experimentalApi": True},
                                },
                            }
                        )
                        self.assertIn("result", until(lambda m: m.get("id") == 1))
                        send({"method": "initialized", "params": {}})
                        send(
                            {
                                "id": 2,
                                "method": "thread/start",
                                "params": {"cwd": directory, "environments": []},
                            }
                        )
                        started = until(lambda m: m.get("id") == 2)
                        self.assertIn("result", started)
                        thread_id = started["result"]["thread"]["id"]
                        send(
                            {
                                "id": 3,
                                "method": "turn/start",
                                "params": {
                                    "threadId": thread_id,
                                    "input": [
                                        {
                                            "type": "text",
                                            "text": "Verify ordinary Responses",
                                            "text_elements": [],
                                        }
                                    ],
                                },
                            }
                        )
                        self.assertIn("result", until(lambda m: m.get("id") == 3))
                        completed = until(lambda m: m.get("method") == "turn/completed")
                        self.assertEqual(
                            completed["params"]["turn"]["status"], "completed"
                        )
                        self.assertTrue(requests)
                        self.assertTrue(
                            all(path.endswith("/responses") for path, body in requests)
                        )
                        self.assertTrue(
                            all(
                                body["model"] == "fixture-model"
                                for path, body in requests
                            )
                        )
                    finally:
                        selector.close()
                        process.stdin.close()
                        try:
                            process.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait()
                        process.stdout.close()
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)
