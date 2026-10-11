# Contributing to Hatchling

This project holds itself to an unusually high bar. If that sounds
intimidating, good — it means the bar is working. Here's what we
expect and why.

## The short version

1. Every change is tested. Not "has tests" — **proven** by tests.
2. Every backend PR carries mutation-testing evidence.
3. Every UI PR carries screenshots.
4. Docs must match code. The wire-format docs are checked by the build;
   the rest is on you and the reviewer.
5. Be kind. Be precise. No drive-by criticism without evidence. See the
   [code of conduct](CODE_OF_CONDUCT.md).

## Work orders and pull requests

Core work is planned as **work orders**: specs in
[`docs/workorders/`](docs/workorders/) with a fixed scope, acceptance
criteria and gates. The maintainers use them to drive the core; you can
pick one up (comment on its issue first) or propose one in an issue.

You don't need a work order to send a pull request. Bug fixes, tests,
docs and focused features are welcome as they are, and meet the same
bar as below.

## Setting up

```bash
git clone https://github.com/undergroundrap/hatchling
cd hatchling
cargo install cargo-deny --version 0.20.2 --locked
cargo install cargo-mutants --version 27.1.0 --locked
git config core.hooksPath .githooks   # optional: run the gate before every push
```

The pinned toolchain installs itself from `rust-toolchain.toml`. Add the
MSRV with `rustup toolchain install 1.85.0`. Isolation tests need Linux
5.11+ with unprivileged user namespaces (or WSL2:
[docs/testing-on-wsl2.md](docs/testing-on-wsl2.md)). Set
`HATCHLING_REQUIRE_CELL=1` so they fail instead of skipping when the
kernel can't build a cell. On Ubuntu 23.10 and later, AppArmor restricts
unprivileged user namespaces, and the cell tests skip (or, with that
variable set, fail) until you lift it:
`sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`. CI
does the same.

## Backend (Rust)

### Mutation testing is mandatory

We run [cargo-mutants](https://github.com/sourcefrog/cargo-mutants) with
a **kill floor of 1.0**: every mutant of your changed code must be
killed by a test or rigorously proven equivalent. Not 90%. Not 99%.
All of them.

- Run the mutants for every file you changed:
  `scripts/mutant-run.sh --base <your base commit> -- <changed .rs files>`.
- Every changed non-test function must be called **by name** from test
  code. The run's pre-flight (`cargo run -p hatchling-dev -- preflight
  --base <commit>`) checks this and refuses to start otherwise.
- Equivalent mutants need a written proof in `.cargo/mutants.toml`, not a
  hand-wave. The proof must show why the mutation cannot change
  observable behavior.
- If your change is too large to mutate in reasonable time, split
  the PR. That's a signal, not an obstacle.

### What your PR must include

- **Mutation results:** the mutant count, kill count, and how each
  survivor was resolved (killed or proven equivalent with proof).
- **Gate logs:** `bash scripts/check.sh` green on the pinned toolchain
  **and** the MSRV (`RUSTUP_TOOLCHAIN=1.85.0 bash scripts/check.sh`).
  It runs fmt, clippy with `-D warnings`, the build, the tests, the
  seed check and `cargo deny`. Paste the summary lines.
- **New dependencies:** justify them in the PR. Every new crate
  dependency gets a review entry in `docs/dependency-review.md`.
  Prefer the standard library. Prefer fewer dependencies.

### Code conventions

- Typed errors, not stringly-typed failures. New error types go
  through the crate's error module.
- Newtypes for domain concepts (no bare `String` for identifiers,
  no raw `u64` for timestamps).
- `main.rs` stays thin. Logic lives in libraries, tested directly.
- No `unwrap()`, `expect()`, `panic!`, indexing or `as` casts in
  production code: clippy denies them. Tests may use `unwrap()` and
  `expect()`.
- `unsafe` is denied everywhere except `hatchling-sys`. There, each
  block needs a `// SAFETY:` comment and a second pair of eyes.
- An `#[allow]` or `#[expect]` names its lint and gives a
  `reason = "..."`. There are no file-wide allows.

## Frontend (TypeScript)

### Strictness is non-negotiable

- `strict: true` and `noUncheckedIndexedAccess: true`. No
  exceptions, no per-file disables.
- `npm run lint` (the project's own linter) must pass.
- `cd frontend && npm ci && npm run check` must be green: typecheck,
  lint, design and protocol checks, formatting, and tests.
- Visual changes follow [`frontend/STYLE_GUIDE.md`](frontend/STYLE_GUIDE.md).

### What your PR must include

- **Screenshots** for any UI change. Before/after for visual
  changes. No screenshot, no merge.
- **Coverage:** the live protocol adapter stays at 100% line, branch
  and function coverage (`npm run test:live-coverage`, part of
  `npm run check`).

### Architecture

- UI talks only to the `ProtocolAdapter` boundary. No direct
  backend calls from components.
- Protocol types are generated from the schema. Never hand-edit
  generated files: after changing a wire type, run
  `cargo run -p hatchling-dev -- protocol-schema --write`, then
  `cd frontend && npm run protocol:generate`.
- The `ProtocolAdapter` contract (`frontend/docs/PROTOCOL-LOCK.json`)
  is frozen. Changing it needs a maintainer's approval first; never
  regenerate it to make a check pass.

## Documentation

Docs are tested. `docs/local-api.md`'s examples and tables are
verified by `crates/hatchling-core/tests/local_api_doc.rs` — if
your change makes the docs lie, `check.sh` fails.

When you change behavior, update the docs in the same PR. A PR
that changes behavior without updating docs is incomplete.

## Security

If your change touches anything in the trust boundary
(authentication, isolation, secrets, network egress, permissions),
say so explicitly in the PR description. Security-relevant PRs
get extra review. When in doubt, flag it — over-flagging is fine,
under-flagging is not.

Never commit secrets, keys, tokens, or credentials.

To report a vulnerability, don't open a PR or an issue: see
[SECURITY.md](SECURITY.md).

## Commit messages

- Use [Conventional Commits](https://www.conventionalcommits.org/)
  with the area as the scope: `feat(vault):`, `fix(broker):`,
  `docs(api):`, etc.
- One logical change per commit.
- Reference the tracking issue if there is one.

## License

Hatchling is licensed under AGPL-3.0-or-later. By contributing,
you agree your contributions will be licensed the same way.
