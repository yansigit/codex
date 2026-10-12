# Configuration

For basic configuration instructions, see [this documentation](https://developers.openai.com/codex/config-basic).

For advanced configuration instructions, see [this documentation](https://developers.openai.com/codex/config-advanced).

For a full configuration reference, see [this documentation](https://developers.openai.com/codex/config-reference).

`features.chatgpt_chat` is experimental and disabled by default. It gates
[native ChatGPT](../codex-rs/chatgpt-chat/README.md), experimental Chat RPCs, and
the independent **Codex | Chat** terminal surfaces on trusted local connections.
The Chat surface requires the owned full-screen renderer and alternate screen.
TCP connections, including localhost WebSockets, are unsupported for Chat.

Enable Chat in `config.toml`, then restart Nyacode:

```toml
[features]
chatgpt_chat = true

[tui]
fullscreen_transcript = true
alternate_screen = "always"
```

F6 switches surfaces; remap it with `tui.keymap.global.switch_surface`. `/chat`
and `/codex` also switch. Chat has independent drafts, transcripts, attachments,
and scroll positions. `/new` creates a saved draft, `/temporary` creates a new
memory-only draft, `/history` opens ChatGPT history, `/drafts` recovers local saved
drafts, `/chats` switches open chats, `/model` chooses a model, and `/attach <path>`
selects a file. F2 opens another chat during a response; F3 lists open
chats. F5 refreshes a pending conversation without changing its draft. Ctrl-V selects a clipboard image without creating a temporary file.
Attachments upload on Submit. Ordinary `!`, `@`, and `$` text stays literal.
Ctrl-C requests a local stop; the UI distinguishes it from confirmed server
cancellation. `/refresh` reconciles pending submissions without resending them.

Native transport, saved recovery through fresh embedded/daemon processes,
temporary continuation, five attachment formats, terminal switching, cancellation,
socket reconnection, and feature disablement have recorded live checks.
Transport builds passed for all nine target configurations on Rust 1.95.0.
Linux x86_64 GNU and musl ran tests; Windows, macOS, and ARM checks establish
cross-compilation and linking. The compatibility probe remains read-only unless
`--write-scenario` is explicitly selected. See the linked acceptance assessment
for exact scenarios and validation limits.

## Lifecycle hooks

Admins can set top-level `allow_managed_hooks_only = true` in
`requirements.toml` to ignore user, project, and session hook configs while
still allowing managed hooks from requirements and managed config layers. This
setting is only supported in `requirements.toml`; putting it in `config.toml`
does not enable managed-hooks-only mode.
