# WO-042: Keep the docs and the queue honest in CI

**Status:** QUEUED
**Area:** `crates/hatchling-dev` (the audit's checks), `.github/workflows/` (a check of pull requests and issues), `README.md`, `docs/workorders/`
**Issue:** [#487](https://github.com/undergroundrap/hatchling/issues/487)
**Trust boundary:** no

## Problem

On 2026-11-16 a read of the docs against the code found about twenty-five places the docs had fallen behind (the session protocol said v13 at v31; the install said four system users of five), and the queue had stopped: two work orders in progress whose work had shipped, one never written, issues open for work that had merged. Each was a fact the code or the queue knows that nothing compared with what the docs say. CI already checks some (every crate has a row in the module map); the rest drifted.

## Scope

**In scope:**

- In `hatchling-dev audit`: the versions the docs state (`docs/arch/protocol.md`, `docs/session-protocol.md`) against `PROTOCOL_VERSION` and `UI_SCHEMA_VERSION`; the system users `docs/install.md` names against `packaging/sysusers.d`.
- In `hatchling-dev audit`, the queue: every work-order file has a row with the same status and every row a file; numbers run without a gap; every work order names an issue; a `CLOSED` one has every acceptance item ticked or struck through with its reason.
- A CI job, read-only on GitHub: a `CLOSED` work order whose issue is open, or an open one whose issue is closed, fails it; a pull request that changes `crates/` and says neither `Closes #N` nor `Part of #N`, and isn't labelled `trivial`, fails it.
- The README says the test count in words that don't go stale, or the count is made by the gate.

**Not in scope:** checking prose for meaning; a model in CI.

## Acceptance criteria

- [ ] Each check fails on a deliberately stale tree (a wrong version, a missing queue row, a gap in numbers, a closed work order with an unticked item, a missing issue) and passes on the real one; each is a test.
- [ ] The issue check fails on a fixture of a closed work order with an open issue, and passes on the repository as it is.
- [ ] The pull-request check fails on a body with no reference and passes with `Closes #N`, `Part of #N` or the `trivial` label.
- [ ] `AGENTS.md` and `MAINTAINING.md` say what each check holds.

## Gates

As [TEMPLATE.md](TEMPLATE.md).

## Evidence

Filled in at close.
