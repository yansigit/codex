# Native Cursor provider

This crate speaks `agent.v1.AgentService` directly in Rust. It has no dependency
on `codex-core`, Cursor's executable, JavaScript, or a sidecar. The host supplies
the executable tool catalog and remains responsible for approval, sandbox,
hooks, tool presentation, and durable checkpoint persistence.

`CursorProvider::new(Options)` accepts a configurable origin, client version,
connection timeout, response timeout, and completion grace period. HTTPS is
required outside loopback fixtures. Redirects are disabled. `GetUsableModels`
uses protobuf unary RPC; `Run` uses a streaming request and response over HTTP/2
with Connect protobuf envelopes and bounded gzip decoding.

## Authentication

The host can use `auth::AuthClient` with its shared account store:

- `LoginChallenge::new()` supplies the browser URL.
- `poll_auth(challenge, cancellation)` completes the PKCE polling flow.
- `refresh_auth(refresh_token, cancellation)` refreshes an OAuth credential.
- `exchange_api_key(api_key, cancellation)` obtains an AgentService bearer token.
- `token_expires_at(token)` supplies an unverified JWT expiry hint.

API keys beginning with `crsr_` are exchanged before discovery and inference;
the original API key is retained by the host. The optional `auth::AuthStore`
provides private atomic credential writes and a cross-process refresh lock for
standalone consumers. Credential structs deliberately omit `Debug`.

## Checkpoints and tool execution

The session emits `Event::Checkpoint` before each new tool call and before
acknowledging server blob writes or a submitted host result. The host **must
persist each checkpoint before polling again**. The shared provider driver
implements this ordering. Raw conversation checkpoint bytes, opaque blob IDs,
blob data, and a completed execution journal are retained.

Resume requires matching provider, account, model, thread, system prompt, and
the exact saved message prefix. Content-addressed blob hashes are verified.
An unfinished execution after a restart has an unknown outcome and causes an
explicit checkpoint error rather than repeating a possible side effect.
Completed tool redelivery returns the saved result without another host call.

All ordinary host tools are exposed through Cursor MCP tool definitions. Native
operations are accepted only when the corresponding host alias is executable:

| Alias | Arguments | Successful result |
| --- | --- | --- |
| `cursor_shell` | `command`, optional `working_directory`, `timeout_ms` | `stdout`, `stderr`, `exit_code` |
| `cursor_read_file` | `path`, optional `offset`, `limit` | `content`, `total_lines`, `file_size`, optional `truncated`, `range_applied` |
| `cursor_write_file` | `path`, `content` | `lines_created`, `file_size` |
| `cursor_delete_file` | `path` | object |
| `cursor_ls` | `path`, `ignore` | `files` (names), `directories` (paths) |
| `cursor_grep` | Native pattern/path/glob/context/case/type/limit/multiline/sort/offset fields | `files`, `counts`, or `matches` according to `output_mode` |

Grep counts contain `{file,count}`. Content matches contain
`{file,lines:[{line_number,content,is_context_line}]}`. A failed host operation
sets `ToolResult::is_error`. Approval rejection additionally supplies
`{rejected:true,reason:...}`, which native codecs convert to rejection variants.
MCP and ordinary Codex tool results preserve typed text and inline PNG, JPEG,
WebP, or GIF images as native MCP content. Audio, resources, and remote tool
result image URLs require native adapters and return explicit unsupported
errors. Protobuf double arguments that represent exact integers retain integer
JSON syntax for host fields such as `yield_time_ms`.

The provider does not read, write, search, delete, or execute host files itself.
Absent protobuf options are omitted from host arguments. Shell hard timeouts use
the protocol `hard_timeout` field in milliseconds. Foreground shell streams
finish with a structured shell result and an explicit exec stream close,
including when execution is denied. Background shells require a host process
lifecycle adapter and return an explicit unsupported error.

## Protocol references and validation

The implementation was checked against pinned sources:

- [OpenCodex 848ec37](https://github.com/lidge-jun/opencodex/tree/848ec37ac230f8d93465c2358ba651a7f9f8764f):
  generated `agent.proto` descriptor, request/blob encoding, tool argument
  decoding, framing, and completion behavior.
- [Jcode 29d17a9](https://github.com/1jehuang/jcode/tree/29d17a9aee1851d90d8a7a061d4535e2d1864994):
  Rust HTTP/2 transport and model discovery behavior.
- [Shunt e147f9b](https://github.com/pleaseai/shunt/tree/e147f9b9c13b7072c04b1330bd1e7fd61d63b4c5):
  Cursor PKCE polling and native OAuth refresh behavior.
- [Pi Cursor SDK eee1ae4](https://github.com/fitchmultz/pi-cursor-sdk/tree/eee1ae42d41011ff3edc3994d9b0eed8dcafd85d):
  account/session lineage and host tool lifecycle lessons. This reference uses
  the Cursor SDK; this crate does not use that runtime.

Targeted tests cover fragmented/truncated/oversized/error Connect envelopes,
native argument codecs, credential exchange and refresh, private credential
writes, cancellation, foreign or corrupted checkpoints, unknown execution
outcomes, image blob encoding, and real in-process bidirectional HTTP/2 tool
requests with duplicate redelivery and trailing checkpoints, plus a native
shell stream that continues only after the structured result and close.

An authorized live probe with one existing account and
`gpt-5.6-luna-none` verified 256 discovered models, exact text completion, two
sequential host MCP calls and their results on one HTTP/2 stream, cancellation,
and a second user turn resumed from the native checkpoint. These observations
do not establish every discovered model or every native operation's live
behavior. A separate live probe observed native `shellStreamArgs` (exec tag 14),
ran an allowlisted `printf` command through an asynchronous test host, and
received exact `NATIVE_SHELL_OK` completion with a native checkpoint. This test
host does not validate Codex approval or sandbox enforcement.

Model-specific reasoning/output controls, remote image URL retrieval, history
image replay without a checkpoint, binary native writes, and unknown native
execution/control operations currently return explicit unsupported errors.
Active inline image data is encoded using `SelectedContext` and content-addressed
blobs; live vision acceptance is separate from the codec fixture.
