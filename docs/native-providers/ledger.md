# Native provider implementation ledger

The implementation remains under final integration review. Provider transports
and selected live sessions do not establish complete end-to-end acceptance.
Public source revisions and licenses are recorded in
[references.md](references.md); behavioral coverage and evidence limits are in
the [coverage matrix](coverage-matrix.md).

## Architecture and integration

- Provider crates depend on `provider-api`, not `codex-core`.
- Core supplies host execution through the normal tool router, preserving
  Codex-owned approvals, sandbox policy, hooks, and user-facing tool events.
- `provider-auth` isolates credentials by provider/account and stores
  checkpoints and tool journals with provider/account/model/thread identity.
  One account is selected per provider; there is no automatic rotation.
- OAuth uses PKCE/state, cancellable callbacks, atomic credential writes, and
  coordinated refresh. An interrupted tool call refuses automatic replay.
- CLI provider commands, configuration, native account RPCs, model discovery,
  and schemas are additive. Native thread inheritance supports cold resume and
  fork when the client omits an explicit provider override.
- A thread-scoped execution lease now preserves the uncertain-tool fence across
  provider/account/model changes; a deliberate fork receives a separate lease.

## Verification evidence (2026-10-10)

- **App-server:** `/tmp/native-appserver-tests.log` records all three native
  account regressions passing: token login/read/logout and notifications,
  stored Go account preservation during Claude logout, env-token override
  rejection, and native Go model/provider inheritance through fork and cold
  resume.
- **Responses:** `/tmp/native-responses-tests.log` records 33 compatibility
  tests passing.
- **Core and provider regressions:** `/tmp/native-review-regressions.log`
  records the additional cross-namespace execution-lease, checkpoint, codec,
  catalog, and provider suites passing. The earlier core suite in
  `/tmp/native-core-tests-final.log` recorded 20 native-provider tests passing.
- **Cursor:** current isolated suite has 26 passing tests with strict clippy
  success. Live browser auth/refresh and Linux native operations are recorded
  below. The latest production model catalog enables only model-supported
  image routes; full Codex image-path reruns are in progress.
- **Schema:** config, app-server JSON/TypeScript, and Python SDK generation
  succeeded; the schema fixture test passed. Python emitted `uint*` format
  warnings but completed.
- **Final gates:** a full current-source build, root clippy and feature checks
  are still in progress. Do not treat earlier build/clippy logs as final.

## Live acceptance receipts

Authorized account flows were tested without printing credentials. Pool account
selection was explicit; no automatic account fallback was used.

| Provider    | Verified live behavior                                                                                                                                                                                                                                                                                                                                                                                                                                                                   | Still open                                                                                                                                                                              |
| ----------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Cursor      | Browser OAuth and refresh passed; refresh changed access, retained refresh token and identity generation, cleared the pending marker, then model discovery (256) and text passed. Linux CLI exercised native read/write, `ls`, `grep`, delete, shell output, exec/stdin, MCP text/image, failure, policy denial, cancellation and resume. Interactive approval RPC decline left a file absent and generation continued; acceptance wrote `ACCEPTED_WRITE` through the normal Codex host. | The MCP image check used a temporary image-enabled model catalog; rerun against the production-default image catalog on current binary.                                                 |
| OpenCode Go | Chat (`glm-5.3`), Messages (`minimax-m3`), and Responses (`gpt-6-luna`) passed actual Codex tool calls and resume reads. Model change and branch/compaction passed. Normal permission denial and approval acceptance passed. Chat and Responses vision recognized the tested red PNG.                                                                                                                                                                                                    | Messages view-image misidentified the tested image on two probes. An image-only result placeholder fix is integrated; full-core rerun is pending. Repeat acceptance on the final build. |
| Claude      | Browser OAuth, 14-model discovery, refresh, reasoning, core view-image, live Codex tool calls and resume passed. Model change, branch/compaction, approval denial/acceptance, and TUI status/model/provider/account presentation passed.                                                                                                                                                                                                                                                 | Repeat on final build and continue checking signatures across model/account/compaction boundaries.                                                                                      |
| Antigravity | Browser OAuth, 26-model discovery, Gemini and Claude-family model changes, branch/compaction and permission denial/acceptance passed. Native Rust Gemini and Claude zero-argument image loops identified the tested red PNG.                                                                                                                                                                                                                                                             | Full core image verification on final build is pending; native provider-specific error and cancellation cases need continued coverage.                                                  |

All four latest lifecycle receipts are in
`/tmp/native-lifecycle-current-{cursor,opencode-go,claude,antigravity}.log`;
each passes branch and compaction without an explicit fork provider override.
All four model-change receipts passed. Normal Codex approval denial and
acceptance for Go, Claude and Antigravity are in
`/tmp/native-permissions-{opencode-go,claude,antigravity}.log`.

Cursor-specific refresh output is summarized in
`/tmp/native-cursor-core-acceptance/browser-refresh-native.log`; native
`ls`/`grep`/delete and approval output are under
`/tmp/native-cursor-core-acceptance/native-ls-grep-delete.*` and
`permissions-sonnet.*`. Antigravity browser/model/image tests are summarized in
`/tmp/native-fresh-antig-images.out`.
Core image routes are under `/tmp/native-core-services-images-per7emtn/` and
`/tmp/native-core-services-images-a3g5t7dj/`; the Go Messages color failures
are under `/tmp/native-core-services-images-siarjpky/`. These are private
receipts outside the repository.

## Remaining acceptance work

- Complete and record the final root build, clippy, and feature checks.
- Re-run Cursor MCP image with the production default catalog, Go Messages
  view-image after the placeholder fix, and Antigravity images through the full
  Codex core on the final binary.
- Rerun Cursor MCP image with the production-default catalog; the accepted
  image call used a temporary image-enabled fixture catalog.
- Re-run the provider sessions and lifecycle paths on the final binary after
  root build changes, then test provider-specific quota, revocation, malformed
  response, and cancellation cases.
- Audit signed reasoning and checkpoints through model/account changes,
  compaction, branch, restart, and interrupted execution.
- Continue broader UI/app-server and entitlement checks without assuming that
  one account/model represents every provider account or model.

No provider is marked complete; the implementation still has current-build,
multimodal-route, and provider-specific acceptance follow-ups.
