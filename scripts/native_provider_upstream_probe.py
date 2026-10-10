#!/usr/bin/env python3
"""Probe an upstream merge in a disposable git worktree without pushing."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
GENERATED_PATH_PARTS = {"generated", "precomputed", "schema", "schemas"}
GENERATED_SUFFIXES = {".zst"}


def classify_conflicts(paths: list[str]) -> tuple[list[str], list[str]]:
    generated: list[str] = []
    source: list[str] = []
    for path in paths:
        parts = set(Path(path).parts)
        if parts & GENERATED_PATH_PARTS or Path(path).suffix in GENERATED_SUFFIXES:
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

    def record(label: str, result: subprocess.CompletedProcess[str]) -> None:
        log.append(f"$ git {label}\n{result.stdout}{result.stderr}")

    base_result = command(repo, ["rev-parse", "HEAD"])
    record("rev-parse HEAD", base_result)
    if base_result.returncode:
        raise RuntimeError("unable to identify probe base commit")
    base = base_result.stdout.strip()

    fetch = subprocess.run(
        ["git", "-C", str(repo), "fetch", "--no-tags", upstream_url, upstream_ref],
        capture_output=True,
        text=True,
        check=False,
    )
    record(f"fetch --no-tags {upstream_url} {upstream_ref}", fetch)
    if fetch.returncode:
        raise RuntimeError("unable to fetch upstream ref")
    upstream_result = command(repo, ["rev-parse", "FETCH_HEAD"])
    record("rev-parse FETCH_HEAD", upstream_result)
    if upstream_result.returncode:
        raise RuntimeError("unable to identify fetched upstream commit")
    upstream = upstream_result.stdout.strip()

    result: dict[str, Any] = {
        "base_sha": base,
        "upstream_sha": upstream,
        "upstream_ref": upstream_ref,
        "merge_status": "not-run",
        "merge_tree": None,
        "focused_checks": "skipped-conflicts",
        "source_conflicts": [],
        "generated_conflicts": [],
        "push_attempted": False,
    }
    with tempfile.TemporaryDirectory(prefix="codex-native-provider-probe-") as temp:
        worktree = Path(temp) / "checkout"
        add = command(repo, ["worktree", "add", "--detach", str(worktree), base])
        record("worktree add --detach <temporary-path> " + base, add)
        if add.returncode:
            raise RuntimeError("unable to create temporary probe worktree")
        merge = command(worktree, ["merge", "--no-commit", "--no-ff", upstream])
        record("merge --no-commit --no-ff " + upstream, merge)
        if merge.returncode == 0:
            tree = command(worktree, ["write-tree"])
            record("write-tree", tree)
            if tree.returncode:
                raise RuntimeError("unable to record clean merge tree")
            result["merge_status"] = "clean"
            result["merge_tree"] = tree.stdout.strip()
            checks_script = worktree / "scripts/native_provider_checks.py"
            checks_dir = output_dir / "focused-checks"
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
                "$ python3 scripts/native_provider_checks.py --artifact-dir "
                f"{checks_dir}\n{checks.stdout}{checks.stderr}"
            )
            result["focused_checks"] = "passed" if checks.returncode == 0 else "failed"
            result["focused_checks_exit_code"] = checks.returncode
        else:
            conflicts_result = command(
                worktree, ["diff", "--name-only", "--diff-filter=U"]
            )
            record("diff --name-only --diff-filter=U", conflicts_result)
            if conflicts_result.returncode:
                raise RuntimeError("unable to list merge conflicts")
            paths = [line for line in conflicts_result.stdout.splitlines() if line]
            source, generated = classify_conflicts(paths)
            result["merge_status"] = "conflicts" if paths else "failed"
            result["source_conflicts"] = source
            result["generated_conflicts"] = generated
            if not paths:
                raise RuntimeError("merge failed without producing conflict paths")
        abort = command(worktree, ["merge", "--abort"])
        record("merge --abort", abort)
        remove = command(repo, ["worktree", "remove", "--force", str(worktree)])
        record("worktree remove --force <temporary-path>", remove)

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
    except (OSError, RuntimeError) as error:
        print(f"upstream merge probe failed: {error}", file=sys.stderr)
        return 2
    print(
        f"upstream merge probe: {result['merge_status']} "
        f"base={result['base_sha']} upstream={result['upstream_sha']}"
    )
    if result["merge_status"] == "clean" and result["focused_checks"] != "passed":
        print("focused checks failed after a clean upstream merge", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
