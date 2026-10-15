# Pull Request Template

## What changed

<!-- One paragraph. What does this PR do and why? -->

Work order: <!-- WO-NNN, or "none" -->

## Evidence

<!-- PRs without evidence are closed. Fill in every section that applies.
     A docs-only PR fills in the Docs section only. -->

### Backend (Rust)

- [ ] `bash scripts/check.sh` green on the pinned toolchain — paste summary lines:
- [ ] `bash scripts/check.sh` green on MSRV (1.85.0) — paste summary lines:
- [ ] Mutation testing (`scripts/mutant-run.sh`): <!-- mutants tested / caught / missed / timed out / unviable -->
  - Kill floor 1.0 met: <!-- yes / no — if no, explain -->
  - Equivalent mutants proven in `.cargo/mutants.toml`: <!-- list or "none" -->
- [ ] New dependencies: <!-- list or "none" — each needs a docs/dependency-review.md entry -->

### Frontend (TypeScript)

- [ ] `cd frontend && npm ci && npm run check` green
- [ ] Screenshots attached: <!-- required for any UI change -->
- [ ] Live adapter coverage still 100%: <!-- yes / n/a -->

### Docs

- [ ] Behavior changes reflected in docs: <!-- yes / n/a — which docs? -->
- [ ] Doc examples/tables still verified: <!-- check.sh covers this -->

## Security

- [ ] This PR touches the trust boundary: <!-- yes / no -->
  <!-- If yes: auth, isolation, secrets, egress, or permissions — describe the impact -->
- Attack this stops: <!-- attacker, exploit path, prevented outcome; n/a if not a defense -->
- Hand mutant result: <!-- patch, exact command, test name and expected failure; n/a with reason -->
- [ ] Real-cell proof ran with `HATCHLING_REQUIRE_CELL=1`: <!-- command and adversarial summary; n/a with reason; cgroups also require HATCHLING_REQUIRE_CGROUP=1 -->

## Checklist

- [ ] One logical change per commit, as a Conventional Commit with a scope (`feat(vault):`, `fix(broker):`, `docs(api):`)
- [ ] A line under **Unreleased** in `CHANGELOG.md`, or no user-visible change
- [ ] No secrets, keys, or credentials committed
- [ ] I've signed the [CLA](https://github.com/undergroundrap/hatchling/blob/main/CLA.md) (on your first PR, the CLA bot asks you to)
- [ ] I've read and understood every change in this PR and can answer questions about it, however it was written
- [ ] No `unwrap()`/`expect()` in production paths
