# Testing patterns

Tests must distinguish the working defense from a broken one. Use these
patterns with [CONTRIBUTING.md](../CONTRIBUTING.md#security-fixes); they
supplement its mutation floor and gates.

## Mutation traps

- **Timer boundaries.** `Duration::ZERO` alone cannot distinguish every
  mutation of a comparison such as `<`, `<=` or `==`. Exercise equality
  and nonzero values on both sides, and prove that a wait actually waits.
  Account for kernel timer granularity when testing socket timeouts:
  [#53](https://github.com/undergroundrap/hatchling/pull/53) diagnosed an
  early timeout by one tick instead of repeatedly rerunning the test.
- **Forked assertions.** A child that unwinds into libtest may leave a
  successful child exit despite a failed assertion. Catch the panic in
  the child, map it to an explicit nonzero exit, and have the parent
  assert the exit status within a deadline. The `in_child` helper in
  [spawn.rs](../crates/hatchling-isolation/src/linux/spawn.rs) does this.
- **An accidental exec.** A mutant can turn an error path into a successful
  `exec`, replacing the test process and reporting success without its
  assertions. Run anything that could exec in a forked child. Check the
  child's result and that the expected program or failure path ran.
- **Default-valued fixtures.** A mutant returning `Default::default()`
  survives if the fixture already produces the default. Use explicit
  nondefault settings and test missing, valid and invalid cases. Exercise
  environment constructors in fresh subprocesses with a controlled
  environment; do not change shared process environment during parallel tests.
- **Disjoint flags.** `A | B` and `A ^ B` are equivalent when no bits overlap.
  Use a named combined mask when appropriate. If an equivalent mutant
  remains, prove that equivalence in `.cargo/mutants.toml`; do not exclude
  it merely because it survived.
- **Scripted peers.** A broken send, connect or destructor can leave a
  peer waiting forever and hang its join. Bound waits, close the test's
  socket before joining, and make failure cleanup release the peer. Keep
  the normal attack unchanged: a rescue connection must not replace the
  cancellation-before-connect case it protects. [#54](https://github.com/undergroundrap/hatchling/pull/54)
  exercises this with a deleted `CellRun::drop`.

## Cell evidence and hand mutants

Set `HATCHLING_REQUIRE_CELL=1` for gates, adversarial tests and mutation.
Otherwise a mutant that breaks the environment probe can make the tests
skip. For cgroup defenses, also set `HATCHLING_REQUIRE_CGROUP=1` and give
the tests a usable `HATCHLING_TEST_CGROUP` delegation. Check the adversarial
summary: that binary uses its own runner, not libtest's `test result:` line.

For a hand mutant, copy only source and tests into disposable scratch
space on a disk filesystem (see [WSL2 testing](testing-on-wsl2.md#7-full-mutation-run)).
Stage new files first so `git ls-files` includes them; this copies
the working files, including unstaged edits to tracked files. For example,
hand-break #52's init protection and run its supervisor-death check:

```bash
M=$(mktemp -d "$HOME/hatchling-hand-XXXXXX")
git ls-files -z | xargs -0 cp --parents -t "$M"
(
  cd "$M"
  # Edit this copy to bypass the init protection.
  export CARGO_TARGET_DIR="$M/target" HATCHLING_REQUIRE_CELL=1
  cargo build --workspace --locked
  cargo test -p hatchling-broker --locked --test adversarial \
    a_cell_that_clears_its_death_signal_still_dies_with_its_supervisor -- --exact
)
```

Carry over required test configuration, including the cgroup variables
where applicable. Give each copy its own Cargo target; never copy a
workspace's build output or share targets across scratch paths. Preserve
the failing assertion and patch, then remove only that scratch copy.
The test must pass on the fixed source and fail by assertion on the broken
source, not be killed by a runner timeout. A bounded deadline assertion
for unexpected survival is valid. Keep attacks bounded and reap their children.

## Lints and failures

Run plain clippy first on both toolchains so warnings in dependent crates
remain visible:

```bash
for tc in 1.98.1 1.85.0; do
  RUSTUP_TOOLCHAIN=$tc cargo clippy --workspace --all-targets --all-features --locked
done
```

This diagnostic pass does not replace `scripts/check.sh`, which uses
`-D warnings`. Stop and fix warning/error sites before the full gate.
For a failure or flake, retain the first failure and diagnose it; test the
repair and report both the failure and the successful proof. A false
failure can make a mutant look caught, so classify outcomes before claiming
the kill floor. Report missed, timed out and unviable counts separately.
