#!/usr/bin/env python3
"""Build Nyacode and its Code Mode helper using the pinned V8 artifacts."""

import argparse
import os
from pathlib import Path
import subprocess


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--release", action="store_true", help="Build optimized binaries"
    )
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    os.environ["CODEX_REPO_ROOT"] = str(root)
    # Reuse the package builder's archive/bindings checksum validation.
    from codex_package.targets import TARGET_SPECS
    from codex_package.v8 import resolve_codex_v8_cargo_env

    rustc_version = subprocess.check_output(
        ["rustc", "-vV"], cwd=root / "codex-rs", text=True
    )
    host = next(
        line.removeprefix("host: ")
        for line in rustc_version.splitlines()
        if line.startswith("host: ")
    )
    target = os.environ.get("CARGO_BUILD_TARGET", host)
    if target not in TARGET_SPECS:
        parser.error(f"No pinned V8 package configuration for {target}")
    env = os.environ.copy()
    env.update(resolve_codex_v8_cargo_env(TARGET_SPECS[target]))
    command = [
        "cargo",
        "build",
        "--locked",
        "-p",
        "codex-cli",
        "-p",
        "codex-code-mode-host",
        "--bin",
        "nyacode",
        "--bin",
        "codex",
        "--bin",
        "codex-code-mode-host",
    ]
    if args.release:
        command.append("--release")
    return subprocess.run(
        command, cwd=root / "codex-rs", env=env, check=False
    ).returncode


if __name__ == "__main__":
    raise SystemExit(main())
