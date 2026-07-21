# grok-build-search

Use an existing local Grok Build CLI installation and login session as a guarded
Codex MCP web-search backend. No xAI API key is required.

This project is community maintained and is not affiliated with or endorsed by
xAI or OpenAI.

## Requirements

- Codex with plugin support.
- Grok Build CLI with `--tools`, `--always-approve`, and structured JSON output,
  installed as `grok` and already signed in.
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
${XDG_CACHE_HOME:-$HOME/.cache}/grok-build-search/v0.1.8/
```

The launcher validates the cached binary on every start. A corrupt or modified
cache entry is replaced from the release. The checksum protects download and
cache integrity; trust in the release still comes from this GitHub repository.

The MCP server then locates `grok` through `GROK_BIN`, `PATH`,
`~/.local/bin/grok`, or `~/.grok/bin/grok`. Each operation runs in a separate
Grok process with:

- no plugin-level process deadline, no plugin-level agent-turn limit, a
  365-day Codex MCP host ceiling, and a maximum of two concurrent processes;
- an explicit `web_search,web_fetch` tool allowlist, preventing unrelated Grok
  built-in tools from entering the backend request;
- automatic approval is limited to that allowlist so headless web tools cannot
  be cancelled by a closed prompt;
- read-only Grok sandboxing and explicit file/terminal tool denials;
- a private `0600` prompt file instead of query text in process arguments;
- a unique temporary `GROK_HOME` and `HOME`, with the existing authentication
  file plus copied native configuration and model metadata;
- native Grok model and reasoning-effort resolution: the plugin does not pass
  `--model` or `--reasoning-effort`, and does not validate, map, or clamp effort;
- Codex's `model_reasoning_effort` controls only the outer Codex task and is not
  forwarded to Grok;
- common AI API-key environment variables removed from the child process;
- no retries and no Grok subagents, memory, plans, or automatic updates.

Grok runs until it exits or the MCP call is cancelled. Cancellation terminates
the Grok process group before the isolated runtime is removed.

The plugin parses `grok --version` for diagnostics but does not reject valid
semantic versions by a numeric range. Unsupported CLI flags or backend protocol
contracts are reported through Grok's explicit process failure.
Invalid native model or effort configuration is likewise left to Grok and is
not retried or silently changed by the plugin.

The temporary Grok state, including sessions, prompt history, memory indexes,
and logs, is removed after every operation. Concurrent operations hold separate
activity locks, and abandoned state is retried at the start of the next tool
call. A successful response includes a `CLEANUP_DEFERRED` warning if temporary
state could not be removed immediately.

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
