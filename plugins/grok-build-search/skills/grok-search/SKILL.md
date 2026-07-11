---
name: grok-search
description: Use when the user invokes $grok-search or explicitly asks to use Grok for web search, research, source discovery, or fetching a known public HTTP(S) URL through the local Grok Build CLI.
---

# Grok Search

## Overview

Route explicit Grok requests through the `grok-build-search` MCP first. Preserve its public-web safety boundary and use Codex-native web access only for backend failures.

## Route The Request

Call one primary tool:

| Need | Tool |
| --- | --- |
| Discover sources or answer a research question | `web_search` |
| Read a known public HTTP(S) URL | `web_fetch` |
| Diagnose installation or version support at the user's request | `doctor` |

Do not call `doctor` after an ordinary `web_search` or `web_fetch` failure. Do not invoke the `grok` executable directly.

## Handle The Result

- When `ok` and `verified` are true and `sources` is non-empty, answer from `answer` and cite the returned source URLs. Do not perform another search.
- For `NO_SOURCES`, `GROK_NOT_FOUND`, `GROK_UNSUPPORTED_VERSION`, `GROK_TIMEOUT`, `GROK_EXIT_FAILED`, or `BAD_GROK_JSON`, make one fallback attempt with Codex's native web search or fetch capability. Do not retry the MCP call. State briefly that native fallback was used; do not attribute fallback results to Grok.
- If Codex's native web capability is absent, state that fallback is unavailable and stop. Do not answer from model memory or provide unverified source URLs.
- For `INVALID_URL` or `PRIVATE_URL`, stop and report the rejection. Never bypass it with native search/fetch, a browser, `curl`, another shell command, or direct Grok invocation.
- For `INVALID_QUERY`, `INVALID_INSTRUCTIONS`, or `INVALID_MAX_CHARS`, correct the request when unambiguous; otherwise ask the user for a valid value. Do not switch backends.

Treat structured responses with `ok: false` as failures even when the MCP transport itself returned content.

## Output Contract

Keep the response focused on the requested answer. Include public source links, disclose native fallback when it occurs, and omit Grok's internal reasoning, session details, and tool diagnostics unless the user requested diagnosis.

## Common Mistakes

- Repeating a failed Grok call or probing with `doctor` wastes time and can repeat the same failure.
- Falling back after `INVALID_URL` or `PRIVATE_URL` defeats the plugin's SSRF boundary.
- Calling `curl` or the Grok CLI is not a Codex-native web fallback.
