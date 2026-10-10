# Native provider refactor boundary

The provider implementations, protocol codecs, registry, and shared runtime
belong under `codex-rs/providers/`. The owned subtree is an independent Rust
subsystem: its packages may depend on one another, but they must not import
`codex-core` or other upstream integration packages. `codex-provider-adapter` is
the sole package allowed to connect that subsystem to upstream crates. The
maintained [integration manifest](integration-manifest.json) names the package
boundary and the upstream hooks that must remain present.

Provider-specific protocol behavior stays in provider implementations and
registry metadata. Upstream code keeps generic choices and typed boundaries;
the only provider-specific upstream compatibility surface is legacy wire
aliases. The integration hooks are:

- `model-provider-info` keeps the existing legacy enum as a wire alias while
  registry metadata supplies provider selection.
- Remote thread configuration carries generic provider identity through the
  existing config/proto path.
- `model-provider` retains its optional backend/facade surface.
- Core keeps a small adapter bridge in `native_provider.rs`; client stream and
  authentication routing use the backend. `SessionControl` owns session
  lifecycle and cancellation.
- Turn handling records typed completion, retry, and terminal state. Parallel
  tool dispatch captures structured results before resubmitting them through
  the adapter.
- Thread model metadata remains scoped to its thread.
- App-server account operations use generic adapter dispatch and an explicit
  ownership lock. Thread fork and cold-resume inherit provider identity when a
  caller supplies no override.
- CLI account and model choices come from generic provider metadata.

`scripts/native_provider_architecture.py` reads Cargo metadata and checks that the manifest exactly accounts for
packages in the owned subtree, that each is physically inside that subtree,
and that dependency closures remain inside the subtree for every subsystem
package. The adapter can reach upstream packages, but no owned package may
reach codex-core. Registry descriptor IDs must match the maintained scan list,
and each retained hook names its regression test group. The branch scan covers
multiline provider comparisons, match arms, and legacy wire variants in
reviewed integration sources.

## CI and upstream drift probe

The hosted Linux workflow uses the pinned Rust toolchain from
codex-rs/rust-toolchain.toml, runs formatting and provider clippy checks, the
architecture check and its failure-control tests, focused integration tests,
the CLI build, and adapter feature checks with no features, each provider
enabled alone, and all features enabled. Each stage writes a separate log; the
final JSON summary records the checkout/GitHub commit IDs and is also appended
to the GitHub Actions job summary.

A daily and manual job fetches upstream `main` into an isolated temporary
worktree, attempts a no-commit merge, records the base and upstream commit
hashes, and lists source conflicts separately from generated schema/export
conflicts. It installs the same pinned Rust toolchain and hosted Linux build
dependencies as deterministic CI, then runs the same focused suite after a
clean merge. The probe never resolves conflicts and never pushes. It always
writes logs and hashes, including when fetching or merging fails, and cleans up
its temporary worktree. GitHub only starts scheduled workflows from the
repository's default branch, so the daily schedule remains inactive until this
workflow reaches main.

The probe reports mergeability only. A clean merge still requires normal tests
and generated artifact review before integration.
