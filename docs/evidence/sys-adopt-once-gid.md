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

**What callers see:** every real adoption is of a descriptor that crossed `exec`, which is never close-on-exec, so none changes. The agent's tests handed over an end of an in-process `UnixStream::pair`, which is close-on-exec. They now hand it over as a launcher does: `handed` clears the flag first.

## Tests

In `hatchling-sys`:
- `an_inherited_device_is_adopted_and_nothing_else`: a second adoption while it's owned is refused ("adopted already"), and so is a device opened here, left as it was.
- `only_an_inherited_unix_stream_is_adopted_and_it_closes_on_exec`: the same for a connection, and for an end of a pair never handed across `exec`.
- `a_drop_leaves_the_user_alone_with_nothing_more`: `drop_to(4242, 0)` is refused as "drop to root's group".

The agent's 210 tests pass with their ends handed over as a launcher would.

## The gate

RESULTS
