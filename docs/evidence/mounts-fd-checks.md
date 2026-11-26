# A cell's mounts judged by what's mounted (#680)

Closes [#680](https://github.com/undergroundrap/hatchling/issues/680), hardening in `crates/hatchling-sys/src/mounts.rs` from an outside review, confirmed in the code. Three places fell short of the module's own rule: resolve once, then operate on the fd.

## The threats, and the fixes

1. **A device bind was checked by path, not on the fd it mounts.**
   - **The gap:** `MountPlan::new()` checked a device source with `symlink_metadata`. `stage()` then opened it `O_PATH` and mounted that fd writable, with no `NODEV`. Whoever could replace the path in between got a writable bind with no `NODEV` of what they put there. Today the sources are the host's `/dev/null`, `/dev/zero` and `/dev/urandom`, which only root can replace, so the risk was low.
   - **Now:** `stage()` checks the opened fd with `fstat` and refuses anything that isn't a character device, before it mounts. The plan-time check stays, so a bad plan fails before any mount.
2. **Locked flags were read by path.**
   - **The gap:** `remount_flags_for` called `statvfs(path)` right after the source was opened by fd. The two could name different mounts, though traced both ways it failed closed.
   - **Now:** it takes the opened fd and reads `fstatvfs`, which the kernel allows on an `O_PATH` fd. The new root's own flags are read the same way, from the root opened with no link followed.
3. **The plan's own places weren't refused at plan time.**
   - **The gap:** a bind at the pivot's `old`, at `proc`, or under either, or under the scratch tmpfs, failed late (`EEXIST`) or was hidden.
   - **Now:** `new()` refuses `old`, `proc` and anything under them, and `with_scratch()` refuses anything at or under `scratch`.
   - Without a scratch tmpfs, a bind at `scratch` is still a plan of its own: a harness cell binds its scratch directory there. Names that only start the same (`oldest`, `processes`, `scratchy`) are fine.

## Tests

In `hatchling-sys`'s `mounts::tests`:
- `a_device_bind_swapped_after_the_plan_is_refused_at_the_mount`: in a child with user and mount namespaces of its own, the plan is made while `/dev/null` is a device. A regular file is then bound over `/dev/null`, and `apply()` is refused with "no longer a character device". It ran here under `HATCHLING_REQUIRE_CELL=1`, so it couldn't skip.
- `remount_flags_come_from_what_was_opened`: a directory is opened, then moved, with a link to `/proc` left at its path. Its flags are its own mount's, not `/proc`'s `NOEXEC`.
- `an_opened_source_is_a_character_device_or_not`: `/dev/null` is one; `/etc/passwd` and `/dev` aren't.
- `the_plans_own_places_are_refused_as_targets`: each reserved place, and one nested under each, is refused. Look-alikes are fine. `scratch/...` is refused only under a scratch tmpfs.
- `remount_flags_retain_source_mount_flags` now reads from the opened fd.

## The gate

RESULTS
