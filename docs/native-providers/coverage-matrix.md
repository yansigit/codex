# Native provider evidence matrix

This matrix separates local tests, provider-account checks, and product-level
acceptance. Local fake transports and temporary fixtures do not prove behavior
against a service. Live checks against one model do not establish entitlement
or compatibility for every model. Pinned upstream evidence is catalogued in
[references.md](references.md).

| Area | Current implementation and local evidence | Live evidence recorded | Remaining acceptance |
| --- | --- | --- | --- |
| CLI, provider selection, configuration | `codex provider login/status/logout/models` is implemented for `cursor`, `opencode-go`, `claude`, and `antigravity`; IDs route through model-provider configuration. | The provider `models` calls were exercised for selected accounts below. | Integrated CLI checks and user-facing status/model selection behavior; clarify account selection for commands without `--account`. |
| Login, cancellation, credentials | Shared auth tests cover callback state/PKCE, credential persistence, cancellation, refresh coordination, revoked credentials, and provider/account isolation. Token input is read from stdin. | Claude and Antigravity refresh succeeded; Cursor API-key exchange succeeded. | Interactive OAuth acceptance for Claude, Antigravity, and Cursor; cancellation through each real callback flow. |
| Refresh and rotation | Local auth regressions cover concurrent refresh and preserve the prior refresh token when a response omits a replacement. | Claude rotated credentials successfully; Antigravity refreshed successfully. | Repeat refresh under concurrent real requests and validate revoked-token recovery for each service. |
| Model discovery and protocol families | Provider crates have local codec/transport regression suites. Catalog and protocol selection are model-specific. | Go: `glm-5.3` Chat, `minimax-m3` Messages, and `gpt-6-luna` Responses; Claude: `claude-haiku-5-5` Messages with reasoning; Antigravity: Gemini and Claude-family models. | Broader model coverage, entitlement handling, and integrated Responses compatibility tests. |
| Streaming, framing, cancellation | Local tests cover provider codecs, transport errors, malformed/fragmented events, terminal handling, and cancellation. | Selected provider sessions streamed text and cancellation passed. | Validate malformed/truncated streams against the integrated Codex event mapper and all provider families. |
| Tool calls and result pairing | Driver tests cover parallel calls, call/result matching, pending-tool terminal errors, and cancellation. Cursor translation has nine isolated unit tests, including generated read/write/grep/list commands against temporary fixtures. | Go, Claude, and Antigravity passed two harmless mock-host echo tool rounds; Cursor passed two sequential host-tool results on one HTTP/2 stream. | Exercise Codex shell, file, grep/list, MCP tools and approval allow/deny through the integrated runtime. The service mock-host checks do not prove Codex permission behavior. |
| Checkpoints, resume, interrupted execution | Store/driver tests cover provider/account/model/thread namespace isolation, checkpoint cancellation, and refusing incomplete tool-journal replay. | Go, Claude, and Antigravity resumed a second turn from cumulative checkpoints; Cursor completed a checkpoint second turn. | Process restart, branching, compaction, model/account changes, and interruption around provider commit. |
| Cursor native file and search translation | Linux-only operation availability is gated on required executables and Cursor's normal `exec_command`/`write_stdin` tools. Isolated temp-fixture tests check unterminated lines, UTF-8 cap handling, binary rejection, grep filters/context/count, ignored names, and absolute directory results. | Cursor live continuation used mock host tools; it did not exercise the Codex native operations and approval runtime. | Integrated Codex tool execution, permissions, large files, malformed filenames, and platform availability checks. |
| Signatures and reasoning persistence | Provider-specific codecs retain opaque provider state in checkpoints; local codec tests pass. | Claude reasoning and selected model streams were exercised; no complete signature persistence audit is recorded. | Verify signatures round-trip for the exact model/account/thread and across process restart, branch, compaction, and model changes. |
| UI and app-server surfaces | CLI and normal model-provider selection are wired. | No separate TUI or app-server login/logout acceptance is recorded. | Verify provider status and account flows in app-server and TUI surfaces, if exposed. |
| Live acceptance by provider | Local tests are not live acceptance. The lead's ledger records provider-specific integration boundaries and current gaps. | Cursor API-key exchange/discovery and two streamed tool continuations; Go, Claude, and Antigravity selected-model tool rounds, checkpoint resume, and cancellation passed. | Complete interactive OAuth, full Codex host/approval path, restart/branch/compaction, and provider-specific failure/quota cases. |

## Live check details

- **Cursor:** `gpt-5.6-luna-none` passed API-key exchange, discovery (256
  models), text, two sequential host-tool results on the same HTTP/2 stream,
  cancellation, and checkpoint second turn. OAuth refresh and Codex native
  operations remain open.
- **OpenCode Go:** an explicitly selected `opencode_go_1` pool account passed
  two tool rounds, canonical-history checkpoint resume, and cancellation across
  Chat, Messages, and Responses model families. The original upstream key
  returned monthly-quota exhaustion; no fallback account was selected.
- **Claude:** `claude-haiku-5-5` passed refresh, discovery (14 models),
  reasoning, two tool rounds, checkpoint resume, and cancellation. Interactive
  login and full Codex execution remain open.
- **Antigravity:** `gemini-3.5-flash-lite` passed refresh, discovery (26
  models), two tool rounds, checkpoint resume, and cancellation. Interactive
  login and full Codex execution remain open.

## Evidence boundaries

- The live tool rounds used a harmless mock host. They prove provider streaming
  and tool-call continuation for those sessions, not local approval or sandbox
  behavior.
- Model counts and live outcomes describe the tested accounts and date only;
  they are not promises about current catalog size or service entitlement.
- The Cursor translator tests run generated shell commands only against
  temporary fixtures in an isolated harness. They do not execute arbitrary
  model-supplied commands or establish the complete Codex runtime integration.
- Full integrated CLI/core verification and existing Responses compatibility
  checks are tracked in the implementation ledger.
