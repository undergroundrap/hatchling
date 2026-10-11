# WO-008: A request-body policy for model vendors

**Status:** QUEUED
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

- [ ] A request carrying a URL-sourced image or document, a server-side fetch or search tool, or a remote tool server is refused before it leaves, on every supported API; tested.
- [ ] Ordinary chat-completions and tool-calling requests pass unchanged; tested.
- [ ] The trust boundary describes what the policy allows.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.
