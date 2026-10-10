# Native provider implementation ledger

The implementation remains in progress. Provider transport checks and selected
live runs do not establish complete end-to-end acceptance. Reference revisions
and licenses are recorded in [references.md](references.md); requirements and
evidence boundaries are in the [coverage matrix](coverage-matrix.md).

## Architecture and integration

- Compile-time provider crates depend on `provider-api`, not `codex-core`.
- Core routes model-supplied calls through the existing host tool executor, so
  Codex handles execution, approvals, sandbox policy, hooks, and presentation.
- `provider-auth` isolates credentials by provider/account and checkpoints/tool
  journals by provider/account/model/thread. One account is selected per
  provider; account rotation is explicit.
- OAuth uses PKCE/state, cancellable callbacks, atomic credential persistence,
  and cross-process refresh locking. Incomplete tool journals refuse replay.
- Provider CLI commands, config IDs, native account RPCs, and schema surfaces
  are additive. Native thread inheritance is being checked for fork/cold resume
  when clients omit an explicit provider override.

## Verification evidence (2026-10-10)

- **Core conversion:** `/tmp/native-core-tests-final.log` records 20 passing
  native-provider tests, covering message/tool conversion, history validation,
  checkpoint lineage, cancellation, and safe file/search translation fixtures.
- **Provider package snapshot:** `/tmp/native-package-tests.log` records 110
  passing tests across selected workspace/provider packages, including 25
  codec/transport tests, API/auth tests, and provider tests. The later isolated
  Cursor suite has 23 passing tests; its earlier 20-test run is included in the
  110-test snapshot.
- **CLI:** a full CLI build passed on the logged revision. Typed tool errors,
  multimodal support, and native thread inheritance changed afterward; rerun
  the full build against current source.
- **Schema:** config and app-server schema generation succeeded. The existing
  app-server fixture test passed (1 passed), and stable JSON/TypeScript plus
  Python SDK artifacts were generated. Python emitted `uint*` format warnings
  but completed successfully.
- **Model changes:** all four live provider model-change checks passed:
  Cursor to `gpt-5.6-luna-low`; Go to `minimax-m3`; Claude and Antigravity to
  `claude-sonnet-4-6`.
- **Responses and quality gates:** current-source Responses compatibility,
  app-server tests, and authoritative workspace clippy remain pending. The
  recorded `native-clippy.log` predates fixes and is not a passing gate.
- **Account/fork regression tests:** `native_account.rs` adds real app-server
  tests for native token login/read/logout and notifications, cross-provider
  credential isolation, environment-token logout rejection, and Go provider
  inheritance across fork/cold resume. The file is committed in the worker
  branch but has not yet been integrated and run by the root workspace.

## Live acceptance receipts

Live runs used authorized local accounts without printing credentials. Pool
account selection was explicit; no automatic fallback was used.

| Provider/model                                             | Observed result                                                                                                                                                                                                                    | Open follow-up                                                                                                                                                                                           |
| ---------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Cursor / `gpt-5.6-luna-none`                               | API-key exchange and discovery; native read/write and shell output; ordinary exec and `write_stdin`; MCP text echo/error; missing-file failure; permission denial; process cancellation; text restart/resume; model change passed. | Newer MCP image path and latest-source build remain pending. Browser OAuth did not complete in the prior timed-out callback attempt. Native grep/list/delete through the full runtime remain unverified. |
| OpenCode Go / `glm-5.3`, `minimax-m3`, `gpt-6-luna`        | On each route, real Codex ran two exec/write/read tools and a resumed exec/read; exact files and markers matched. Model change and explicit-provider branch/compaction checks passed.                                              | Rerun on current source after typed errors, multimodal and thread-inheritance changes; test implicit fork/cold-resume provider inheritance.                                                              |
| Claude / `claude-haiku-5-5`                                | Real Codex tool cycles and resumed exec/read passed. Interactive OAuth completed through the user callback. Model change passed.                                                                                                   | Re-run branch/compaction; the saved log is inconclusive because two runs wrote to one path. Rerun current source after typed-error and multimodal changes.                                               |
| Antigravity / `gemini-3.5-flash-lite`, `claude-sonnet-4-6` | Both routes passed real Codex tool cycles and resumed exec/read. Gemini refresh/discovery passed; model change and explicit-provider branch/compaction passed.                                                                     | Fresh interactive OAuth is awaiting another user flow. Rerun current source after typed-error and multimodal changes.                                                                                    |

Receipts are summarized in
`/tmp/native-codex-services-latest-20feyi06/summary.json`,
`/tmp/native-model-change-{cursor,opencode-go,claude,antigravity}.log`, and
`/tmp/native-lifecycle-{cursor,opencode-go,antigravity}.log`. The canonical
service binary predates typed tool-error and multimodal changes; these live
checks establish ordinary tool execution and resume, not those newer paths.
The Claude lifecycle log is explicitly inconclusive and is not counted as a
pass. The original OpenCode Go credential exhausted monthly quota; the
explicitly selected `opencode_go_1` pool account was used for tests.

Cursor runtime details and private per-run logs are under
`/tmp/native-cursor-core-acceptance/`; the evidence summary was supplied by the
Cursor worker. It includes a resume after text interruption. An interrupted
sleeping command was killed and did not complete; a subsequent resume failed
closed and required an explicit branch. MCP image acceptance is pending a
freshly built binary.

## Remaining acceptance work

- Build current source and run app-server, core, Responses compatibility,
  formatting, and clippy gates. Resolve any failures before relying on old
  binary evidence.
- Integrate and run the native account and fork/cold-resume app-server tests.
  The current thread inheritance fix needs explicit test coverage for Go
  `glm-5.3` while global config defaults to OpenAI.
- Repeat Go, Claude, and Antigravity live tool/resume checks on current source;
  rerun Claude branch/compaction with a unique log path.
- Rebuild Cursor and validate MCP image conversion, shell cleanup, native
  search/list/delete, and the current typed error bridge.
- Finish Cursor and Antigravity interactive OAuth flows. Exercise cancellation,
  approval allow/deny, sandbox behavior, and provider-specific failure paths.
- Verify signatures and durable checkpoints across restart, branch,
  compaction, account/model changes, and interrupted execution.
- Verify TUI/app-server status and account presentation where exposed.

No provider is marked complete until its remaining acceptance checks pass.
