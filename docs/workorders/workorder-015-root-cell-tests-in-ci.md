# WO-015: Run the cell test suites as root in CI

**Status:** QUEUED
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

- [ ] The root CI job runs the cell suites with `HATCHLING_REQUIRE_CELL=1` and passes; it's a required check on `main`.
- [ ] The job's log lists every test skipped as root, each with its reason, and the list is short enough to review in a PR (each skip is justified in the PR that adds it).
- [ ] Each failure found while adding the job is recorded in the evidence as a product bug (with its fix and test) or a harness assumption (with its fix).
- [ ] A deliberately broken root-only path (for example, a socket handed to the broker's own uid instead of the cell's) fails the root job and passes the unprivileged one, shown by a hand mutant.
- [ ] `HATCHLING_ROOT_PASS=1 bash scripts/check.sh` reproduces the job locally.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- GitHub's Ubuntu runners allow `sudo` without a password, so the job can run the test binaries as root. Build as the runner user, then run the built test binaries under `sudo -E`, so the build cache and target directory stay owned by the runner.
- The runner's AppArmor and core-dump settings that the gate job already sets (`kernel.apparmor_restrict_unprivileged_userns=0`, `kernel.core_pattern`) apply to this job too.
