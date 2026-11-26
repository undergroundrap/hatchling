# A forked child allocates nothing before its exec (#704)

Closes [#704](https://github.com/undergroundrap/hatchling/issues/704), with [#731](https://github.com/undergroundrap/hatchling/pull/731) (its other two items), from an outside review of `crates/hatchling-sys`.

## The threat

**What `fork()` promises.** Its contract said only async-signal-safe calls are sound in the child until `exec`, and that "the spawner forks before any other thread exists". But:
- **`exec()` built its arguments in the child:** its `CString`s, a `format!` for each `k=v`, and the pointer arrays.
- **The callers allocated after the fork too:**
  - the browser's launch collected its argv and env;
  - the Room's supervisor built its seccomp filter in the child;
  - a cell's program built its environment and filter there.
- **The broker does fork with other threads,** as `exec()`'s own `ETXTBSY` retry says.

**Reach today was low.** Releases use the system allocator, and glibc holds its malloc locks across a fork and resets them in the child, so the allocator deadlock didn't happen in the shipped build. But that rested on a C library detail, not on the contract the code states.

## The fix

- **`PreparedExec`** (`process.rs`): the path, argv and env as C strings, and their NULL-terminated pointer arrays, made before the fork.
  - `PreparedExec::exec(last)` then runs `last` and `execve`, with the `ETXTBSY` retry, allocating nothing.
  - `exec()` stays as `PreparedExec::new` and `exec` together, documented as for a process with no other thread.
- **`PreparedFilter`** (`seccomp.rs`): `seccomp::prepare(profile)` compiles a profile before the fork, and `install()` is the `prctl` alone. `seccomp::install` stays as the two together.
- **`place_across_exec`** keeps its copies in a fixed array of `MAX_PLACED` (8), not a growing one. More is refused, before anything is copied.
- **Each child prepares before its fork:**
  - **the browser's launch** prepares Chromium's exec;
  - **the Room's supervisor launch** prepares its exec and its filter;
  - **a cell's init** prepares the program's exec, its channels at the numbers it will hold, and its filter, before it forks the program.
- **`fork()`'s doc** now says what a child may do, and which children may do more: one of a process with no other thread, such as a cell's init.

**What's left:** a failing child still writes its reason, which allocates. That's an exit path that fails closed, since the parent's deadline kills a child that hangs.

## Tests

`crates/hatchling-sys/tests/exec_no_alloc.rs`, a binary whose global allocator ends a child that allocates while it's told not to (exit 77):
- `a_prepared_exec_allocates_nothing_after_the_fork`: 50 forks and execs of `/bin/true`, while another thread allocates in a loop. Each child execs without allocating, under a deadline.
- `a_prepared_filter_installs_allocating_nothing`: the agent's filter, prepared, installs in the child without allocating.
- `building_the_exec_in_the_child_is_caught`: the check works. `exec()`, building in the child, is ended for it.

In `fds::tests`, one descriptor past `MAX_PLACED` is refused. The isolation (98), browser (31) and `sys` (333) unit tests pass with each child preparing before its fork.

## The gate

RESULTS
