# WO-007: Bind agent sessions to the cells the broker spawned

**Status:** QUEUED
**Area:** `crates/hatchling-broker` (serve, cell, run launcher), `crates/hatchling-isolation`, `crates/hatchling-agent`
**Issue:** [#10](https://github.com/undergroundrap/hatchling/issues/10)
**Trust boundary:** yes

## Problem

The broker admits an agent session by its kernel uid alone, and the `cell_id` in the hello is self-asserted. Any host process with the agent's uid can open a full agent session, and a session can log itself under another cell's id. The handshake also has no read timeout, so a connection that never says hello holds a connection slot.

Found in the pre-launch review of the trust boundary (see [Known limitations](../arch/trust-boundary.md#known-limitations)).

## Scope

**In scope:** tying each session to the cell the broker spawned (through an inherited pre-connected socket, or a per-spawn secret over an inherited descriptor); using the broker-known cell id in audit records; a handshake read timeout; refusing `nobody` (65534) as the agent uid at startup, unless explicitly overridden.

**Not in scope:** network transports for agents.

## Acceptance criteria

- [ ] A process with the agent's uid that wasn't spawned as a cell is refused before any request is served; tested.
- [ ] Audit records carry the broker-known cell id; tested.
- [ ] A connection that sends nothing is closed after the timeout and frees its slot; tested.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.
