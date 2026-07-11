# grok-build-search

Use an existing local Grok Build CLI installation and login session as a guarded
Codex MCP web-search backend. No xAI API key is required.

This project is community maintained and is not affiliated with or endorsed by
xAI or OpenAI.

## Requirements

- Codex with plugin support.
- Grok Build CLI `>=0.2.93,<0.3.0`, installed as `grok` and already signed in.
- macOS or Linux on Apple Silicon/AArch64 or x86-64.
- `curl` plus either `sha256sum` or `shasum` for the release launcher.

Check the local backend before installing the plugin:

```bash
grok --version
```

## Install

Add this repository as a Codex marketplace, then install the plugin:

```bash
codex plugin marketplace add https://github.com/lich13/grok-build-search.git
codex plugin add grok-build-search@grok-build-search
```

Start a new Codex thread after installation so the new Skill and MCP tools are
loaded.

## Use

Invoke `$grok-search` or explicitly ask Codex to use Grok for a web search. For
example:

```text
Use $grok-search to find the latest stable Rust release and cite the sources.
```

The plugin exposes three MCP tools:

| Tool | Purpose |
| --- | --- |
| `web_search` | Search the public web and return an answer with source URLs. |
| `web_fetch` | Read one known public HTTP(S) URL after network-target validation. |
| `doctor` | Check the local Grok version, optionally with a live search. |

Ordinary search and fetch requests do not call `doctor` first. If Grok fails or
returns no sources, the Skill permits at most one Codex-native web fallback. URL
validation failures never fall back to another backend.

## How it works

The plugin starts a small Rust MCP server. Its launcher selects one of four
GitHub Release binaries, downloads the matching `SHA256SUMS`, verifies SHA-256,
and caches the verified executable under:

```text
${XDG_CACHE_HOME:-$HOME/.cache}/grok-build-search/v0.1.2/
```

The launcher validates the cached binary on every start. A corrupt or modified
cache entry is replaced from the release. The checksum protects download and
cache integrity; trust in the release still comes from this GitHub repository.

The MCP server then locates `grok` through `GROK_BIN`, `PATH`,
`~/.local/bin/grok`, or `~/.grok/bin/grok`. Each operation runs in a separate
Grok process with:

- a 120-second timeout and a maximum of two concurrent processes;
- read-only Grok sandboxing and explicit file/terminal tool denials;
- a private `0600` prompt file instead of query text in process arguments;
- common AI API-key environment variables removed from the child process;
- no retries and no Grok subagents, memory, plans, or automatic updates.

`web_fetch` accepts only public `http` or `https` targets. It rejects URL
userinfo, localhost names, and local, private, documentation, multicast, or
otherwise reserved IP ranges before Grok starts. Search responses are accepted
only when they contain public HTTP(S) source URLs; internal reasoning is not
returned.

## Development overrides

The plugin normally uses the release launcher. These environment variables are
available for development and testing:

| Variable | Purpose |
| --- | --- |
| `GROK_BUILD_SEARCH_MCP_BIN` | Use an already-built MCP executable and skip download. |
| `GROK_BUILD_SEARCH_CACHE_DIR` | Override the launcher cache root. |
| `GROK_BIN` | Select a specific local Grok executable. |

## Build and test

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
python3 -m unittest discover -s tests -p 'test_*.py'
sh -n plugins/grok-build-search/scripts/grok-build-search-mcp
```

Release tags must match the package and plugin version. The Release workflow
builds the four supported platform assets and publishes them with
`SHA256SUMS`.

## License

[MIT](LICENSE)
