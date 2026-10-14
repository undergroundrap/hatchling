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
5. A user-visible change adds its line to [`CHANGELOG.md`](CHANGELOG.md)
   under **Unreleased**, in the same PR.
6. Be kind. Be precise. No drive-by criticism without evidence. See the
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

### Adding a crate

Most new code is a module in an existing crate, behind a feature if
it's optional. A new crate is for code that brings heavy dependencies
most users don't need (a cloud or secret-manager client, say) or that
ships its own program. Each published crate costs:

- a `[workspace.dependencies]` entry in the root `Cargo.toml`, and
  `version.workspace = true` in its own manifest;
- an `exceptions` entry in `deny.toml`, so our own AGPL license passes
  while a third-party AGPL dependency still fails;
- a manual first publish by the maintainer (crates.io only lets the
  owner create a crate), then trusted-publishing setup for it on
  crates.io, after which releases publish it automatically.

A development or test crate sets `publish = false` instead, and needs
none of that. The required "crates.io dry run" check fails a pull
request that leaves a published crate unpublishable.

How releases and first publishes work, step by step:
[docs/releasing.md](docs/releasing.md).

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

The diagram in the README's "How it works" is part of the docs. A
PR that changes what crosses the boundary (a new way out of a cell,
a new place a key goes, a component moving between sides) updates
it in the same PR.

## Security

If your change touches anything in the trust boundary
(authentication, isolation, secrets, network egress, permissions),
say so explicitly in the PR description. Security-relevant PRs
get extra review. When in doubt, flag it — over-flagging is fine,
under-flagging is not.

### Security fixes

**Name the threat first.** Start a trust-boundary PR with the attacker,
the hole they can exploit, and the outcome the fix prevents. Then explain
how the fix closes that path. A new defense needs a concrete attack.

**Hand-break the fix.** In a source-only copy, remove or bypass the
protection and show that the new test fails for the expected reason.
Report the patch, exact test command, failed assertion and test name in
the PR. A build error, unrelated failure or a test process killed by the
runner's timeout is not that proof. A bounded assertion that the attacker
survived past a deadline is valid.
Restore the protection and show the test passes. Mechanical mutation
testing is still required; it does not replace this check.

**Attack each new cell defense in a real cell.** Use the production
spawn path and the profile the affected program uses:

1. Add an `Attack` variant, its parsing/slug cases, and an `Outcome`
   when the attack reports a result in
   [`hatchling-adversarial`](crates/hatchling-adversarial/src/lib.rs).
2. Implement the attack in [`probe.rs`](crates/hatchling-adversarial/src/probe.rs)
   and update its unit/report tests. Bound resource-consuming attacks;
   the probe must not exhaust the host when the defense is broken.
3. Add a check to the broker's
   [`adversarial.rs`](crates/hatchling-broker/tests/adversarial.rs) and
   register it in `CHECKS`. Assert kernel results (errno, exit signal,
   limits, membership) and cleanup, not just a report saying "safe".
   For a signal verdict, verify the probe's readiness marker so a
   startup failure cannot impersonate the defended attack.
4. Build with `cargo build --workspace --locked`, then run
   `HATCHLING_REQUIRE_CELL=1 cargo test -p hatchling-broker --locked --test adversarial`.
   Report the adversarial summary as well as the gate's test counts.
   For cgroup tests, provision a delegated parent as in
   [Cell limits](README.md#cell-limits), set `HATCHLING_TEST_CGROUP`,
   and also set `HATCHLING_REQUIRE_CGROUP=1` so they cannot skip.

For a defense outside the cell boundary, describe and test its attack at
the affected boundary instead. Trust-boundary changes receive independent
review before merge. Worked examples: [#52](https://github.com/undergroundrap/hatchling/pull/52)
(supervisor death), [#53](https://github.com/undergroundrap/hatchling/pull/53)
(hard rlimits), and [#54](https://github.com/undergroundrap/hatchling/pull/54)
(aggregate limits and descendant cleanup).

**Diagnose flakes.** Record the failing test, command and environment;
prove the cause and fix it in a separate scoped commit. Do not rerun
until green and present the last run as proof. Repetition after a fix
measures it; it does not replace diagnosis. See
[testing patterns](docs/testing-patterns.md) for the traps behind these rules.

Never commit secrets, keys, tokens, or credentials.

To report a vulnerability, don't open a PR or an issue: see
[SECURITY.md](SECURITY.md).

## Commit messages

- Use [Conventional Commits](https://www.conventionalcommits.org/)
  with the area as the scope: `feat(vault):`, `fix(broker):`,
  `docs(api):`, etc.
- One logical change per commit.
- Reference the tracking issue if there is one.

## License and CLA

Hatchling is dual-licensed: AGPL-3.0-or-later, and a commercial license
for organizations that need other terms (see the README). So that both
can cover your work, contributions are accepted under the
[Contributor License Agreement](CLA.md).

- On your first pull request, the CLA bot comments with a sentence to
  reply with. Reply with it exactly and the check passes. You sign
  once, for all your future contributions.
- You keep the copyright in your work, and every contribution in a
  public release stays available under the AGPL.
- If your employer may own your work, get its permission first, or
  sign for the organization (section 8 of the CLA).
