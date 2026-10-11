# Native providers and complex agent tools

`spawn_agent` accepts an optional `model_provider` in both collaboration tool
versions. It resolves an existing configured provider ID, including aliases,
and validates the explicit model against that provider's catalog. Omitting the
provider keeps the parent provider. A provider change requires a fresh child:
`fork_turns: "none"` in V2 or `fork_context: false` in V1.

For example, a parent on Cursor can request this V2 child:

```json
{
  "task_name": "check",
  "model_provider": "opencode-go",
  "model": "glm-5.3",
  "fork_turns": "none",
  "message": "Inspect the requested files and report your findings."
}
```

The child retains the parent's effective workspace and permission restrictions.
It uses the target provider's model metadata, credentials, and account pool.
Parent-specific catalog, context limits, and default child model settings do not
leak into the target provider. Reloading a child restores its recorded provider.
Each thread has its own account affinity; automatic account failover stays within
that thread's provider and endpoint. Provider selection is an explicit spawn
choice, not a fallback for an unavailable model or account.

Plaintext V2 task, follow-up, and completion messages are translated in the native
adapter as attributed conversational input. Native tool schemas omit the
Responses-only encrypted-argument annotation, and returned calls carry explicit
plaintext metadata. Unexpected encrypted Responses arguments and encrypted agent
messages are refused.
Full-history forks across providers are also refused: neither signed thinking nor
opaque history is silently rewritten to fit another provider.

Native sessions defer incoming agent mail until durable session completion; they
do not abandon an in-flight provider request to deliver a mailbox update. Mail is
then included in the next request. During a session, `list_agents` supplies current
worker status and completed results. Explicit interruption still preserves the
existing uncertain-execution fence and recovery requirement.

Native Responses function tools explicitly retain optional schema arguments.
Cursor accepts the complete collaboration catalog within a bounded 512 KiB local
serialization limit. Descriptions and parameter schemas are not truncated.
Antigravity expands local schema references with depth and allocation limits;
remote, recursive, unresolved, and conflicting references remain explicit errors.

## Reproducible checks

The shared [integration manifest](integration-manifest.json) includes child
provider resolution, collaboration tool schemas and catalog overrides, adapter
message translation, and provider codec fixtures. The pooled-turn regression
combines out-of-order parallel results, an approval denial, a conclusive rejection,
and cross-account continuation. Repeated call IDs reuse recorded results; a new
ID with identical command text still executes.

The opt-in live runner uses the built app-server and existing private credentials:

```sh
python3 scripts/native_provider_multiagent_acceptance.py \
  --codex "$PWD/codex-rs/target/debug/codex" \
  --codex-home "$CODEX_HOME" \
  --provider cursor --model gpt-5.6-luna-none \
  --child opencode-go:glm-5.3 \
  --child antigravity:claude-sonnet-4-6 \
  --receipt /tmp/native-multiagent-receipt.json
```

Every participant must have an enabled managed pool. The runner verifies saved
child ownership, provider/model routing, effective account labels, command records,
exact temporary file contents, chained tools, deliberate exit-code failure,
follow-up delivery, and parent aggregation. It never accepts an agent's final
claim as sufficient evidence. Add `--lifecycle` to exercise child approval
decline/accept and cancellation of an active child command. The harness approves
only its exact temporary-file command and workspace.

Fixture threads disable connected apps and plugins by default. `--with-apps`
advertises them too, but Antigravity currently refuses some app schemas (including
schema-valued `additionalProperties`) rather than weakening their constraints.
The new home can contain ChatGPT credentials alongside native credentials, making
those app tools visible even when a native model is selected. This is an explicit
compatibility gap; local-tool acceptance does not establish arbitrary MCP/app-tool
support.

These are real model requests and can consume quota. The runner neither injects
quota failures nor changes pool membership. Separate test-only transport wrappers
cover the handoff boundaries described in [account-pools.md](account-pools.md).
Passing representative workflows does not establish every model, tool, operating
system, or simultaneous quota-failure combination.
