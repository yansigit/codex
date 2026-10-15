#!/usr/bin/env python3
"""Probe an upstream merge in a disposable git worktree without pushing."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
GENERATED_PATH_PARTS = {"generated", "precomputed", "schema", "schemas"}
GENERATED_SUFFIXES = {".gz", ".zst"}
GENERATED_SUFFIX_CHAINS = {".schema.json"}
GENERATED_CONFIG_PROTO_OUTPUT = "codex.thread_config.v1.rs"


def classify_conflicts(paths: list[str]) -> tuple[list[str], list[str]]:
    generated: list[str] = []
    source: list[str] = []
    for path in paths:
        parts = set(Path(path).parts)
        path_obj = Path(path)
        if path_obj.suffix == ".proto":
            source.append(path)
        elif (
            parts & GENERATED_PATH_PARTS
            or path_obj.suffix in GENERATED_SUFFIXES
            or any(path.endswith(suffix) for suffix in GENERATED_SUFFIX_CHAINS)
            or (
                path_obj.name == GENERATED_CONFIG_PROTO_OUTPUT
                and "proto" in parts
                and "thread_config" in parts
            )
        ):
            generated.append(path)
        else:
            source.append(path)
    return source, generated


def command(repo: Path, args: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        text=True,
        check=False,
    )


def run_probe(
    repo: Path, upstream_url: str, upstream_ref: str, output_dir: Path
) -> dict[str, Any]:
    output_dir.mkdir(parents=True, exist_ok=True)
    log: list[str] = []
    started = time.monotonic()
    last_stage = started
    durations = {}

    def record(label: str, result: subprocess.CompletedProcess[str]) -> None:
        nonlocal last_stage
        now = time.monotonic()
        durations[label] = round(now - last_stage, 3)
        last_stage = now
        log.append(
            f"$ git {label} (exit {result.returncode})\n{result.stdout}{result.stderr}"
        )

    result: dict[str, Any] = {
        "base_sha": None,
        "upstream_sha": None,
        "upstream_ref": upstream_ref,
        "merge_status": "not-run",
        "merge_tree": None,
        "focused_checks": "not-run",
        "source_conflicts": [],
        "dependency_conflicts": [],
        "merge_bases": [],
        "affected_manifest_groups": [],
        "gates": {
            name: "not-run"
            for name in (
                "mergeability",
                "compilation",
                "deterministic_tests",
                "protocol",
                "live_acceptance",
                "deployment",
            )
        },
        "generated_conflicts": [],
        "push_attempted": False,
    }
    worktree: Path | None = None
    worktree_added = False
    try:
        base_result = command(repo, ["rev-parse", "HEAD"])
        record("rev-parse HEAD", base_result)
        if base_result.returncode:
            result["merge_status"] = "base-identification-failed"
            result["error"] = "unable to identify probe base commit"
        else:
            base = base_result.stdout.strip()
            result["base_sha"] = base
            fetch = subprocess.run(
                [
                    "git",
                    "-C",
                    str(repo),
                    "fetch",
                    "--no-tags",
                    upstream_url,
                    upstream_ref,
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            record(f"fetch --no-tags {upstream_url} {upstream_ref}", fetch)
            if fetch.returncode:
                result["merge_status"] = "fetch-failed"
                result["error"] = f"upstream fetch exited {fetch.returncode}"
            else:
                upstream_result = command(repo, ["rev-parse", "FETCH_HEAD"])
                record("rev-parse FETCH_HEAD", upstream_result)
                if upstream_result.returncode:
                    result["merge_status"] = "upstream-identification-failed"
                    result["error"] = "unable to identify fetched upstream commit"
                else:
                    upstream = upstream_result.stdout.strip()
                    result["upstream_sha"] = upstream
                    bases = command(repo, ["merge-base", "--all", base, upstream])
                    record("merge-base --all", bases)
                    result["merge_bases"] = bases.stdout.splitlines()
                    changed = command(repo, ["diff", "--name-only", base, upstream])
                    record("diff --name-only inputs", changed)
                    manifest_path = (
                        repo / "docs/native-providers/downstream-manifest.json"
                    )
                    if manifest_path.exists():
                        groups = json.loads(manifest_path.read_text())["groups"]
                        result["affected_manifest_groups"] = [
                            g["id"]
                            for g in groups
                            if any(
                                path == owned
                                or path.startswith(owned.rstrip("/") + "/")
                                for path in changed.stdout.splitlines()
                                for owned in g["owned_paths"] + g["upstream_hooks"]
                            )
                        ]
                    with tempfile.TemporaryDirectory(
                        prefix="codex-native-provider-probe-"
                    ) as temp:
                        worktree = Path(temp) / "checkout"
                        add = command(
                            repo, ["worktree", "add", "--detach", str(worktree), base]
                        )
                        record("worktree add --detach <temporary-path> " + base, add)
                        if add.returncode:
                            result["merge_status"] = "worktree-add-failed"
                            result["error"] = f"worktree add exited {add.returncode}"
                        else:
                            worktree_added = True
                            merge = command(
                                worktree,
                                [
                                    "-c",
                                    "user.name=Native provider merge probe",
                                    "-c",
                                    "user.email=merge-probe@example.invalid",
                                    "merge",
                                    "--no-commit",
                                    "--no-ff",
                                    upstream,
                                ],
                            )
                            record("merge --no-commit --no-ff " + upstream, merge)
                            if merge.returncode == 0:
                                tree = command(worktree, ["write-tree"])
                                record("write-tree", tree)
                                if tree.returncode:
                                    result["merge_status"] = "tree-write-failed"
                                    result["error"] = (
                                        "unable to record clean merge tree"
                                    )
                                else:
                                    result["merge_status"] = "clean"
                                    result["merge_tree"] = tree.stdout.strip()
                                    checks_script = (
                                        worktree / "scripts/native_provider_checks.py"
                                    )
                                    checks_dir = output_dir / "focused-checks"
                                    try:
                                        checks = subprocess.run(
                                            [
                                                sys.executable,
                                                str(checks_script),
                                                "--artifact-dir",
                                                str(checks_dir),
                                            ],
                                            cwd=worktree,
                                            capture_output=True,
                                            text=True,
                                            check=False,
                                        )
                                        log.append(
                                            "$ python3 scripts/native_provider_checks.py "
                                            f"(exit {checks.returncode})\n"
                                            f"{checks.stdout}{checks.stderr}"
                                        )
                                        result["focused_checks"] = (
                                            "passed"
                                            if checks.returncode == 0
                                            else "failed"
                                        )
                                        result["focused_checks_exit_code"] = (
                                            checks.returncode
                                        )
                                    except OSError as error:
                                        result["focused_checks"] = "failed-to-start"
                                        result["error"] = str(error)
                            else:
                                conflicts_result = command(
                                    worktree,
                                    ["diff", "--name-only", "--diff-filter=U"],
                                )
                                record(
                                    "diff --name-only --diff-filter=U",
                                    conflicts_result,
                                )
                                paths = [
                                    line
                                    for line in conflicts_result.stdout.splitlines()
                                    if line
                                ]
                                source, generated = classify_conflicts(paths)
                                result["merge_status"] = (
                                    "conflicts" if paths else "merge-failed"
                                )
                                dependencies = [
                                    p
                                    for p in source
                                    if Path(p).name
                                    in {
                                        "Cargo.lock",
                                        "Cargo.toml",
                                        "deny.toml",
                                        "MODULE.bazel",
                                        "MODULE.bazel.lock",
                                    }
                                ]
                                result["dependency_conflicts"] = dependencies
                                result["source_conflicts"] = [
                                    p for p in source if p not in dependencies
                                ]
                                result["generated_conflicts"] = generated
                                if not paths:
                                    result["error"] = (
                                        "merge failed without producing conflict paths"
                                    )
                        abort = command(worktree, ["merge", "--abort"])
                        record("merge --abort", abort)
                        remove = command(
                            repo, ["worktree", "remove", "--force", str(worktree)]
                        )
                        record("worktree remove --force <temporary-path>", remove)
                        result["worktree_cleanup"] = (
                            "passed" if remove.returncode == 0 else "failed"
                        )
                        if remove.returncode:
                            result["error"] = "worktree removal failed"
                        worktree_added = remove.returncode != 0
    except (OSError, subprocess.SubprocessError) as error:
        result["merge_status"] = "probe-error"
        result["error"] = str(error)
        log.append(f"probe exception: {type(error).__name__}: {error}")
    finally:
        if worktree_added and worktree is not None:
            abort = command(worktree, ["merge", "--abort"])
            record("merge --abort (cleanup)", abort)
            remove = command(repo, ["worktree", "remove", "--force", str(worktree)])
            record("worktree remove --force <temporary-path> (cleanup)", remove)
            result["worktree_cleanup"] = (
                "passed" if remove.returncode == 0 else "failed"
            )
            if result["worktree_cleanup"] != "passed":
                result["error"] = result.get("error") or "worktree cleanup failed"

    result["stage_durations_seconds"] = durations
    result["duration_seconds"] = round(time.monotonic() - started, 3)
    result["gates"]["mergeability"] = (
        "passed" if result["merge_status"] == "clean" else "failed"
    )
    # A clean textual merge is not compilation, protocol or live acceptance.
    checks_summary = output_dir / "focused-checks/summary.json"
    if checks_summary.exists():
        stages = json.loads(checks_summary.read_text()).get("stages", [])
        builds = [s for s in stages if s["name"] == "cli-build"]
        if builds:
            result["gates"]["compilation"] = (
                "passed" if all(s["exit_code"] == 0 for s in builds) else "failed"
            )
        result["gates"]["deterministic_tests"] = result["focused_checks"]
    result["outstanding_gates"] = [
        k for k, v in result["gates"].items() if v != "passed"
    ]
    (output_dir / "probe.json").write_text(json.dumps(result, indent=2) + "\n")
    (output_dir / "source-conflicts.txt").write_text(
        "\n".join(result["source_conflicts"]) + "\n"
    )
    (output_dir / "generated-conflicts.txt").write_text(
        "\n".join(result["generated_conflicts"]) + "\n"
    )
    (output_dir / "probe.log").write_text("\n".join(log))
    hashes = {
        name: hashlib.sha256((output_dir / name).read_bytes()).hexdigest()
        for name in [
            "probe.json",
            "probe.log",
            "source-conflicts.txt",
            "generated-conflicts.txt",
        ]
    }
    (output_dir / "artifact-sha256.json").write_text(
        json.dumps(hashes, indent=2) + "\n"
    )
    github_summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if github_summary:
        with Path(github_summary).open("a") as summary_file:
            summary_file.write(
                "## Native provider upstream probe\n\n"
                f"- Result: **{result['merge_status']}**\n"
                f"- Base commit: {result.get('base_sha') or 'unavailable'}\n"
                f"- Upstream commit: {result.get('upstream_sha') or 'unavailable'}\n"
                f"- Merge tree: {result.get('merge_tree') or 'unavailable'}\n"
                f"- Focused checks: **{result['focused_checks']}**\n"
                f"- Source conflicts: {len(result['source_conflicts'])}\n"
                f"- Generated conflicts: {len(result['generated_conflicts'])}\n"
                f"- Error: {result.get('error', 'none')}\n"
            )
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=ROOT)
    parser.add_argument("--upstream-url", required=True)
    parser.add_argument("--upstream-ref", default="main")
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    try:
        result = run_probe(
            args.repo.resolve(), args.upstream_url, args.upstream_ref, args.output_dir
        )
    except OSError as error:
        print(f"unable to write upstream probe artifacts: {error}", file=sys.stderr)
        return 2
    print(
        f"upstream merge probe: {result['merge_status']} "
        f"base={result['base_sha']} upstream={result['upstream_sha']}"
    )
    if result["merge_status"] == "clean" and result["focused_checks"] != "passed":
        print("focused checks failed after a clean upstream merge", file=sys.stderr)
        return 1
    if result["merge_status"] != "clean":
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
