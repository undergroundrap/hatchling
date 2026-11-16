# AGENTS.md

Instructions for AI agents working in this repository.

## Project conventions

- **Read the style guide:** `frontend/STYLE_GUIDE.md` is the authoritative
  reference for frontend design decisions. Any UI change must conform to it.
- **Backend changes** follow `CONTRIBUTING.md`: mutation testing with kill
  floor 1.0, `check.sh` green on the pinned toolchain and the 1.85.0 MSRV,
  no `unwrap()`/`expect()` in production paths.
- **Core work** is planned in `docs/workorders/`. Stay inside a work
  order's scope; anything outside it is a different work order.
- **Docs must match code.** If you change behavior, update the docs in the
  same commit.
- **Security fixes** follow `CONTRIBUTING.md#security-fixes` and `docs/testing-patterns.md`: name the attack, hand-break the defense, and prove the affected boundary.
- **Maintainer work** follows [MAINTAINING.md](MAINTAINING.md) for issue/WO tracking, labels, milestones, reviews, the CLA, changelog and branches.

- **App and installer checks** follow [Testing with an AI agent](docs/testing-with-an-agent.md): use a disposable install, prove connected behavior and preserve the evidence. Read the current CI workflows for the complete gates.

## Work orders, issues and pull requests

The details are in [MAINTAINING.md](MAINTAINING.md#issues-work-orders-and-milestones)
and [the queue](docs/workorders/README.md). These are the rules to work by.

**Which one a piece of work needs:**

| The work | It needs |
| --- | --- |
| A typo, a doc fix, a one-line fix, a test that only adds coverage | A pull request, no issue |
| A bug: something does what it shouldn't, or doesn't do what the docs say | An issue labelled `bug` first, then a pull request that says `Closes #N` |
| A focused change of behavior one pull request can finish, outside the trust boundary | An issue, then a pull request that closes it |
| Anything touching the trust boundary (isolation, the vault, egress, approvals, the broker's privileges, the protocol), or larger than one pull request | A work order **and** its tracking issue, before any code |
| A direction or idea nobody has agreed to build | Neither: it stays a candidate in the queue, or with the maintainer, until it's agreed |

**Before work starts:**

1. Find the work's issue or work order. If there isn't one and the table says it needs one, make it first.
2. A new work order takes the next unused number in [the queue](docs/workorders/README.md), is written from [TEMPLATE.md](docs/workorders/TEMPLATE.md), and gets a tracking issue titled `WO-NNN: <title>`. The spec links the issue; the issue links the spec.
3. Label the issue (`work order`, `trust boundary` where it applies, `bug` for a bug) and put it on the release's milestone once its scope is agreed.
4. Mark the work order and its queue row `IN-PROGRESS`, and say on the issue who is doing it.

**While it's under way:**

- Scope is fixed. A new problem found on the way gets its own issue or work order; it isn't added quietly.
- A pull request that does part of a work order says `Part of #N` (the tracking issue) and names the WO. It doesn't close the issue.
- Each slice commits its evidence record under `docs/evidence/`, and the work order links it.

**When it's done:**

- The pull request that finishes it says `Closes #N` and names the WO.
- In the same pull request, the work order's acceptance criteria are ticked against the evidence that proves each one (or say plainly which isn't proven), its status is `CLOSED`, and its queue row matches.
- After the merge: check the issue closed, and close the milestone when all its work is done.

**Never:** leave a merged work order's issue open, reuse a WO number, or let work that needed a work order ship without one. If that happened, write the work order after the fact, from what shipped, and close it the same way.

## Repository layout

- `crates/` — Rust workspace. See `docs/arch/module-map.md` for what each
  crate does.
- `frontend/` — TypeScript UI. Talks only to the `ProtocolAdapter` boundary.
- `docs/` — Documentation. `docs/arch/` has the architecture overview.
- `scripts/` — Developer scripts: `check.sh` (the gate) and
  `mutant-run.sh` (supervised mutation runs).

## Key invariants

- The `hatchling-dev` crate is never a dependency of shipped code.
- Protocol types are generated from Rust via schemars — never hand-edit
  `frontend/protocol/schema.json` or the generated TypeScript.
- `AGENTS.md` files (this one and `frontend/AGENTS.md`) must reference
  `STYLE_GUIDE.md` — the frontend design gate checks this.

## Before you push

1. `bash scripts/check.sh` — must be green.
2. `cd frontend && npm ci && npm run check` — must be green.
3. If you touched protocol types: `cargo run -p hatchling-dev -- protocol-schema --write`,
   then `cd frontend && npm run protocol:generate`.
