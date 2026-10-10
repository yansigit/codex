#!/usr/bin/env python3
"""Check native-provider crate ownership and upstream dependency boundaries."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "docs/native-providers/integration-manifest.json"
CARGO_MANIFEST = ROOT / "codex-rs/Cargo.toml"
REQUIRED_HOOK_PATHS = {
    "model-info-registry-and-legacy-enum": "codex-rs/model-provider-info/src/lib.rs",
    "remote-thread-config-proto": "codex-rs/config/src/thread_config/remote.rs",
    "optional-model-provider-facade": "codex-rs/model-provider/src/native.rs",
    "core-provider-bridge": "codex-rs/core/src/native_provider.rs",
    "client-stream-and-auth-routing": "codex-rs/core/src/client.rs",
    "client-session-control": "codex-rs/core/src/client_common.rs",
    "turn-recorded-completion-retry-terminal": "codex-rs/core/src/session/turn.rs",
    "parallel-structured-completion-capture": "codex-rs/core/src/tools/parallel.rs",
    "thread-scoped-model-metadata": "codex-rs/core/src/thread_manager.rs",
    "generic-account-operations-and-ownership-lock": "codex-rs/app-server/src/request_processors/account_processor.rs",
    "thread-provider-inheritance": "codex-rs/app-server/src/request_processors/thread_processor.rs",
    "generic-cli-metadata-choices": "codex-rs/cli/src/provider_cmd.rs",
}

def provider_branch_patterns(provider_ids: list[str], provider_symbols: list[str]):
    id_pattern = "(?:" + "|".join(re.escape(value) for value in provider_ids) + ")"
    symbol_pattern = "(?:" + "|".join(re.escape(value) for value in provider_symbols) + ")"
    return (
        re.compile(
            r"\b(?:if|else\s+if|match)\b[^;{}\n]*\b(?:provider|wire_api|backend)\b"
            + r"[^;{}\n]*\b"
            + id_pattern
            + r"\b",
            re.IGNORECASE,
        ),
        re.compile(r"\b" + symbol_pattern + r"\b"),
    )


def load_metadata(metadata_path: Path | None) -> dict[str, Any]:
    if metadata_path is not None:
        return json.loads(metadata_path.read_text())
    result = subprocess.run(
        [
            "cargo",
            "metadata",
            "--manifest-path",
            str(CARGO_MANIFEST),
            "--format-version",
            "1",
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def validate(
    metadata: dict[str, Any], manifest: dict[str, Any], root: Path = ROOT
) -> list[str]:
    errors: list[str] = []
    if manifest.get("schema_version") != 1:
        return ["integration manifest has an unsupported schema_version"]

    subsystem = manifest.get("subsystem_packages", [])
    adapter = manifest.get("adapter_package")
    if not isinstance(adapter, str) or not adapter:
        errors.append("integration manifest must name one adapter_package")
    if not subsystem or len(subsystem) != len(set(subsystem)):
        errors.append("subsystem_packages must be a non-empty list without duplicates")
    if adapter in subsystem:
        errors.append("adapter_package must not also appear in subsystem_packages")
    packages = metadata.get("packages", [])
    by_name: dict[str, list[dict[str, Any]]] = {}
    for package in packages:
        by_name.setdefault(package["name"], []).append(package)

    owned_root = (root / manifest["owned_root"]).resolve()
    workspace_member_ids = set(metadata.get("workspace_members", []))
    workspace_packages = [
        package for package in packages if package.get("id") in workspace_member_ids
    ]
    workspace_by_name = {package["name"]: package for package in workspace_packages}
    expected_owned = set(subsystem) | ({adapter} if isinstance(adapter, str) else set())
    actual_owned: set[str] = set()
    for package in packages:
        manifest_path = Path(package["manifest_path"]).resolve()
        try:
            manifest_path.relative_to(owned_root)
        except ValueError:
            continue
        actual_owned.add(package["name"])

    for name in sorted(expected_owned - set(by_name)):
        errors.append(f"manifest package is missing from Cargo metadata: {name}")
    for name in sorted(set(by_name) - expected_owned):
        if any(
            Path(package["manifest_path"]).resolve().is_relative_to(owned_root)
            for package in by_name[name]
        ):
            errors.append(f"unmanifested package under owned provider root: {name}")
    for name in sorted(expected_owned - actual_owned):
        if name in by_name:
            errors.append(f"provider package is outside owned root {owned_root}: {name}")

    for name in sorted(set(subsystem) & set(by_name)):
        for package in by_name[name]:
            for dependency in package["dependencies"]:
                workspace_dependency = workspace_by_name.get(dependency["name"])
                dependency_path = dependency.get("path")
                if workspace_dependency is not None:
                    dependency_path = workspace_dependency["manifest_path"]
                if dependency["name"] == "codex-core":
                    errors.append(f"subsystem package {name} must not depend on codex-core")
                if not dependency_path:
                    continue
                resolved_dependency_path = Path(dependency_path).resolve()
                try:
                    resolved_dependency_path.relative_to(owned_root)
                except ValueError:
                    errors.append(
                        f"subsystem package {name} directly depends on upstream workspace "
                        f"package {dependency['name']} at {resolved_dependency_path}"
                    )

    branch_scan = manifest.get("provider_branch_scan", {})
    scan_paths = branch_scan.get("paths", [])
    compatibility_paths = set(branch_scan.get("allowed_compatibility_paths", []))
    provider_ids = branch_scan.get("provider_ids", [])
    provider_symbols = branch_scan.get("provider_symbols", [])
    if not scan_paths or not provider_ids or not provider_symbols:
        errors.append(
            "provider_branch_scan must name scan paths, provider_ids, and provider_symbols"
        )
    branch_patterns = (
        provider_branch_patterns(provider_ids, provider_symbols)
        if provider_ids and provider_symbols
        else ()
    )
    for scan_path in scan_paths:
        if scan_path in compatibility_paths:
            continue
        path = root / scan_path
        if not path.is_file():
            errors.append(f"provider branch scan path is missing: {scan_path}")
            continue
        source = path.read_text()
        if any(pattern.search(source) for pattern in branch_patterns):
            errors.append(f"provider-specific execution branch found outside registry: {scan_path}")

    hook_ids: set[str] = set()
    for hook in manifest.get("integration_hooks", []):
        hook_id = hook.get("id")
        hook_path = hook.get("path")
        if not hook_id or hook_id in hook_ids:
            errors.append(f"integration hook has a missing or duplicate id: {hook_id!r}")
        hook_ids.add(hook_id)
        if not isinstance(hook_path, str) or not (root / hook_path).is_file():
            errors.append(f"integration hook path is missing: {hook_path!r}")
        if not hook.get("purpose"):
            errors.append(f"integration hook {hook_id!r} is missing its purpose")
    missing_hooks = sorted(set(REQUIRED_HOOK_PATHS) - hook_ids)
    if missing_hooks:
        errors.append(
            "integration manifest is missing required hook(s): "
            + ", ".join(missing_hooks)
        )
    for hook in manifest.get("integration_hooks", []):
        hook_id = hook.get("id")
        expected_path = REQUIRED_HOOK_PATHS.get(hook_id)
        if expected_path and hook.get("path") != expected_path:
            errors.append(
                f"integration hook {hook_id!r} must remain at {expected_path}"
            )

    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--metadata-json", type=Path, help="use saved Cargo metadata")
    parser.add_argument("--manifest", type=Path, default=MANIFEST)
    parser.add_argument(
        "--list-test-packages",
        action="store_true",
        help="print manifested provider package names one per line",
    )
    args = parser.parse_args()

    manifest = json.loads(args.manifest.read_text())
    if args.list_test_packages:
        for package in manifest["subsystem_packages"] + [manifest["adapter_package"]]:
            print(package)
        return 0

    try:
        metadata = load_metadata(args.metadata_json)
    except (OSError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        print(f"unable to load Cargo metadata: {error}", file=sys.stderr)
        return 2
    errors = validate(metadata, manifest, ROOT)
    if errors:
        print("native provider architecture check failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print("native provider package ownership and dependency boundaries are valid")
    print(f"checked {len(manifest['subsystem_packages'])} subsystem packages and adapter")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
