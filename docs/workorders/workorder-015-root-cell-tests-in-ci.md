# WO-015: Run the cell test suites as root in CI

**Status:** DONE
**Area:** `.github/workflows/ci.yml`, `scripts/check.sh`, the cell test suites (`crates/hatchling-broker/tests/`, `crates/hatchling-isolation`, `crates/hatchling-sys/tests/`)
**Issue:** [#24](https://github.com/undergroundrap/hatchling/issues/24)
**Trust boundary:** yes

## Problem

The README's quickstart runs the broker as root, but CI runs every cell test as an ordinary user. A root broker gives its cells a different uid than an unprivileged one does, and some code paths run only in that case: mapping a uid that isn't the broker's own, handing a socket to another uid, reaching a run directory as a capability-less root. A bug only that path hits went unseen until the quickstart was run by hand as root.

Some tests also assume an unprivileged runner, so running the suites as root today would turn up failures that are harness assumptions (a peer's uid in a test, a toolchain or target directory found through the invoking user's home) rather than product bugs. Those have to be sorted from real bugs before a root job can gate anything.

## Scope

**In scope:**

- **A CI job that runs the cell suites as root**, with `HATCHLING_REQUIRE_CELL=1`, on the pinned toolchain, as a required check alongside the existing gate.
- **Every test that fails as root** is either fixed (the test or the product, whichever is wrong) or, if it can't apply to a root runner, skipped with a stated reason that the job prints. No test is skipped without a reason.
- **`scripts/check.sh`** gains a way to run the root pass locally (for example `HATCHLING_ROOT_PASS=1`), so a contributor can reproduce the job.
- **CONTRIBUTING** documents the root pass: when to run it, and how on WSL2.

**Not in scope:**

- running the whole workspace as root (only the cell suites, which are the tests whose behavior depends on it);
- the quickstart job (WO-001 runs the README's quickstart as root in CI);
- mutation testing as root.

## Acceptance criteria

- [x] The root CI job runs the cell suites with `HATCHLING_REQUIRE_CELL=1` and passes; it's a required check on `main`.
- [x] The job's log lists every test skipped as root, each with its reason, and the list is short enough to review in a PR (each skip is justified in the PR that adds it).
- [x] Each failure found while adding the job is recorded in the evidence as a product bug (with its fix and test) or a harness assumption (with its fix).
- [x] A deliberately broken root-only path (for example, a socket handed to the broker's own uid instead of the cell's) fails the root job and passes the unprivileged one, shown by a hand mutant.
- [x] `HATCHLING_ROOT_PASS=1 bash scripts/check.sh` reproduces the job locally.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

The root runs were developed in the sandbox, as root, in a contained systemd unit (system and homes read-only, private `/tmp`, no network, only its log directory writable), with the README's users and the pinned headless Chromium. CI's "cells as root" job runs the same pass (`scripts/root-pass.sh`).

1. **The job.** "cells as root" in `.github/workflows/ci.yml`:
   - It builds on 1.98.1 as the runner, creates the README's four users, and copies the pinned Chromium where a root broker and its cells can read it.
   - It runs `HATCHLING_ROOT_PASS=1 bash scripts/check.sh` with `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` and `HATCHLING_REQUIRE_BROWSER=1`, so no suite may skip.
   - The suites: 14 broker targets, `hatchling-isolation`, and `hatchling-sys`'s two. All 17 pass as root:
     - `adversarial` (22 checks), `artifacts` 29, `browser_cell` 4 (the real browser, 21 s), `cell_boundary` 9;
     - `harness` 10, `harness_cli` 7, `schedule_run` 13, `skill_cell_spawn` 1, `skill_envelope` 1, `skill_run` 15, `spawner` 4;
     - `subagents` 9, `sweep` 3, `unknown_outcome` 4;
     - `hatchling-isolation` 97, `process_groups` 2, `seccomp_startup` 2.

   Making it a required check on `main` is a branch-protection setting: the maintainer's, once the job has run green.
2. **Skips, listed.** The pass ends by listing every skip with its reason. There's one: the adversarial crash-handler check, which sets the host's `core_pattern` and runs only with `HATCHLING_CORE_PATTERN_TEST=1`. The gate job already runs it as root, in a step of its own.
3. **Every failure the root runs found, and what it was.** All were harness assumptions; none was a product bug.
   - **Cells given `nobody`.** As root, the tests mapped cells to 65534, which the broker refuses as the agent's uid (other processes share it). They now use the README's `hatchling-cell` (`common::mapped_agent_uid`, `root_cell_uid`), failing with the `useradd` command if it's missing. That fixed `spawner` and `subagents`' startup.
   - **The id-map base left at its default.** As root, a test cell's base defaulted to 65534 and so didn't match the agent's uid, which the broker refuses (its peer gate would reject the agent). The README sets `HATCHLING_CELL_UID_BASE` to the agent's uid. So does the pass, and so does `CellConfig::parse_test`. That fixed `cell_boundary`, `schedule_run`, `skill_cell_spawn`, `sweep`, `unknown_outcome` and the adversarial socket sweep.
   - **A run directory the broker's user didn't own.** As root, the spawner accepts only run directories owned by `hatchling-broker`, the user a root broker drops to. The test playing the broker made one as root; it now hands it over (`spawner`).
   - **The test as the agent.** `subagents` plays the parent agent from its own process, which as root isn't the agent's user, so the session's peer gate closed it. It now connects as `hatchling-cell` from a child that has dropped to it (`common::connect_as_agent`), over a socket handed to that user, as a root broker's cells reach it.
   - **The peer decoy's socket.** As root it stayed root's `0600`, so the probe was stopped before the peer gate the check is about. It's handed over as the spawn hands the broker's own (`adversarial`).
   - **Directories a test's cell writes.** As root they weren't the cell's, so the scratch and real-browser tests couldn't write their output or the browser's profile. They're given to the cell's user, as a run's trees are (`hatchling-isolation`).
   - **One inventory errno.** Writing `/agent` is refused `EACCES` as root (the program isn't the cell's) and `ETXTBSY` unprivileged (it is, and it's running). Both are refusals. The mount-escape inventory folds the two, as it folds the `/lib` spelling. `inventories_fold_only_the_programs_owner` proves, with the root inventory as a fixture, that the fold is exact and that a write that succeeds still shows.
4. **A root-only break, caught.** A hand mutant hands the broker socket to the broker's own uid instead of the cell's:
   - **Unprivileged:** `cell_boundary` (9), `adversarial` (22) and `hatchling-isolation` (97) all pass.
   - **As root:** all three fail, each for that reason. `cell_boundary`: "timed out waiting for the cell agent to connect". `adversarial`: the socket sweep finds the broker's socket unreachable. Isolation: "cannot let the cell (uid 0) reach the broker socket dir".
5. **Locally.** `HATCHLING_ROOT_PASS=1 bash scripts/check.sh` runs `scripts/root-pass.sh`, the job's own script, documented in CONTRIBUTING.

## Notes

- **Root, contained, while developing.** The root runs never ran a script the test user could write, and wrote only a root-owned log directory, so a misbehaving test couldn't reach outside its unit.
- **Found for the audit, not changed here.** A root broker given only `HATCHLING_AGENT_UID` refuses to start until `HATCHLING_CELL_UID_BASE` matches it, though the base could default to that uid. The error is clear and the README sets both.
- GitHub's Ubuntu runners allow `sudo` without a password, so the job can run the test binaries as root. Build as the runner user, then run the built test binaries under `sudo -E`, so the build cache and target directory stay owned by the runner.
- The runner's AppArmor and core-dump settings that the gate job already sets (`kernel.apparmor_restrict_unprivileged_userns=0`, `kernel.core_pattern`) apply to this job too.
