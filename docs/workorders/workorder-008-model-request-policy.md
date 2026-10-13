# WO-008: A request-body policy for model vendors

**Status:** CLOSED
**Area:** `crates/hatchling-egress`, `crates/hatchling-broker` (harness relay)
**Issue:** [#11](https://github.com/undergroundrap/hatchling/issues/11)
**Trust boundary:** yes

## Problem

The proxy and the harness relay check where a request goes, not what it asks for. A model vendor's API can fetch URLs on the caller's behalf: image and document sources given by URL, server-side web and fetch tools, remote tool servers. A hostile cell can ask the vendor to fetch an attacker's URL, carrying data out to a host that isn't on the allowlist.

Found in the pre-launch review of the trust boundary (see [Known limitations](../arch/trust-boundary.md#known-limitations)).

## Scope

**In scope:** a per-API body policy for model destinations: parse the JSON, refuse URL-sourced content, server-side tools and remote tool servers unless explicitly allowed, and allowlist beta headers; documenting the policy in the trust boundary.

**Not in scope:** filtering the content of prompts.

## Acceptance criteria

- [x] A request carrying a URL-sourced image or document, a server-side fetch or search tool, or a remote tool server is refused before it leaves, on every supported API; tested.
- [x] Ordinary chat-completions and tool-calling requests pass unchanged; tested.
- [x] The trust boundary describes what the policy allows.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Environment: WSL2, Ubuntu 26.04.1 LTS, kernel 6.6.87.2-microsoft-standard-WSL2; Rust 1.98.1 (pinned) and 1.85.0 (MSRV); cargo-mutants 27.1.0.

- **The policy** is `crates/hatchling-egress/src/policy.rs`, applied by the egress proxy (`Proxy::forward`, before the credential swap) and by the harness relay (`relay_one`, before anything reaches the vendor).
- **Refused, on every supported API** (`content_given_by_url_is_refused_wherever_it_is`, `a_tool_the_vendor_runs_is_refused`, `remote_tool_servers_and_the_vendors_own_search_are_refused`): an image or document by URL in chat completions, Anthropic messages (including inside a tool result) and responses; audio, video and document URLs; Anthropic's web search, web fetch and code execution tools; OpenAI's web search, file search, MCP, image generation and code interpreter tools, and a tool type no vendor has shipped; `mcp_servers`, `web_search_options`, `plugins`, a stored prompt by `id`, an `:online` model; the same inside a batch of requests.
- **Passes unchanged** (`ordinary_chat_messages_and_responses_requests_pass`): a chat-completions request with tools, tool calls and an inline image; an Anthropic messages request shaped as Claude Code sends one (its own tools, a tool call whose input names URLs, thinking, caching, an inline image, a tool result); a responses request shaped as Codex sends one (function, custom and `local_shell` tools, function calls, reasoning); an embeddings request. A proxy test sends one ordinary and one fetching request through a real local endpoint: the ordinary one arrives byte for byte, the fetching one never leaves. A relay test does the same against a scripted vendor, which receives nothing.
- **Parser differentials** (`a_body_that_is_not_json_or_names_a_key_twice_is_refused`): a body that isn't JSON, or whose object names a key twice, is refused.
- **Known vendors** (`a_model_vendor_is_open_to_its_model_endpoints_only`): batches, files, assistants and Gemini's native API are refused on a vendor's host.
- **Gate:** `HATCHLING_REQUIRE_CELL=1 bash scripts/check.sh`, green on 1.98.1 and 1.85.0: 2,060 passed, 0 failed, each.
- **Mutation:** `scripts/mutant-run.sh --base origin/main` over `policy.rs`, `proxy.rs`, `error.rs`, `harness_relay.rs` and `serve.rs`: 271 tested, 233 caught, 0 missed, 0 timed out, 38 unviable, 0 orphans. The first launch was refused by the pre-flight until unit tests called each of the policy's helpers and the strict reader's methods by name.
- **Hand mutants**, each failing its test: the proxy skips the policy; the relay skips it; the policy reads a tool call's input; it allows a duplicate key; it allows a tool the vendor runs; it opens a vendor's other APIs; it allows a `url` source.
- **The trust boundary** describes the policy: [The model request policy](../arch/trust-boundary.md#the-model-request-policy).

## Notes

- **Beta headers pass.** A beta only unlocks request fields, and the fields that fetch are refused whichever betas are set; allowlisting betas would break each harness update for nothing the body check doesn't already cover.
- **What it costs.** A harness set to use its vendor's web search or fetch gets those requests refused with a 403 that says why. A new vendor feature that fetches stays refused until it's reviewed and listed in `policy.rs`.
- **Not the vendor itself.** What a cell sends to the operator's chosen vendor, under the operator's account, is out of scope, as the WO says: the policy stops the vendor fetching on the cell's behalf, not the conversation reaching the vendor.
