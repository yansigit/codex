#!/usr/bin/env bash
# Run Nyacode's Rust verification inside a Ubuntu 24.04 container.
#
#   scripts/verify-container.sh fast
#   scripts/verify-container.sh
#
# Docker and Podman both work. The host home directory is not mounted.
# See docs/container-verify.md.

set -euo pipefail

IMAGE_TAG="nyacode-verify:rust-1.95"
VOLUME_REGISTRY="nyacode-verify-cargo-registry"
VOLUME_GIT="nyacode-verify-cargo-git"
VOLUME_TARGET="nyacode-verify-target"
VOLUME_SCCACHE="nyacode-verify-sccache"
CONTAINER_HOME="/home/verifier"
CONTAINER_SRC="/src"
CONTAINER_TARGET="${CONTAINER_SRC}/codex-rs/target"

usage() {
    cat <<'EOF'
Usage: scripts/verify-container.sh [options] [full|fast|run -- command]

  full    fmt, clippy, debug build, nextest, and nyacode --version/--help (default)
  fast    fmt, then clippy and nextest for crates changed since origin/main
  run     run a command in the verification container

Options:
  --engine docker|podman   container engine (default: the first one that responds)
  --reset-cache            delete Cargo, target, and sccache volumes first
  --no-cache               rebuild the image without layer cache
  -h, --help               show this help
EOF
}

if [[ "${NYACODE_VERIFY_INNER:-}" == "1" ]]; then
    ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
else
    ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
fi

refuse_if_home_is_mounted() {
    local target
    target="$(findmnt -n -T "$HOME" -o TARGET 2>/dev/null || true)"
    if [[ "$target" == "$HOME" ]]; then
        echo "refusing to run: $HOME is a mount point. Sandbox tests bind-mount the home directory, and a nested host-home mount crashes that run." >&2
        exit 1
    fi
}

configure_inner_env() {
    export HOME="${HOME:-$CONTAINER_HOME}"
    export CARGO_HOME="${CARGO_HOME:-/usr/local/cargo}"
    export RUSTUP_HOME="${RUSTUP_HOME:-/usr/local/rustup}"
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$CONTAINER_TARGET}"
    export SCCACHE_DIR="${SCCACHE_DIR:-/var/cache/sccache}"
    export RUSTC_WRAPPER="${RUSTC_WRAPPER:-sccache}"
    export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
    export CARGO_NET_GIT_FETCH_WITH_CLI="${CARGO_NET_GIT_FETCH_WITH_CLI:-true}"
    export RUST_MIN_STACK="${RUST_MIN_STACK:-67108864}"
    export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"
    export TMPDIR="${TMPDIR:-/tmp}"
    mkdir -p "$HOME" "$CARGO_TARGET_DIR" "$SCCACHE_DIR" "$TMPDIR"
    if ! git config --global --get-all safe.directory 2>/dev/null | grep -qx "$ROOT"; then
        git config --global --add safe.directory "$ROOT"
    fi
    ulimit -s 65536 || echo "warning: could not raise the stack ulimit (current soft limit: $(ulimit -s) KiB)" >&2
    if [[ -z "${CARGO_BUILD_JOBS:-}" ]]; then
        local mem_kb jobs cpus
        mem_kb="$(awk '/MemAvailable:/ {print $2}' /proc/meminfo)"
        jobs=$((mem_kb / 2500000))
        cpus="$(nproc)"
        if ((jobs < 1)); then
            jobs=1
        fi
        if ((jobs > cpus)); then
            jobs=$cpus
        fi
        export CARGO_BUILD_JOBS="$jobs"
    fi
    if [[ "$RUSTC_WRAPPER" == "sccache" ]]; then
        sccache --start-server
        sccache --zero-stats >/dev/null
    fi
}

read_verify_metadata() {
    python3 - "$ROOT/codex-rs/Cargo.toml" <<'PY'
import sys
import tomllib
from pathlib import Path

manifest = tomllib.loads(Path(sys.argv[1]).read_text())
config = manifest["workspace"]["metadata"]["nyacode"]["container-verify"]
excludes = config["exclude"]
if not excludes:
    raise SystemExit("container-verify.exclude is empty")
test_filter = " ".join(config.get("test-filter", "").split())
print("\n".join(excludes))
print("---")
print(test_filter)
PY
}

load_excludes_and_filter() {
    local raw excludes_text
    raw="$(read_verify_metadata)"
    excludes_text="${raw%%---*}"
    TEST_FILTER="${raw#*---}"
    TEST_FILTER="${TEST_FILTER#$'\n'}"
    EXCLUDES=()
    local name
    while IFS= read -r name; do
        if [[ -n "$name" ]]; then
            EXCLUDES+=("$name")
        fi
    done <<<"$excludes_text"
    EXCLUDE_ARGS=()
    for name in "${EXCLUDES[@]}"; do
        EXCLUDE_ARGS+=(--exclude "$name")
    done
}

affected_packages() {
    python3 - "$ROOT" <<'PY'
import subprocess
import sys
import tomllib
from pathlib import Path

root = Path(sys.argv[1])
workspace = root / "codex-rs"
manifest = tomllib.loads((workspace / "Cargo.toml").read_text())
excludes = set(manifest["workspace"]["metadata"]["nyacode"]["container-verify"]["exclude"])

def git(*args: str) -> list[str]:
    result = subprocess.run(
        ["git", *args],
        cwd=root,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        return []
    return [line for line in result.stdout.splitlines() if line]

base = ""
for ref in ("origin/main", "main"):
    probe = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", ref],
        cwd=root,
        check=False,
        capture_output=True,
        text=True,
    )
    if probe.returncode == 0:
        merge = subprocess.run(
            ["git", "merge-base", "HEAD", ref],
            cwd=root,
            check=False,
            capture_output=True,
            text=True,
        )
        if merge.returncode == 0:
            base = merge.stdout.strip()
            break

changed = set(git("diff", "--name-only", "--no-renames", base or "HEAD"))
changed.update(git("diff", "--name-only", "--cached", "--no-renames"))
changed.update(git("ls-files", "--others", "--exclude-standard"))

workspace_markers = {
    "codex-rs/Cargo.toml",
    "codex-rs/Cargo.lock",
    "codex-rs/rust-toolchain.toml",
    "codex-rs/rustfmt.toml",
    "codex-rs/.cargo/config.toml",
    "codex-rs/.config/nextest.toml",
}
if changed & workspace_markers:
    print(
        "workspace-level files changed; fast mode does not rebuild every crate. Run full for the workspace.",
        file=sys.stderr,
    )

def package_at(path: Path) -> str | None:
    current = path if path.is_dir() else path.parent
    while current != workspace and workspace in current.parents:
        cargo_toml = current / "Cargo.toml"
        if cargo_toml.is_file():
            data = tomllib.loads(cargo_toml.read_text())
            name = data.get("package", {}).get("name")
            if isinstance(name, str) and name not in excludes:
                return name
            return None
        parent = current.parent
        if parent == current:
            break
        current = parent
    return None

packages: set[str] = set()
for relative in sorted(changed):
    name = package_at(root / relative)
    if name:
        packages.add(name)
for name in sorted(packages):
    print(name)
PY
}

phase() {
    local name="$1"
    shift
    local start end elapsed
    start="$(date +%s)"
    echo "==> ${name}"
    "$@"
    end="$(date +%s)"
    elapsed="$((end - start))"
    echo "==> ${name} done in ${elapsed}s"
    printf '%s\t%s\n' "$name" "$elapsed" >>"$TIMING_LOG"
}

run_fmt() {
    cargo fmt -- --config imports_granularity=Item --check
}

run_clippy() {
    cargo clippy --workspace --tests "${EXCLUDE_ARGS[@]}" -- -D warnings
}

run_build() {
    cargo build --workspace --bins --tests "${EXCLUDE_ARGS[@]}"
}

run_tests() {
    local -a args=(nextest run --workspace --no-fail-fast "${EXCLUDE_ARGS[@]}")
    if [[ -n "$TEST_FILTER" ]]; then
        args+=(-E "$TEST_FILTER")
    fi
    cargo "${args[@]}"
}

run_cli_smoke() {
    local cli="${CARGO_TARGET_DIR}/debug/nyacode"
    if [[ ! -x "$cli" ]]; then
        echo "nyacode binary not found at $cli" >&2
        exit 1
    fi
    "$cli" --version
    "$cli" --help >/dev/null
}

print_timing_summary() {
    echo "==> timings"
    if [[ -s "$TIMING_LOG" ]]; then
        awk -F '\t' '{printf "  %-12s %ss\n", $1, $2}' "$TIMING_LOG"
    fi
    if [[ "${RUSTC_WRAPPER:-}" == "sccache" ]]; then
        echo "==> sccache"
        sccache --show-stats || true
    fi
}

run_inner() {
    local mode="${1:-full}"
    configure_inner_env
    refuse_if_home_is_mounted
    load_excludes_and_filter
    TIMING_LOG="$(mktemp)"
    trap 'status=$?; if ((status != 0)); then print_timing_summary || true; fi' EXIT
    echo "rustc: $(rustc --version)"
    echo "cargo: $(command -v cargo) ($(cargo --version))"
    echo "HOME=${HOME} (mount target: $(findmnt -n -T "$HOME" -o TARGET))"
    echo "CARGO_TARGET_DIR=${CARGO_TARGET_DIR}"
    echo "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}"
    echo "RUST_MIN_STACK=${RUST_MIN_STACK} ulimit_stack_kib=$(ulimit -s)"
    echo "excluded: ${EXCLUDES[*]}"
    cd "$ROOT/codex-rs"

    case "$mode" in
        full)
            phase fmt run_fmt
            phase clippy run_clippy
            phase build run_build
            phase test run_tests
            phase smoke run_cli_smoke
            ;;
        fast)
            phase fmt run_fmt
            local packages=()
            local package
            while IFS= read -r package; do
                if [[ -n "$package" ]]; then
                    packages+=("$package")
                fi
            done < <(affected_packages)
            if ((${#packages[@]} == 0)); then
                echo "==> no affected crates; skipping clippy and tests"
            else
                echo "==> affected: ${packages[*]}"
                local -a package_args=()
                for package in "${packages[@]}"; do
                    package_args+=(-p "$package")
                done
                phase clippy cargo clippy --tests "${package_args[@]}" -- -D warnings
                local -a test_args=(nextest run --no-fail-fast --no-tests=pass "${package_args[@]}")
                if [[ -n "$TEST_FILTER" ]]; then
                    test_args+=(-E "$TEST_FILTER")
                fi
                phase test cargo "${test_args[@]}"
            fi
            ;;
        run)
            shift
            if [[ "${1:-}" == "--" ]]; then
                shift
            fi
            if (($# == 0)); then
                echo "run requires a command" >&2
                exit 2
            fi
            phase run "$@"
            ;;
        *)
            echo "unknown mode: $mode" >&2
            usage >&2
            exit 2
            ;;
    esac
    print_timing_summary
}

engine_works() {
    "$@" info >/dev/null 2>&1
}

select_engine() {
    local requested="${1:-}"
    local candidate
    ENGINE=()
    if [[ -n "$requested" ]]; then
        if engine_works "$requested"; then
            ENGINE=("$requested")
        elif engine_works sudo "$requested"; then
            ENGINE=(sudo "$requested")
        else
            echo "container engine '$requested' is not available" >&2
            exit 1
        fi
        return
    fi
    for candidate in docker podman; do
        if engine_works "$candidate"; then
            ENGINE=("$candidate")
            return
        fi
        if engine_works sudo "$candidate"; then
            ENGINE=(sudo "$candidate")
            return
        fi
    done
    echo "docker or podman is required" >&2
    exit 1
}

assert_mount_is_not_host_home() {
    local spec="$1"
    local source="${spec%%:*}"
    local home
    if [[ "$source" != /* ]]; then
        return
    fi
    home="$(CDPATH= cd -- "$HOME" && pwd)"
    if [[ "$source" == "$home" || "$source" == "$home"/* ]]; then
        echo "refusing to mount host home path $source" >&2
        exit 1
    fi
}

run_outer() {
    local engine_name=""
    local reset_cache=0
    local no_cache=0
    local -a mode_args=()
    while (($#)); do
        case "$1" in
            --engine)
                engine_name="${2:-}"
                if [[ -z "$engine_name" ]]; then
                    echo "--engine requires docker or podman" >&2
                    exit 2
                fi
                shift 2
                ;;
            --reset-cache)
                reset_cache=1
                shift
                ;;
            --no-cache)
                no_cache=1
                shift
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            --)
                mode_args+=("$@")
                break
                ;;
            *)
                mode_args+=("$1")
                shift
                ;;
        esac
    done
    if ((${#mode_args[@]} == 0)); then
        mode_args=(full)
    fi

    select_engine "$engine_name"
    local engine_bin="${ENGINE[-1]}"
    echo "engine: ${ENGINE[*]}"

    local -a build_args=(build --network=host -f "$ROOT/Containerfile" -t "$IMAGE_TAG")
    if ((no_cache)); then
        build_args+=(--no-cache)
    fi
    build_args+=("$ROOT")
    local build_start build_end
    build_start="$(date +%s)"
    "${ENGINE[@]}" "${build_args[@]}"
    build_end="$(date +%s)"
    echo "==> image build done in $((build_end - build_start))s"

    if ((reset_cache)); then
        "${ENGINE[@]}" volume rm -f "$VOLUME_REGISTRY" "$VOLUME_GIT" "$VOLUME_TARGET" "$VOLUME_SCCACHE" >/dev/null
    fi

    local -a mounts=(
        "${ROOT}:${CONTAINER_SRC}"
        "${VOLUME_REGISTRY}:/usr/local/cargo/registry"
        "${VOLUME_GIT}:/usr/local/cargo/git"
        "${VOLUME_TARGET}:${CONTAINER_TARGET}"
        "${VOLUME_SCCACHE}:/var/cache/sccache"
    )
    local spec
    for spec in "${mounts[@]}"; do
        assert_mount_is_not_host_home "$spec"
    done

    local -a run_args=(
        run --rm
        --network=host
        -e NYACODE_VERIFY_INNER=1
        -e HOME="$CONTAINER_HOME"
        -e CARGO_HOME=/usr/local/cargo
        -e RUSTUP_HOME=/usr/local/rustup
        -e CARGO_TARGET_DIR="$CONTAINER_TARGET"
        -e SCCACHE_DIR=/var/cache/sccache
        -e RUSTC_WRAPPER=sccache
        -e CARGO_INCREMENTAL=0
        -e CARGO_NET_GIT_FETCH_WITH_CLI=true
        -e CARGO_TERM_COLOR=always
        -e RUST_MIN_STACK=67108864
        -e RUST_BACKTRACE=1
        -e TMPDIR=/tmp
        -w "$CONTAINER_SRC"
        --ulimit "stack=67108864:67108864"
        --cap-add SYS_ADMIN
        --security-opt seccomp=unconfined
    )
    if [[ -n "${CARGO_BUILD_JOBS:-}" ]]; then
        run_args+=(-e "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}")
    fi
    if [[ "$engine_bin" == "podman" ]]; then
        run_args+=(--security-opt label=disable)
    elif [[ -d /sys/kernel/security/apparmor ]]; then
        run_args+=(--security-opt apparmor=unconfined)
    fi
    for spec in "${mounts[@]}"; do
        run_args+=(-v "$spec")
    done
    run_args+=("$IMAGE_TAG" "${CONTAINER_SRC}/scripts/verify-container.sh" "${mode_args[@]}")

    local run_start run_end
    run_start="$(date +%s)"
    "${ENGINE[@]}" "${run_args[@]}"
    run_end="$(date +%s)"
    echo "==> container run done in $((run_end - run_start))s"
}

if [[ "${NYACODE_VERIFY_INNER:-}" == "1" ]]; then
    run_inner "$@"
else
    run_outer "$@"
fi
