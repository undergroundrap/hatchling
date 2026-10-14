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
