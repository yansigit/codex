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

`scripts/native_provider_architecture.py` reads Cargo metadata and checks that
the manifest exactly accounts for packages in the owned subtree, that each is
physically inside that subtree, that integration hooks still exist, and that
only the adapter can directly depend on the listed upstream packages. The
script's tests cover both accepted adapter dependencies and rejected subsystem
dependencies or ownership drift.

## CI and upstream drift probe

The hosted Linux workflow runs the architecture check, its failure-control
tests, provider package tests, and adapter feature checks with no features,
each provider enabled alone, and all features enabled.

A daily and manual job fetches upstream `main` into an isolated temporary
worktree, attempts a no-commit merge, records the base and upstream commit
hashes, and lists source conflicts separately from generated schema/export
conflicts. The probe never resolves conflicts and never pushes. It uploads the
merge log and conflict lists for review. GitHub only starts scheduled workflows
from the repository's default branch, so the daily schedule remains inactive
until this workflow reaches `main`.

The probe reports mergeability only. A clean merge still requires normal tests
and generated artifact review before integration.
