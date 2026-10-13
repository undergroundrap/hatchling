# WO-010: Schedule ownership and API grant edges

**Status:** CLOSED
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

- [x] A cell's list shows only its own schedules, and it can't cancel another's; tested.
- [x] An API caller with no grants is refused skill invocation, messages and cancels, whatever the owner holds; tested.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Environment: WSL2, Ubuntu 26.04.1 LTS, kernel 6.6.87.2-microsoft-standard-WSL2; Rust 1.98.1 (pinned) and 1.85.0 (MSRV); cargo-mutants 27.1.0.

- **Ownership:** each schedule records its `Creator` (`crates/hatchling-scheduler/src/schedule.rs`): the owner (the local API, or the broker itself) or the agent (a session, a run's follow-up included). `Creator::reaches` decides who may see and cancel what.
- **A cell's list and cancel** (`a_schedule_the_api_makes_is_the_owners_and_a_cell_can_neither_see_nor_cancel_it`, `a_schedule_is_created_as_its_callers_and_cancelled_only_by_who_reaches_it`): an agent session's list shows only the agent's schedules; its cancel of the owner's answers `NotFound`, as for an id that doesn't exist, and the schedule stays; the owner lists and cancels both. An entry stored without a creator reads as the owner's.
- **A grantless API caller** (`a_caller_with_no_grants_is_refused_everything_even_when_the_owner_holds_none`): with an owner holding no grants, a caller holding none is refused a message, a skill, a cancel and a create, each `NotGranted`.
- **Gate:** `HATCHLING_REQUIRE_CELL=1 bash scripts/check.sh`, green on 1.98.1 and 1.85.0: 2,066 passed, 0 failed, each.
- **Mutation:** `scripts/mutant-run.sh --base origin/main` over `schedule_ops.rs`, `api_ops.rs`, `schedule.rs` and `serve.rs`: 188 tested, 148 caught, 1 missed, 0 timed out, 39 unviable. The miss was the message-length bound in the API's `send` (`>` to `>=`), older code inside the changed hunk; a test at exactly the bound now kills it. Rerun over `api_ops.rs` with it: 22 tested, 16 caught, 0 missed, 0 timed out, 6 unviable, 0 orphans. The first launch was refused by the pre-flight until unit tests called `SealedScheduleStore::cancel` and `create_schedule` by name.
- **Hand mutants**, each failing its test: a cell lists every schedule; a cell cancels any; a create doesn't record its creator; a grantless API caller passes; the owner reaches only its own.

## Notes

- **Cells aren't told apart from each other.** Every agent session runs as the one agent uid and names its own cell, so "the agent's" schedules are every agent session's. Telling one cell's from another's waits on [WO-007](workorder-007-bind-sessions-to-cells.md), which binds a session to the cell the broker spawned; the trust boundary says so in WO-007's row.
- **Hooks** are created and cancelled only through agent sessions, so they have one creator and are out of this WO's scope.
- **The API's other actions** (those with no backend in this build) are refused as `NotGranted` to a caller with no grants, before their backend is looked for.
