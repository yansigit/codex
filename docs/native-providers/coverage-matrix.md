# Native provider evidence matrix

This matrix is a source-triage aid for the planned implementation and
acceptance work. It prevents tests for one layer from being mistaken for proof
of another. `Ref` means a pinned reference has relevant source or tests;
`Partial` means only part of the behavior or a mock/local test is evident;
`Gap` means this inventory has no provider-specific evidence; `Live: none`
means no live acceptance was run as part of this inventory. These labels do
not claim that a full audit of each repository's test suite is complete.

| Required behavior | OpenCodex | Shunt | Jcode | Antigravity-auth / Pi SDK | Acceptance still required |
| --- | --- | --- | --- | --- | --- |
| OAuth login, cancellation, persisted state | Ref: Cursor OAuth and Claude integration tests | Partial: OAuth flows and notes; audit tests per adapter | Ref: `auth/oauth_tests/`, Cursor/Claude auth modules | Ref: Antigravity OAuth tests; Pi SDK has Claude OAuth source | Exercise login and user cancellation for each supported native provider; restart and verify state |
| Token refresh, refresh rotation, concurrent refresh | Partial: Claude revoked-token integration and auth tests; separate cases must be checked | Partial: Anthropic multi-account refresh material | Ref: `refresh_coordinator.rs`, `refresh_state.rs`, provider refresh tests | Partial: account lifecycle tests; not evidence for every token rotation case | Test concurrent requests share refresh, rotated refresh token is atomically persisted, and failure leaves recoverable state |
| Revoked credentials, account isolation | Partial: `tests/claude-integration/messages-revoked-token.test.ts`; Cursor pool/OAuth tests | Partial: multi-account provider and inbound auth tests | Ref: auth account pools and provider tests | Partial: account identity/repository tests | Verify revoked account fails clearly and never bleeds auth/state into another account or provider |
| Fragmented, malformed, and terminal streams | Ref: Cursor framing, EOF, transport, protocol-event tests | Partial: Responses/Anthropic translation tests | Partial: `protocol_tests/comm_responses.rs` and provider streams | Partial: `agy-transport.test.ts` | Run against real provider streams; cover split frames, invalid frames, truncation, clean end, upstream error |
| Parallel tool calls and call/result pairing | Ref: Cursor protobuf event and tool continuation suites | Partial: translation tests | Partial: provider protocol tests | Partial: Gemini/Antigravity transport does not establish tool-call semantics across all models | Verify IDs, order, arguments, multiple simultaneous calls, and results across resume |
| Timeouts, cancellation, and terminal handling | Ref: Cursor timeout/cancel/EOF tests; Claude request cancellation tests | Partial: HTTP timeouts and stream tests | Partial: local protocol tests | Partial: transport tests | Prove abort reaches socket/body, timeout is bounded, no late event is emitted, and a canceled turn is terminal |
| Tools, approval, sandbox behavior | Ref: Cursor tool execution policy and native exec suites | Partial: provider translation and tool support | Ref: tool execution and sandbox suites (host harness behavior) | Gap: auth plugin is not a tool approval/sandbox implementation | Exercise tool approval deny/allow, sandbox boundaries, and that provider metadata cannot bypass local policy |
| Signatures and reasoning persistence | Ref: Cursor continuity/signature-related tests; Claude integration | Ref: `docs/notes/anthropic-thinking-signatures.md` and translation audit | Partial: provider message persistence paths | Partial: Antigravity signature handling is provider/model-specific | Verify provider-authentic signatures round-trip with exact model/account/branch context; reject missing, stale, or modified signatures |
| Resume, branch, compaction, model/account change | Ref: Cursor checkpoint/continuation suites; local checkpoint store is not durable proof | Partial: adapter translation does not establish durable session semantics | Ref: compaction and persisted-state tests; inspect branch semantics separately | Gap: account repository tests do not prove agent conversation resume | Run crash/restart, branch, compaction, model/account switch, and stale-state cases; ensure old opaque state is invalidated |
| Interrupted turn cannot replay or duplicate spend | Ref: Cursor no-replay/committed-state cases and Claude cancel integration | Partial: retry-safety docs and adapter cases | Partial: response recovery/replay tests | Gap | Force disconnect before/after request commit and prove the client never silently resends a possibly billed request |
| Responses API compatibility and configuration | Ref: OpenAI/Responses adapters and compatibility suites | Ref: Responses translation docs/tests | Ref: provider protocol and config paths | Gap | Validate exact Responses request/event semantics and config round-trip in this product |
| UI and app-server integration | Ref: OpenCodex product integration only | Partial: Shunt plugin/config docs | Ref: Jcode server/provider control surfaces | Ref: OpenCode/Pi plugin integration | Implement and verify this repository's login, provider selection, status, and server/API surfaces |
| Live acceptance per provider | Gap: only a live-smoke environment gate was inspected; no acceptance was run | Gap: no live acceptance run here | Gap: opt-in account API test was not run | Gap: no live acceptance run here | Run authenticated checks separately for OpenAI/Codex, Claude subscription Messages, Cursor, OpenCode Go, and Antigravity: minimal text, streaming, tool round-trip where supported, cancel/timeout, refresh/rotation, and model config |

## Evidence boundaries

- Pinned repository tests are design evidence only. A test that mocks HTTP or
  uses local fixtures is not a live provider acceptance test.
- OpenCodex includes `tests/providers/cursor/cursor-live-smoke-gate.test.ts`;
  this test exercises the environment gate. Finding a gate in source does not
  mean a live request was executed.
- Jcode contains an opt-in account API test using
  `JCODE_ACCOUNT_TEST_API_BASE`; its presence does not establish that this
  inventory ran authenticated acceptance.
- Antigravity-auth tests establish local OAuth/account/transport behavior;
  they do not validate all model families, stream variants, or tool semantics.
- Do not port signatures, OAuth details, identity heuristics, retry behavior,
  or undocumented service headers without an explicit protocol/security review.
