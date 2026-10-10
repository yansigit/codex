# Native provider implementation ledger

The native provider implementation now has live end-to-end evidence across
four provider families. The remaining root build, schema, and strict clippy
gates are still in progress. Provider source revisions and licenses are in
[references.md](references.md); scenario-level receipts and limits are in the
[coverage matrix](coverage-matrix.md).

## Architecture and integration

- Provider crates depend on `provider-api`, not `codex-core`.
- Core supplies host execution through the normal tool router, preserving
  Codex-owned approvals, sandbox policy, hooks, and user-facing tool events.
- `provider-auth` isolates credentials by provider/account and stores
  checkpoints and tool journals with provider/account/model/thread identity.
  One account is selected per provider; automatic rotation and fallback are
  not performed.
- OAuth uses PKCE/state, cancellable callbacks, atomic credential writes, and
  coordinated refresh. Interrupted host tools fail closed: resume requires an
  explicit branch after the caller inspects possible effects.
- CLI provider commands, configuration, account RPCs, model discovery, and
  schemas are additive. Native thread inheritance supports fork and cold
  resume when the client omits a provider override.
- A thread-scoped execution lease preserves the uncertain-tool fence across
  provider/account/model changes. A deliberate fork receives a separate
  execution identity.
- Native provider protocols own their authentication, discovery, headers, and
  transport. Unsupported overrides, timeout settings, and retry behavior are
  rejected instead of silently ignored.

## Verification evidence

- **Core:** `/tmp/native-core-tests-21.log` records 21 native-provider tests
  passing, including output-cap draining. Cross-namespace lease/checkpoint
  and provider regression tests are recorded in
  `/tmp/native-review-regressions.log`.
- **Provider packages:** the latest package snapshot recorded 198 passing
  tests. Cursor-specific validation recorded 26 Cursor tests and 29 codec
  tests in `/tmp/native-final-codec-cursor-tests.log`.
- **Responses:** `/tmp/native-responses-tests-final.log` records 33
  compatibility tests passing.
- **App-server:** `/tmp/native-appserver-tests.log` records three native
  account/fork tests passing: login/read/logout notifications, account
  isolation, env-token override rejection, and native fork/cold-resume
  provider inheritance.
- **Schema:** the native `Account`, `LoginAccountParams`, and
  `LoginAccountResponse` variants were regenerated into stable and
  experimental schema artifacts. The final four-test schema fixture gate is
  running; do not report it as passed yet.
- **Feature check:** the no-default-features check passed in
  `/tmp/native-feature-check.log`.
- **Final root gates:** the current CLI build, schema fixture suite, and strict
  workspace clippy rerun are still running. Earlier build/clippy results do
  not substitute for these final checks.

## Live provider acceptance

All checks used explicitly selected accounts in private test homes and did not
print credentials. The reusable app-server acceptance runner is
[`scripts/native_provider_acceptance.py`](../../scripts/native_provider_acceptance.py);
reproduction commands are in [usage.md](usage.md).

| Provider    | Verified live behavior                                                                                                                                                                                                                                                                                                                                       | Scope and remaining limits                                                                                                                                        |
| ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Cursor      | Browser login, refresh, 256-model discovery, model changes, ordinary native read/write/grep/list/delete, shell/exec/stdin, MCP text/image, file errors, policy rejection, permission decline/accept, lifecycle and interruption recovery passed. Final MCP image tests used the production-default catalog; historical user/tool image branches also passed. | Linux full-runtime checks. Background shells, binary native writes, and unsupported multimodal resources remain explicit errors.                                  |
| OpenCode Go | Chat `glm-5.3`, Messages `minimax-m3`, and Responses `gpt-6-luna` passed two initial exec/write/read rounds plus one resumed read. Model change, browser login/refresh/discovery, approvals, branch/compaction, interruption/recovery, and Chat/Responses vision passed. Go Messages MCP image passed through the full Codex core.                           | Selected account/model coverage. The original Go key had exhausted its monthly quota, so tests explicitly chose the available pool account; no fallback was used. |
| Claude      | Browser login/refresh, 14-model discovery, reasoning, view-image, model change, live tool rounds/resume, approvals, branch/compaction, active-tool interruption/recovery, and TUI status/provider/account/model display passed.                                                                                                                              | Selected models/accounts; no automatic account rotation.                                                                                                          |
| Antigravity | Browser login/refresh, 26-model discovery, Gemini and Claude-family model changes, full-core image checks, live tool rounds/resume, approvals, branch/compaction, and active-tool interruption/recovery passed.                                                                                                                                              | Selected models/accounts; broader provider error and entitlement coverage remains open.                                                                           |

For all four providers, model changes passed. All four also passed a clean
branch+compaction check without an explicit fork provider override. Active host
tool cancellation followed by restart and model change stayed fenced, then an
explicit branch continued without replay. Completion markers for the canceled
45-second commands remained absent after the timeout windows.

Full ordinary-tool and resume results are in
`/tmp/native-codex-services-final-ve861xmx/summary.json`. Interruption results
are in `/tmp/native-interruption-{opencode-go,claude}.log`,
`/tmp/native-interruption-antigravity-recovery.log`, and
`/tmp/native-harness-interruption-cursor.log`. Full-core image receipts are
listed in the [coverage matrix](coverage-matrix.md).

## Limits and follow-up

- The final root CLI build, four-test schema fixture gate, and strict clippy
  rerun must finish before calling the source snapshot fully validated.
- Live coverage is Linux-only for native filesystem and shell execution. It
  does not establish Windows/macOS command availability or every sandbox
  policy branch.
- Cursor drains completed foreground output for a bounded 500 ms grace period
  to capture late checkpoint writes. This is the tested design assumption;
  longer-running background shells are unsupported.
- Binary native writes, audio/resource inputs, remote-image resources, and
  unsupported model/protocol combinations return explicit errors.
- An optional baseline code-mode host build failed because the upstream V8
  sandbox archive returned 404. Native providers use Codex's ordinary host and
  do not depend on that optional binary; retain the normal default sandbox.
- Auth, model, and service checks cover selected accounts and models. Provider
  entitlements, account-wide rotation, and catalog-wide compatibility are not
  claimed. No provider is marked complete or exhaustive.
