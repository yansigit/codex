#!/usr/bin/env python3
"""Run the focused native-provider validation suite and save every stage log."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
def run_checks(root: Path, artifact_dir: Path) -> list[dict[str, Any]]:
    manifest = json.loads((root / "docs/native-providers/integration-manifest.json").read_text())
    artifact_dir.mkdir(parents=True, exist_ok=True)
    summary_path = artifact_dir / "summary.json"
    stages: list[dict[str, Any]] = []
    env = os.environ.copy()
    env["CARGO_TERM_COLOR"] = "never"

    def run_stage(name: str, command: list[str]) -> None:
        print(f"[native-provider-checks] starting {name}", flush=True)
        started = time.monotonic()
        log_path = artifact_dir / f"{len(stages) + 1:02d}-{name}.log"
        with log_path.open("w") as log:
            result = subprocess.run(
                command,
                cwd=root,
                env=env,
                stdout=log,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
            )
        stage = {
            "name": name,
            "command": command,
            "exit_code": result.returncode,
            "duration_seconds": round(time.monotonic() - started, 3),
            "log": log_path.name,
        }
        stages.append(stage)
        summary_path.write_text(json.dumps({"stages": stages}, indent=2) + "\n")
        state = "passed" if result.returncode == 0 else f"failed ({result.returncode})"
        print(f"[native-provider-checks] {state}: {name} ({stage['duration_seconds']}s)", flush=True)

    run_stage(
        "architecture",
        [sys.executable, "scripts/native_provider_architecture.py"],
    )
    run_stage(
        "python-controls",
        [
            sys.executable,
            "-m",
            "unittest",
            "discover",
            "-s",
            "scripts/tests",
            "-p",
            "test_native_provider_*.py",
        ],
    )

    for group in manifest["focused_test_groups"]:
        command = ["cargo", "test", "--manifest-path", "codex-rs/Cargo.toml"]
        for package in group["packages"]:
            command.extend(["-p", package])
        if "test_target" in group:
            command.extend(["--test", group["test_target"]])
        if "filter" in group:
            command.append(group["filter"])
        run_stage(group["name"], command)

    run_stage(
        "cli-build",
        [
            "cargo",
            "build",
            "--manifest-path",
            "codex-rs/Cargo.toml",
            "-p",
            "codex-cli",
        ],
    )

    adapter = manifest["adapter_package"]
    feature_commands = [
        ("adapter-no-default-features", ["--no-default-features"]),
        *[
            (f"adapter-{feature}", ["--no-default-features", "--features", feature])
            for feature in manifest["single_provider_features"]
        ],
        ("adapter-all-features", ["--all-features"]),
    ]
    for name, feature_args in feature_commands:
        run_stage(
            name,
            [
                "cargo",
                "check",
                "--manifest-path",
                "codex-rs/Cargo.toml",
                "-p",
                adapter,
                *feature_args,
            ],
        )

    failed = [stage["name"] for stage in stages if stage["exit_code"] != 0]
    summary = {
        "result": "failed" if failed else "passed",
        "failed_stages": failed,
        "stages": stages,
    }
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")
    artifact_hashes = {
        path.name: hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(artifact_dir.glob("*.log")) + [summary_path]
    }
    (artifact_dir / "artifact-sha256.json").write_text(
        json.dumps(artifact_hashes, indent=2) + "\n"
    )
    return stages


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=ROOT)
    parser.add_argument("--artifact-dir", type=Path, required=True)
    args = parser.parse_args()
    stages = run_checks(args.repo.resolve(), args.artifact_dir.resolve())
    failures = [stage for stage in stages if stage["exit_code"] != 0]
    if failures:
        print(
            "native provider checks failed: " + ", ".join(stage["name"] for stage in failures),
            file=sys.stderr,
        )
        return 1
    print(f"native provider checks passed ({len(stages)} logged stages)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
