# WO-016: Publish to crates.io, with `hatchling-runtime` as the front door

**Status:** CLOSED
**Area:** `crates/hatchling-runtime` (new), `Cargo.toml` (workspace), `crates/*/Cargo.toml`, `deny.toml`, `.github/workflows/ci.yml`, `README.md`, `docs/`
**Issue:** [#26](https://github.com/undergroundrap/hatchling/issues/26)
**Trust boundary:** yes

## Problem

Hatchling isn't on crates.io, so installing it means cloning and building, and Rust developers who search crates.io or lib.rs never find it. The plan for its crates.io names is public (`hatchling-runtime`, and the `hatchling-*` crates), and crates.io names go to whoever publishes first: crates.io doesn't take a name from its owner to give it to someone else. The plain name `hatchling` is already taken by an unrelated project.

Holding the names with empty placeholder crates would be name-squatting, which crates.io's policy forbids. The way to hold them is to publish the real crates.

## Scope

**In scope:**

- **`hatchling-runtime`**, a new crate that installs one command, `hatchling`:
  - `hatchling broker …` and `hatchling agent …` run what `hatchling-broker` and `hatchling-agent` run today, with the same arguments, through the same library entry points;
  - `hatchling --help` lists the subcommands, and `hatchling --version` prints the version;
  - the broker's spawner helpers (`skill-cell`, `agent-run`), which the broker starts by re-running its own executable, work when that executable is `hatchling`.

  The `hatchling-broker` and `hatchling-agent` binaries stay as they are.
- **Publish the shipped crates** at the workspace version: `hatchling-runtime`, `hatchling-broker`, `hatchling-agent`, and every crate they depend on. Development and test crates (`hatchling-dev`, `hatchling-adversarial`, fixtures) stay `publish = false`. Dev-dependencies on unpublished crates must not block publishing.
- **One version for the workspace:** every published crate inherits `version.workspace = true`.
- **Metadata:** each published crate has a description, keywords, categories, a README or crate docs, and a link to the repository. Each internal crate's docs open by saying it is part of Hatchling, with no stability promise on its own, and point to `hatchling-runtime`.
- **The license check keeps working.** Our own crates are exempted by name in `deny.toml` (see the notes), and a third-party AGPL dependency still fails `cargo deny`.
- **CI keeps it publishable:** `cargo publish --workspace --dry-run` and `cargo semver-checks` (against the version on crates.io) run on every pull request.
- **The first publish**, done by the maintainer by hand with a short-lived crates.io token that is revoked afterwards. WO-013 then moves publishing into the release workflow through trusted publishing.
- **The README:** a crates.io badge, a docs.rs badge, and `cargo install hatchling-runtime` as an install option in the quickstart.

**Not in scope:**

- signed binaries, an SBOM, and the release workflow (WO-013);
- stabilizing any internal API (0.x minor versions may break it; each milestone release bumps the minor version);
- renaming crates.

## Acceptance criteria

- [x] `cargo install hatchling-runtime` on a fresh machine installs `hatchling`. The README's quickstart, with `hatchling broker` in place of `hatchling-broker`, gets the agent's answer; run as written.
- [x] A scheduled run and a skill run both work under a broker started as `hatchling broker`, with real cells, since both re-run the broker's executable; tested.
- [x] `hatchling --help` and `hatchling --version` exit 0 with nothing configured; an unknown subcommand exits 64 with usage on stderr; tested.
- [x] `cargo publish --workspace --dry-run` passes in CI, and a pull request that makes a published crate unpublishable fails it.
- [x] `cargo deny check` passes for the workspace, and fails when a third-party AGPL crate is added; shown by a positive control.
- [x] Every crate in the publish set is on crates.io at the workspace version, owned by the maintainer, and its docs build on docs.rs.
- [x] No crates.io token is stored in the repository or its secrets.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Environment: WSL2, Ubuntu 26.04.1 LTS, kernel 6.6.87.2-microsoft-standard-WSL2; Rust 1.98.1 (pinned) and 1.85.0 (MSRV); cargo-deny 0.20.2; cargo-mutants 27.1.0. CI: GitHub's Ubuntu 24.04 runners.

- **Gate:** `HATCHLING_REQUIRE_CELL=1 bash scripts/check.sh`, green on 1.98.1 and on 1.85.0: 1995 passed, 0 failed, each (#27).
- **Front door, real cells:** the broker's spawner suites (`skill_run`, `schedule_run`, `subagents`, `harness`, `harness_cli`, `skill_cell_spawn`), with `HATCHLING_REQUIRE_CELL=1`: 45 passed with `hatchling-broker` as the spawner, and 45 passed with `HATCHLING_TEST_SPAWNER` set to `hatchling`. CI runs both. A hand mutant that drops the front door's helper route fails 8 of them.
- **CLI:** `crates/hatchling-runtime/tests/cli.rs` runs the real binary: `--help`/`-h`/`help` and `--version`/`-V` exit 0 on stdout with nothing configured; no subcommand, or an unknown one, exits 64 with usage on stderr; `broker`, `agent`, the cell name `/agent` and both spawner helpers each reach their own command line.
- **Mutation:** `scripts/mutant-run.sh --base origin/main` over the six changed source files: 11 tested, 10 caught, 0 missed, 0 timed out, 1 unviable (`route` -> `Default::default()`; `Route` has no default). The first launch was refused by the pre-flight's delegation check until unit tests called the moved `cli` functions by name.
- **Publishing:** `cargo publish --workspace --dry-run --locked` packages and verifies 21 crates, locally and in the required "crates.io dry run" CI job.
- **Positive controls**, each on a throwaway copy:
  - a third-party AGPL crate added as a dependency fails `cargo deny check licenses`;
  - a path-only normal dependency fails the publish dry run, and fails `cargo deny`'s wildcard check.
- **First publish (2026-10-11):** the maintainer published `v0.1.1` (`758fdb4`) from a fresh clone of the tag, with a short-lived token. crates.io allowed a burst of 5 new crates, then about one per 10 minutes; all 21 were live by 18:49 Eastern.
  - Every crate's docs built on docs.rs (21 of 21, from docs.rs's status endpoint).
  - The repository has no secrets, and no crates.io token appears in its tree.
- **From crates.io, end to end:**
  - `cargo install hatchling-runtime --version 0.1.1 --locked`, into a clean root with a clean target directory, installed `hatchling` 0.1.1.
  - The README's quickstart, run as root with that binary as both `hatchling broker` and the agent (`HATCHLING_AGENT_BIN` set to it), got the agent's answer from inside a cell running as `hatchling-cell` (uid 999).
- **Follow-up (0.1.2):**
  - publishing moves to the release workflow, through trusted publishing;
  - `cargo semver-checks` runs on every pull request;
  - per-crate keywords and categories;
  - the README badges, and the `cargo install` path in the quickstart.

## Notes

- **The license check depends on `publish = false` today.** `deny.toml`'s `[licenses.private] ignore = true` is what exempts our own AGPL crates from the license allowlist, and it applies only to unpublished crates. Once the crates are publishable, exempt them by name (a `[[licenses.exceptions]]` entry per `hatchling-*` crate), so a third-party AGPL dependency still fails the gate.
- The workspace comment on `publish = false` says why it exists: an accidental publish would muddy the dual license. Publishing becomes deliberate here: one owner, one documented first publish, then WO-013's workflow. The published crates carry `AGPL-3.0-or-later`, and the commercial license is offered separately, as the README says.
- `cargo publish --workspace` (stable since Rust 1.90) publishes in dependency order. The pinned toolchain has it; the 1.85 MSRV only has to build the crates, not publish them.
- Pulled forward from WO-013 so the names are held by real releases, not placeholders.
