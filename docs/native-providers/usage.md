# Native providers: planned usage

This page describes the intended interface while native provider support is
being built. **The commands below are planned and are not available in the
current CLI. None of these providers has passed live acceptance in Codex.**
Provider metadata and some implementation pieces exist, but login, status,
model discovery, server/UI integration, and provider-specific behavior are
still incomplete.

The intended provider IDs are:

| ID | Service | Intended authentication |
| --- | --- | --- |
| `cursor` | Cursor | Native Cursor account login |
| `opencode-go` | OpenCode Go | API key entered through standard input |
| `claude` | Claude Pro/Max subscription | Native Claude subscription OAuth |
| `antigravity` | Google Antigravity | Native Google OAuth |

## Planned CLI

The planned commands are:

```text
codex provider login <id> [--account default] [--with-token]
codex provider status [<id>]
codex provider logout <id>
codex provider models <id>
```

The default local account name is `default`. `--with-token` selects the
standard-input API-key flow for providers that use a key, currently intended
for OpenCode Go. The key is read from stdin; it must never be passed as a
command-line argument. The CLI must keep token values out of status output,
logs, and diagnostic messages.

Login is intended to store credentials in Codex's private native-provider
state. The initial account interface has one explicitly selected account per
provider. It does not promise account pooling, automatic rotation, or
failover. `status` and `models` are planned read-only views; they are not
currently a guarantee that a provider account is entitled to a model.

## Provider selection

The configuration IDs map to the normal `model_provider` setting. The intended
shape is:

```toml
model_provider = "opencode-go"
model = "<model-id-from-provider-models>"
```

Use `cursor`, `claude`, or `antigravity` in place of `opencode-go` for those
providers. This is an example of the planned config shape, not a claim that the
current runtime can complete a request for each ID. The model catalog and
available capabilities may depend on the provider and account; no catalog
entry should be read as an entitlement check without an explicit live
validation.

## Current completion state

Provider IDs and wire-API metadata are being added to configuration and model
selection. Provider crates, host execution, credential management, and
app-server/CLI surfaces are being integrated in stages. Cursor still needs its
provider adapter. OpenCode Go uses a source-pinned model routing catalog, while
Antigravity model discovery and Claude subscription request handling still
need product-level verification.

The current local tests exercise protocol and state handling with fixtures and
fake endpoints. They do not establish successful login, streaming, tool use,
refresh, cancellation, or quota behavior against provider accounts. Treat all
four IDs as work in progress until the implementation and live acceptance
matrix are complete.

For the pinned research sources and the evidence gaps, see
[the reference inventory](references.md) and
[the coverage matrix](coverage-matrix.md).
