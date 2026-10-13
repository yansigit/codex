> Nyacode fork: Python distribution names, imports, and exported types remain compatibility interfaces. Published `openai-codex` packages belong to upstream. To run this fork, use the local SDK source with `CodexConfig(codex_bin="/absolute/path/to/nyacode")`. The upstream installation and release examples below are retained as reference; they do not install Nyacode.

# Codex CLI Runtime for Python SDK

Platform-specific runtime package consumed by the published `openai-codex`.

This package is staged during release so the SDK can pin an exact Codex CLI
version without checking platform binaries into the repo.

`openai-codex-cli-bin` is intentionally wheel-only. Do not build or publish an
sdist for this package.
