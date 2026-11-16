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

## Adding, splitting or renaming a crate

A new crate is a decision about crates.io as well as code. Say which it is in the pull request.

**Unpublished** (a tool, a fixture, something only built for a cell's image): `publish = false` in its `Cargo.toml`, and the module map says *Not published.* with what it is.

**Published** (anything a published crate depends on must be published too):

1. Its `Cargo.toml` takes the workspace's fields like the others (`version`, `edition`, `rust-version`, `license`, `authors`, `repository`, `homepage`, `readme`, `[lints]`), with its own `description`, `keywords` and `categories`.
2. It gets a line in the root `Cargo.toml`'s `[workspace.dependencies]`: `name = { version = "<workspace version>", path = "crates/<name>" }`. Other crates depend on it through that line.
3. The publish scripts find it by themselves: `scripts/publish-crates.sh` and `scripts/setup-trusted-publishing.sh` read `cargo metadata` and skip `publish = false`. There is no list to edit.
4. **Its first publish is the maintainer's, by hand.** crates.io lets only a crate's owner create it, so the release workflow publishes everything else and skips the new crate, and any crate that depends on it, with a warning. Tell the maintainer before the release is cut, in the pull request and on its issue. After the release they follow [docs/releasing.md, "Adding a crate"](docs/releasing.md#adding-a-crate): a one-day token, the first publish, the token revoked, then trusted publishing set up for it. An agent walks them through it and never handles the token: they type it into a terminal they opened themselves.

**Either way:**

- A row in [the module map](docs/arch/module-map.md), whose count of unpublished crates stays right (the audit checks every crate has a row).
- Its dependencies reviewed in [docs/dependency-review.md](docs/dependency-review.md).
- If it runs inside a cell, its name in the audit's `CELL_CRATES` (`crates/hatchling-dev/src/checks/arch.rs`), so it can never depend on the trusted side.
- **Splitting a crate** is a move first and a change after: the moved code byte for byte, its mutants listed before and after and the same, then any change in its own pull request (as #475 did for `hatchling-room`).
- **Renaming a published crate** makes a new crate on crates.io: the old name stays published at its last version. Avoid it; if it's needed, it follows the steps above as a new crate, and the old one's README says where it went.

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
