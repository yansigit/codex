# Native providers

The CLI currently recognizes `cursor`, `opencode-go`, `claude`, and
`antigravity` as native provider IDs. These IDs can be selected through the
normal `model_provider` setting, for example:

```toml
model_provider = "opencode-go"
model = "<model-id>"
```

Use a model ID returned by `codex provider models <provider>`. That command
uses the provider account available to the CLI and prints model IDs and names;
the returned catalog is not an entitlement check. Availability and supported
features can depend on the provider account and model.

The implemented provider commands are:

```text
codex provider login <provider> [--account <name>] [--with-token]
codex provider status [<provider>]
codex provider logout <provider>
codex provider models <provider>
```

`login` defaults to account `default`. `status` without an ID reports each
recognized provider; it prints the account name or a login-required state, never
credential values. `logout` and `models` currently have no account option.
Provider login uses its OAuth flow unless `--with-token` is supplied. That flag
requires a non-terminal stdin stream and reads the token from stdin, never from
a command-line argument. OpenCode Go uses this API-key path. The CLI accepts
the flag for each recognized provider, but a token is useful only where that
provider supports token authentication.

Provider selection and the CLI are integrated, but not every provider feature
has completed end-to-end acceptance. The live checks recorded in the
[coverage matrix](coverage-matrix.md) cover selected models and flows; they do
not establish general account entitlement, all-model compatibility, or full
Codex tool approval behavior.

Cursor's native file and search operations are currently added only on Linux
when the required local executables are present and the request includes the
normal `exec_command` and `write_stdin` tools. Those operations are translated
into the existing Codex shell tool path, so execution remains subject to Codex
runtime policy. Other Cursor requests continue through the provider's ordinary
tool translation. This gating and the translated operations are still under
integration verification.

See the [pinned reference inventory](references.md) for source revisions and
the [evidence matrix](coverage-matrix.md) for local and live acceptance limits.
