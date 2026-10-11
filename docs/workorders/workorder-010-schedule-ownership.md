# WO-010: Schedule ownership and API grant edges

**Status:** QUEUED
**Area:** `crates/hatchling-broker` (schedule ops, API ops, API auth)
**Issue:** [#13](https://github.com/undergroundrap/hatchling/issues/13)
**Trust boundary:** yes

## Problem

A cell can list and cancel every schedule, including ones the owner or the API created. On the API, the check that a caller holds the owner's whole grant set passes vacuously when the owner holds no schedule grants, so a caller with no grants could invoke skills.

Found in the pre-launch review of the trust boundary (see [Known limitations](../arch/trust-boundary.md#known-limitations)).

## Scope

**In scope:** recording each schedule's creator, and letting a cell list and cancel only its own; refusing an empty set in the owner-set check; making the module documentation match.

**Not in scope:** sharing schedules between cells.

## Acceptance criteria

- [ ] A cell's list shows only its own schedules, and it can't cancel another's; tested.
- [ ] An API caller with no grants is refused skill invocation, messages and cancels, whatever the owner holds; tested.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.
