# Container verification

Run the Rust workspace checks in a cached Ubuntu 26.04 container on a Linux host:

```sh
scripts/verify-container.sh fast
scripts/verify-container.sh full
```

Docker and Podman use the same `Containerfile`; `Dockerfile` is a compatibility symlink. The image includes Rust **1.95.0**, cargo-nextest **0.9.148**, sccache **0.18.0**, and the native build dependencies. Release tool downloads have pinned SHA-256 checksums for x86-64 and ARM64. Ubuntu 26.04 [provides GStreamer 1.28](https://packages.ubuntu.com/resolute/libgstreamer1.0-dev), so voice and Code Mode remain included.

`full` verifies pinned V8 inputs, checks Rust formatting and workspace Clippy with `-D warnings`, builds workspace binaries and tests, runs nextest, and checks `nyacode --version` and `--help`. Binaries such as `codex`, `codex-code-mode-host`, and `test_stdio_server` are built before tests need them. Cargo resolution uses `--locked`; there are no verifier-specific crate exclusions or test filters. Existing repository nextest settings still apply.

`fast` selects packages changed since the merge base with `origin/main` (or local `main`) plus their transitive workspace consumers, including uncommitted and untracked changes. Shared manifests, lockfiles, toolchain/configuration changes, V8 inputs, and verifier changes select the whole workspace. Package discovery failures stop the run. Documentation-only changes may run just formatting, with an explicit message that no Rust packages changed. Fast verification also builds required helpers before running tests.

## Filesystem and sandbox tests

Only the repository and cache volumes are mounted. A checkout beneath the host home directory is supported; mounting the entire home directory or one of its ancestors is rejected. Linked worktrees also mount their common Git metadata read-only. The container home `/home/verifier` stays inside the disposable container, independent of the host home.

The verifier runs with the calling user's UID/GID so it does not create root-owned files in the checkout. Podman uses `--userns keep-id`. Nested sandbox tests require the container's `SYS_ADMIN` capability and relaxed seccomp/AppArmor settings; use this verifier with trusted repository code. Engine probing never opens a password prompt: direct Docker/Podman access or existing noninteractive sudo access is required.

The container sets a 64 MiB Rust thread stack and stack ulimit for large debug test futures. It disables incremental builds and debug symbols to keep workspace verification within hosted runner disk limits. Ordinary local and release build settings are unchanged.

## Caches

Named volumes retain Cargo registry/git data, build outputs, sccache objects, and verified V8 archives. The sccache server uses a socket inside the container home, independent of any host cache server. V8 archive and binding hashes are checked against release manifests whose hashes are pinned in the repository. Corrupt cache entries are replaced only after the downloaded bytes pass verification.

```sh
scripts/verify-container.sh --engine podman fast
scripts/verify-container.sh run -- cargo test --locked -p codex-tui --lib
scripts/verify-container.sh --reset-cache full
```

`--reset-cache` removes only the verifier's named volumes. `--no-cache` rebuilds the image layers. Docker and Podman maintain separate caches. `CARGO_BUILD_JOBS` controls compilation and test concurrency; hosted verification uses two jobs. The command prints phase timings and sccache statistics.

The **Container verification** GitHub workflow runs the complete path for verifier changes and supports manual execution. A successful fast run alone does not establish full-workspace acceptance.
