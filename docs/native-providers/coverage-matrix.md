# Native provider evidence matrix

This matrix separates local tests, provider-account checks, and product-level
acceptance. Local fake transports and temporary fixtures do not prove behavior
against a service. Pinned upstream evidence is catalogued in
[references.md](references.md).

| Area                                     | Current implementation and local evidence                                                                                                                                                                                            | Live or integrated evidence recorded                                                                                                                                                                                                                                                                                          | Remaining acceptance                                                                                                                                                                                                       |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| CLI, provider selection, configuration   | Provider commands and additive model-provider IDs exist for `cursor`, `opencode-go`, `claude`, and `antigravity`. A full CLI build passed on the logged revision. Config schema generation passed.                                   | Explicit Go, Claude, Antigravity and Cursor model selection exercised in live runs. Four provider model-change runs passed.                                                                                                                                                                                                   | Rebuild and run current-source CLI after latest provider/API changes; verify account status and all CLI subcommands.                                                                                                       |
| Login, cancellation, credentials         | API/auth tests cover PKCE/state, duplicate state, success exchange, cancellation, atomic persistence, refresh coordination, revocation, provider/account isolation, and env-token override behavior. Test totals are recorded below. | Claude interactive OAuth completed through the user callback; Cursor API-key exchange passed.                                                                                                                                                                                                                                 | Cursor browser OAuth is pending after an earlier callback flow timed out; Antigravity interactive login is awaiting a fresh user flow. App-server native account regression tests are added but not yet run in root.       |
| Refresh and rotation                     | Regression tests cover concurrent refresh, caller cancellation after refresh acceptance, omitted rotated refresh token, ambiguous refresh failure, and no automatic retry after revocation.                                          | Claude refresh/rotation and Antigravity refresh succeeded for selected accounts.                                                                                                                                                                                                                                              | Revalidate revoked-token and refresh recovery through each service's user flow.                                                                                                                                            |
| Model discovery and protocol families    | Provider tests cover codecs and model-specific protocol routing. OpenCode Go catalog is model-specific; Antigravity discovery tests cover project and catalog response variants.                                                     | Go: `glm-5.3` Chat, `minimax-m3` Messages, `gpt-6-luna` Responses. Claude: `claude-haiku-5-5` Messages with adaptive reasoning. Antigravity: Gemini and Claude-family models.                                                                                                                                                 | Broader entitlements and account coverage; repeat model discovery on current source where code changed.                                                                                                                    |
| Streaming, framing, cancellation         | Codec/transport tests cover fragmented events, malformed or truncated streams, terminal handling, parallel result order, cancellation, and post-terminal usage chunks. Core native provider suite has 20 passing tests.              | All selected Go/Claude/Antigravity real Codex sessions completed tool continuations and a resumed read. Cursor cancellation killed a sleeping child and prevented completion; resume failed closed and requires an explicit branch.                                                                                           | Rerun targeted suites on final source; verify all provider failures and cancellation checkpoints under the final runtime.                                                                                                  |
| Tools, approvals, and sandbox            | Driver tests cover serial/parallel tool calls, result matching, pending-tool failures, and cancellation. Translator tests exercise generated read/write/grep/list commands against temporary fixtures.                               | Go, Claude, and Antigravity each passed two real Codex exec/write/read tool cycles. Cursor passed native file read/write, shell, ordinary exec and stdin, MCP text echo/error, missing-file failure, permission denial, and cancellation.                                                                                     | Exercise approval allow/deny, sandbox behavior, broader MCP and tool types. Cursor MCP image and latest typed-MCP binary acceptance remain pending.                                                                        |
| Checkpoints, restart, branch, compaction | Store/core tests cover full namespace isolation, checkpoint cancellation, edited-history rejection, and refusing interrupted tool replay.                                                                                            | Go, Claude, Antigravity and Cursor each passed process restart/resume for an exec or text check. Model changes passed for all four providers. Branch/compaction passed for Cursor, Go and Antigravity with explicit provider/model fork values; the Claude log is inconclusive because concurrent runs wrote to the same log. | Rerun Claude lifecycle acceptance with a unique log. Validate default fork/cold-resume inheritance without an explicit provider override; regression is added but not yet run. Repeat branch/compaction on current source. |
| Cursor native operations                 | Cursor transport suite reports 23 passing tests in its current isolated workspace; translator has nine isolated fixture tests for path quoting, text caps, binary rejection, grep behavior and directory results.                    | Live Cursor checks covered native read/write, shell output, ordinary TTY exec and stdin, MCP text success/error, file failure, permission denial, process cancellation, restart/resume, and model change.                                                                                                                     | Native grep/list/delete full-runtime acceptance, MCP images, and current-source build after shell cleanup/float-argument and typed-MCP changes.                                                                            |
| Signatures and reasoning persistence     | Codecs retain provider-owned signed or opaque state; tests reject changed checkpoint history/results and verify provider-specific tool/signature round trips.                                                                        | Claude reasoning streamed successfully.                                                                                                                                                                                                                                                                                       | Audit signatures across exact model/account/thread changes, compaction, branches and restart on current source.                                                                                                            |
| UI and app-server                        | Protocol/schema surfaces generated. A native account integration test file covers login/read/logout, notifications, account isolation, and env override, but root has not yet run it.                                                | No accepted TUI/app-server interactive account experience recorded.                                                                                                                                                                                                                                                           | Run native account and fork/cold-resume regression tests in app-server; verify TUI and API client behavior where exposed.                                                                                                  |
| Live acceptance by provider              | Selected workspace package test snapshot: 110 passed; latest Cursor isolated transport suite: 23 passed; core native provider tests: 20 passed. See ledger for exact test receipts and current-source caveats.                       | Go (three wire families), Claude, Antigravity Gemini and Antigravity Claude-model flows passed real Codex tool cycles and resume reads. Cursor passed actual local tool/runtime checks.                                                                                                                                       | Latest-source CLI/app-server build and test gates, lifecycle reruns, OAuth flows, approvals/denials, MCP image path, and remaining provider-specific failure cases.                                                        |

## Live check details

- **Cursor:** `gpt-5.6-luna-none` passed API-key exchange, discovery, text,
  native file read/write, shell output, ordinary exec/stdin, MCP text echo and
  error, missing-file failure, permission denial, cancellation, and restart
  resume. A separate model-change run to `gpt-5.6-luna-low` passed. MCP image
  acceptance awaits a binary containing the newest typed image bridge. Browser
  OAuth remains incomplete after the previous flow timed out.
- **OpenCode Go:** explicitly selected `opencode_go_1` passed live provider
  streaming across Chat (`glm-5.3`), Messages (`minimax-m3`), and Responses
  (`gpt-6-luna`). The real Codex CLI ran two exec/write/read commands and a
  resumed exec/read on each model; files and final markers matched. Model
  change to `minimax-m3` and branch/compaction with explicit fork model passed.
  The original upstream key returned monthly-quota exhaustion; no fallback
  account was selected.
- **Claude:** `claude-haiku-5-5` passed provider refresh, discovery, reasoning,
  live Codex tool cycles, and resumed exec/read. Interactive OAuth login
  succeeded through the user callback. Model change to `claude-sonnet-4-6`
  passed. The named branch/compaction receipt is inconclusive due to competing
  writers; rerun before claiming that lifecycle coverage.
- **Antigravity:** `gemini-3.5-flash-lite` and `claude-sonnet-4-6` passed real
  Codex tool cycles and resumed exec/read. Refresh and discovery passed for the
  Gemini model; model change and branch/compaction with explicit fork provider
  and model passed. Interactive OAuth is awaiting a fresh user flow.

The real Codex service receipts for Go, Claude and Antigravity are from the
canonical-history binary before later typed-error and multimodal-source changes.
They establish ordinary tool execution and resume only; those newest changes
still require live reruns. The Cursor CLI receipt likewise predates its latest
typed-MCP image work.

## Schema and test receipts

- `source /tmp/codex-build-env.sh && just write-config-schema` succeeded.
- The app-server schema fixture writer succeeded; its exact fixture test passed
  (1 passed), and stable JSON/TypeScript plus Python SDK artifacts were
  generated. The Python generator emitted `uint*` format warnings and finished.
- `/tmp/native-core-tests-final.log`: native core tests, 20 passed.
- `/tmp/native-package-tests.log`: selected workspace package suites, 110
  passed, including provider API/auth integration tests and provider codecs.
- `/tmp/native-model-change-{cursor,opencode-go,claude,antigravity}.log`:
  all four model changes passed.
- `/tmp/native-lifecycle-{cursor,opencode-go,antigravity}.log`: branch and
  compaction passed with explicit fork provider/model. The Claude log ends in
  an ambiguous failure and needs a clean rerun.
- `/tmp/native-codex-services-latest-20feyi06/summary.json`: six Go/Claude/
  Antigravity model routes passed first-turn tool execution and resumed reads.
- `/tmp/native-cursor-core-acceptance/`: private Cursor runtime receipts; no
  credentials are checked in.

## Evidence boundaries

- Live provider acceptance covers the named account/model combinations and
  does not establish service entitlement for every account or catalog entry.
- Successful native tool execution does not establish exhaustive approval,
  sandbox, or permission-denial behavior across providers.
- Cursor translator tests use only temporary fixtures; they do not execute
  arbitrary model-supplied commands or prove all operations work through Codex.
- No provider is marked complete. Build, Responses compatibility, app-server,
  clippy, OAuth, lifecycle, and current-source reruns remain open where listed.
