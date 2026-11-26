# ldd with a clean environment, never as root on what others may write (#688)

Closes [#688](https://github.com/undergroundrap/hatchling/issues/688), hardening in `crates/hatchling-isolation` from an outside review, confirmed in the code. The cell's library view decides which host files every cell gets.

## The fixes

1. **`ldd` runs with a clean environment.**
   - **Before:** `ldd_output` inherited everything the broker had, so an operator's `LD_LIBRARY_PATH`, set for something else, silently changed what every cell held. That environment was handed to another user's process when root ran `ldd` as a binary's owner, and a non-C locale could translate the messages it parses ("not found" among them).
   - **Now:** `clean_env` clears the environment and sets only `PATH=/usr/bin:/bin` and `LC_ALL=C`.
2. **Who runs `ldd` is whoever could change the binary, not only its owner.**
   - **Before:** as root, `ldd_identity` looked only at owners on the way to the binary. A root-owned binary in a group- or world-writable directory could be swapped by someone else, and root would then run `ldd` on it.
   - **Now:** `changer` reads each entry, the binary first. The first owner other than root runs it, as before. The first entry others may write runs it as `nobody` (65534): a group- or world-writable file, or such a directory without the sticky bit.
   - A sticky directory, as `/tmp`, lets nobody replace root's file. A link's mode means nothing (`/bin` is one on a merged-`/usr` system), so only its owner counts.
3. **`supervise()` takes the child.** `IsolationBackend::supervise` took `&mut std::process::Child`, leaving the caller a handle that could still wait on the helper, or signal it by a pid supervision now owns. It now takes the child by value. Every caller already dropped its handle straight after, and now passes the child instead. This is a breaking change for `hatchling-isolation`, said so in the changelog.

## Tests

In `hatchling-isolation`:
- `who_could_change_a_binary_is_read_from_each_entry` runs `changer` on each case without needing root, which these tests don't run as:
  - root's alone;
  - the binary writable by its group, or by anyone;
  - a group- or world-writable directory above it;
  - a sticky directory;
  - a sticky file;
  - a link, root's and another's;
  - which entry speaks first.
- `ldd_runs_with_a_clean_environment` runs `/usr/bin/env` through `clean_env`: exactly `LC_ALL=C` and `PATH=/usr/bin:/bin` reach it, nothing of the test's own environment.
- The existing `ldd_runs_as_whoever_could_change_the_binary` (its root half runs only as root) and the supervise tests, now passing the child, pass.

## The gate

RESULTS
