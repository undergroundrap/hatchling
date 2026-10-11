# WO-005: Tighten the seccomp filter and the cell's groups

**Status:** QUEUED
**Area:** `crates/hatchling-sys` (seccomp, process, idmap), `crates/hatchling-isolation`
**Issue:** [#8](https://github.com/undergroundrap/hatchling/issues/8)
**Trust boundary:** yes

## Problem

`socket` is restricted by address family, but `socketpair` isn't, so a cell can create sockets of other families through it. A root broker's supplementary groups carry into the cell, and once `setgroups` is denied, the cell can't drop them. A root broker's id map is refused only when it would map uid 0, not when its count would cover other host accounts.

Found in the pre-launch review of the trust boundary (see [Known limitations](../arch/trust-boundary.md#known-limitations)).

## Scope

**In scope:** allowing `socketpair` for `AF_UNIX` only; calling `setgroups(0, NULL)` in the child before the user namespace is created, when the broker is root; limiting a root broker's id-map count to what the cell needs.

**Not in scope:** a wider redesign of the filter profiles.

## Acceptance criteria

- [ ] `socketpair` with any family but `AF_UNIX` fails in a cell; a real-cell test.
- [ ] A root broker's cell has no supplementary groups (its `Groups:` line is empty); a real-cell test run as root.
- [ ] The seccomp catalog and its tests are regenerated and agree.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.
