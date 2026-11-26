# An inherited fd adopted once, and no drop to root's group (#704, in part)

Part of [#704](https://github.com/undergroundrap/hatchling/issues/704), from an outside review of `crates/hatchling-sys`. Its first item, building everything `execve` needs before `fork`, is its own change.

## The fixes

1. **An inherited descriptor is adopted once.**
   - **Before:** `inherited_unix_stream` (`net.rs`) and `inherited_device` (`fds.rs`) each take ownership of a descriptor a process inherited, and said to call them once for one. Nothing enforced that. A second call gave the same descriptor a second owner, and whichever dropped second closed a number that may name something else by then.
   - **Now:** a descriptor already close-on-exec is refused.
     - One that crossed `exec` can't be close-on-exec: the flag would have closed it. So one that is wasn't inherited.
     - Adopting marks it close-on-exec, so a second adoption is refused too.
   - No count or set is kept, so a descriptor number reused later is judged as itself.
2. **`drop_to` refuses gid 0,** as it refuses uid 0, before anything changes. A misconfigured gid 0 would have left the dropped process in root's group, with what root's group may read and write in reach.

**What callers see:** every real adoption is of a descriptor that crossed `exec`, which is never close-on-exec, so none changes. Two sets of tests handed over descriptors made in-process, which are close-on-exec. They now hand them over as a launcher does, clearing the flag first: the agent's ends of a `UnixStream::pair` (with `handed`), and the keeper's test security key.

## Tests

In `hatchling-sys`:
- `an_inherited_device_is_adopted_and_nothing_else`: a second adoption while it's owned is refused ("adopted already"), and so is a device opened here, left as it was.
- `only_an_inherited_unix_stream_is_adopted_and_it_closes_on_exec`: the same for a connection, and for an end of a pair never handed across `exec`.
- `a_drop_leaves_the_user_alone_with_nothing_more`: `drop_to(4242, 0)` is refused as "drop to root's group".

The agent's 210 tests, and the keeper's, pass with their ends handed over as a launcher would.

## The gate

On the sandbox, with `HATCHLING_REQUIRE_CELL=1`:

- `cargo fmt --check`, and clippy with `-D warnings` on 1.98.1 and 1.85.0: clean.
- **The whole workspace's tests: 4595 passed, 0 failed.**
  - The gate's first run found one more caller to hand over as a launcher does: the keeper's `security_keys_cross_to_the_keeper_by_their_descriptors` adopted a `/dev/null` it had opened itself.
  - The keeper's real launcher already keeps each key across `exec`, and the test now does the same.
- `hatchling-dev audit`: all checks passed.
- **Hand-breaks**, each caught by `hatchling-sys`'s tests:
  - a device adopted though close-on-exec;
  - a connection adopted though close-on-exec;
  - a drop to gid 0 let through.
- **Mutation** on the changed lines of `fds.rs`, `net.rs` and `process.rs`: 14 mutants, 12 caught, 2 unviable, none missed.
