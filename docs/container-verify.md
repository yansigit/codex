# Container verification

`scripts/verify-container.sh` runs the Rust checks in a Ubuntu 24.04 image. Docker and Podman both build `Containerfile` (a `Dockerfile` symlink points at the same file).

```sh
scripts/verify-container.sh fast
scripts/verify-container.sh
```

`fast` runs `cargo fmt --check`, then Clippy and nextest for crates whose files changed since the merge base with `origin/main`. `scripts/verify-container.sh` with no arguments, or `full`, then runs workspace fmt, Clippy, a debug build, nextest, and a `nyacode --version` / `--help` smoke test.

The script bind-mounts the repository and nothing from the host home directory. Sandbox tests inside the container mount `$HOME`; mounting the host home nests those mounts and crashed the first upstream-sync test run. The container home is `/home/verifier`, created in the image. The script refuses to start if that path is itself a mount.

## Toolchain and PATH

The image installs Rust 1.95.0, matching `codex-rs/rust-toolchain.toml`, with rustfmt, clippy, and rust-src. `cargo`, `rustc`, and `cargo-nextest` live on `/usr/local/cargo/bin` and are symlinked into `/usr/local/bin`, and the image `PATH` includes both. A non-login shell still finds `cargo`.

System packages match the Linux CI and source build prerequisites: a C toolchain, pkg-config, OpenSSL headers, Clang, CMake, protobuf, libcap, ALSA headers, and bubblewrap.

## Cache

These named volumes persist across runs:

| Volume | Contents |
| --- | --- |
| `nyacode-verify-cargo-registry` | Cargo registry download cache |
| `nyacode-verify-cargo-git` | Cargo git dependency checkouts |
| `nyacode-verify-target` | `codex-rs/target` |
| `nyacode-verify-sccache` | sccache object cache |

`RUSTC_WRAPPER=sccache` and `CARGO_INCREMENTAL=0` are set for the run. The second run of an unchanged build should finish from the target directory and sccache instead of recompiling. `scripts/verify-container.sh --reset-cache` deletes the volumes before the next run.

Each engine has its own volumes. A Docker cache is not visible to Podman.

## Stack size

Debug tests for `codex-app-server --test all` and `codex-tui --lib` overflow an 8 MiB thread stack and pass at 64 MiB. The container sets `RUST_MIN_STACK=67108864` and raises the process stack ulimit to 64 MiB. `cargo test` reads the same value from `codex-rs/.cargo/config.toml`. `just test` and the full Linux nextest job export it before starting nextest. cargo-nextest 0.9.103, the version CI installs, does not read a profile `env` table, so the variable has to be in the environment nextest inherits.

## Crates this image does not build

`[workspace.metadata.nyacode.container-verify]` in `codex-rs/Cargo.toml` lists the packages the script passes to `--exclude`:

- `codex-code-mode-runtime` and `codex-code-mode-host`: `rusty_v8` 150.4.0 has no Linux prebuilt for `v8_enable_sandbox`.
- `codex-voice-host`: the `gstreamer` crates require GStreamer >= 1.28, and Ubuntu 24.04 ships 1.24.

The same metadata holds a nextest filter for tests that spawn those binaries, including app-server cases that fail closed when `codex-code-mode-host` is absent. Other tests still run. Add a test name to that filter when a new test starts requiring one of the excluded binaries. Tests that only need `codex` or `test_stdio_server` stay in the suite; the full run builds those bins before nextest.

## Other commands

```sh
scripts/verify-container.sh --engine podman fast
scripts/verify-container.sh run -- cargo test -p codex-tui --lib
scripts/verify-container.sh --reset-cache full
```

`run --` executes a command in the same container, with the same mounts, home directory, and caches. `--engine` selects `docker` or `podman`. When it is omitted, the script uses whichever engine is already reachable, including via `sudo`.
