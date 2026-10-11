# WO-009: Egress correctness fixes

**Status:** QUEUED
**Area:** `crates/hatchling-egress`, `crates/hatchling-broker` (serve, config, harness relay), `crates/hatchling-core` (framing)
**Issue:** [#12](https://github.com/undergroundrap/hatchling/issues/12)
**Trust boundary:** yes

## Problem

A group of smaller egress gaps:

- Two allowlist entries can map to the same vault secret name (`api.example.com` and `api-example.com`), so one host could receive the other's credential.
- A credential an upstream echoes back is returned to the cell unscrubbed.
- The harness relay buffers whole request bodies, up to about 1 GiB per run.
- The allowlist is re-resolved on every session instead of once at startup.
- Numeric host forms (`2130706433`) pass the no-IP-literal check.
- A response larger than the 64 KiB session frame (about 48 KiB of body) fails, although the proxy allows 1 MiB.

Found in the pre-launch review of the trust boundary (see [Known limitations](../arch/trust-boundary.md#known-limitations)).

## Scope

**In scope:** refusing colliding secret names at startup (or an injective name encoding); scrubbing the credential from responses; streaming or budgeting relay bodies; resolving the allowlist once and sharing it; refusing numeric host forms; making large responses work, or documenting and enforcing the real bound.

**Not in scope:** new egress features.

## Acceptance criteria

- [ ] Each gap above has a test that fails before its fix and passes after.
- [ ] A 512 KiB model response reaches the agent intact, or the bound is documented and enforced at the proxy.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.
