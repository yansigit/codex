# Native providers

The CLI recognizes `cursor`, `opencode-go`, `claude`, and `antigravity` as
native provider IDs. They use the ordinary model-provider setting and model
selection:

```toml
model_provider = "claude"
model = "claude-haiku-5-5"
```

Use a model ID printed by `codex provider models <provider>`. Discovery reports
the catalog visible to the selected account; it is not an entitlement check.
Availability and supported features can vary by account and model.

## Login and account selection

The provider commands are:

```text
codex provider login <provider> [--account <name>] [--with-token]
codex provider status [<provider>]
codex provider logout <provider>
codex provider models <provider>
```

For an OAuth provider, start its browser flow and inspect status or available
models after it finishes:

```sh
codex provider login claude
codex provider status claude
codex provider models claude
```

OpenCode Go accepts an API key from stdin. Keep the key in a protected file or
pipe it from a secret manager; do not put it in a command argument:

```sh
codex provider login opencode-go --account work --with-token \
  < /path/to/protected-token-file
codex provider status opencode-go
```

`login` selects account `default` unless `--account` names another account.
Each provider has one selected account at a time. Logging in selects that
account; Codex does not automatically rotate or fall back to another account.
`status` without a provider lists all four recognized providers and shows names
or login state, never credential values. `logout <provider>` removes that
provider's selected stored account; it has no account selector. `models` also
uses the selected provider account.

## Credential source and configuration

Credentials can come from the native provider login store or from a configured
bearer token/environment variable. A configured token is authoritative for
requests. Login and logout are refused while the selected provider has a token
override, so remove that override before changing the stored login. For example,
OpenCode Go's built-in provider reads `OPENCODE_API_KEY`; while it is set,
`codex provider status opencode-go` reports a configured token and
`codex provider logout opencode-go` asks you to remove the override.

Built-in provider entries are not overridden by a same-named TOML table. If you
need an environment key for a native protocol, define a separate provider ID
with that native `wire_api`:

```toml
model_provider = "claude_from_env"
model = "claude-haiku-5-5"

[model_providers.claude_from_env]
name = "Claude native from environment"
wire_api = "claude"
env_key = "CLAUDE_API_KEY"
```

The native protocol still identifies as `claude`, so use the
`codex provider login claude`, `codex provider status claude`,
`codex provider logout claude`, and `codex provider models claude` commands for
the native account store.
When `CLAUDE_API_KEY` supplies the effective token, login/logout of stored
Claude credentials are disabled until it is unset. Avoid putting secrets
directly in `config.toml`; use an environment variable or the stdin login path.

Native providers own their authentication, discovery, headers, and transport.
Unsupported auth/header/catalog overrides and per-provider stream/connect
timeout settings are rejected. Request and stream retry settings must be zero;
native requests do not automatically retry or rotate accounts after failure.
Resume is explicit after interruption. Do not set nonzero
`request_max_retries` or `stream_max_retries`, or set
`stream_idle_timeout_ms` or `websocket_connect_timeout_ms`, for a native
provider.

## Private state and interrupted execution

Native credentials, refresh metadata, checkpoints, tool journals, and
interruption markers are stored under
`$CODEX_HOME/native-providers/` (or the default Codex home when `CODEX_HOME` is
unset). Filenames are derived from hashed provider/account/session keys. On
Unix, the state directory is private and state files are owner-readable only.
This state is separate from ChatGPT's `auth.json`; do not edit or remove it
while Codex is running.

If a native request is interrupted after a host tool may have started, Codex
refuses automatic replay. Inspect the local and external effects first, then
explicitly fork the conversation before continuing:

```sh
codex fork <session-id>
```

The recovery fence is thread-scoped. Resuming the same thread with another
model or provider account does not clear it. A new explicit branch has a
separate execution history; keep the original thread for review. Interruptions
before any tool starts can use the saved clean checkpoint, but once tool
execution begins its effects may be uncertain.

## Cursor native file and search operations

On Linux, Cursor native file read/write and search operations are available
when their required local executables are present and the request advertises
the normal `exec_command` and `write_stdin` tools. Codex translates them into
its existing shell-tool path, so normal Codex execution policy applies. Live
read/write execution has passed; the [coverage matrix](coverage-matrix.md)
tracks which search/list operations have only fixture coverage and which
Cursor runtime checks remain pending.

See the [pinned reference inventory](references.md) for source revisions and
the [evidence matrix](coverage-matrix.md) for current local and live acceptance
limits. Native providers are still in progress; these examples do not imply
that every provider flow, model, or feature is complete.
