# The vault's key and keyring files, judged by what was opened (#686)

Closes [#686](https://github.com/undergroundrap/hatchling/issues/686), a hardening from an outside review, confirmed in the code. The envelope and its cryptography were found sound.

## The threat

**Before:**
- The key file was opened with `File::open`, which follows a symlink and can wait on a FIFO.
- Then its path was checked, not what was opened.
- The unlock path (`FileKey::load`, for the key that unlocks the keyring) didn't check the directory.

The keyring did the same: it checked the path, then opened it again to read.

**The attack:** with a directory others can write in, another user renames the real key aside, plants a link to their own file, and puts the real key back once the broker has opened the link. The path then passes every check, while what's read is theirs.

**The effect:** a substituted unlock key, which fails the unlock. That's a denial of service, not a disclosure. A FIFO at the path blocks the start.

## The fix

- **`open_nofollow`** (`vault.rs`) opens with `O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`.
  - A link at the path is refused at the open, and said as no sound key file.
  - A FIFO is opened without waiting, then refused as not a regular file.
  - `O_NONBLOCK` stays set: Linux ignores it on a regular file, the only kind read on, so clearing it would change nothing a test could see.
- **Judged by the descriptor.** The key file (`read_key`) and the keyring (`Keyring::load_as`) are checked by `fstat` on what was opened: a regular file, its mode, its owner. So a file swapped in afterwards is never what's checked. The key's lost-creation-race path reads the same way.
- **The directory on every load.** `check_owner_only_dir`, split from `ensure_owner_only_dir`, checks without making anything. It's used by the unlock path and the keyring's load, since a load must never create. A key or keyring whose directory isn't the expected owner's is now refused at the directory, before the file.
- **The keyring's change check** (`digest_file`) doesn't follow a link either.

**No default install changes:** the key file, the KEK and the keyring sit in `<vault>.key/`, made owner-only `0700` when the first key is created.

## Tests

In `hatchling-vault`:
- `file_key_symlink_is_rejected`: a link to a sound key is refused at the open (`ELOOP`), by load and create alike.
- `file_key_fifo_is_refused_without_blocking`: load and create return within the bound, refused.
- `file_key_is_judged_by_what_was_opened`: the sound key opened, then a loose file renamed over the path, still reads the sound key; the loose file opened, then a sound one renamed over it, is refused.
- `loading_a_key_checks_its_directory`: `0770`, `0777`, `1777` and `0750` each refuse the unlock path.
- `a_keyring_is_opened_as_itself_in_its_own_directory`: a link is refused, a change check doesn't follow one, and a loose directory refuses the load.
- Three existing tests now expect a link refused at the open (`file_key_symlink_loop_fails_closed`) and a wrong owner refused at the directory (`loading_a_key_never_creates_one`, `a_keyring_is_never_overwritten_and_loads_only_when_sound`).

## The gate

On the sandbox, at the tree under test:

- `cargo fmt --check`, and clippy with `-D warnings` on 1.98.1 and 1.85.0: clean.
- The whole workspace's tests: 4576 passed, 0 failed. `hatchling-dev audit`: all checks passed.
- **Hand-breaks**, each caught by the vault's tests:
  - the path's metadata again, not the descriptor's;
  - the unlock path skipping its directory;
  - a link followed (no `O_NOFOLLOW`);
  - a FIFO waited on (no `O_NONBLOCK`);
  - the keyring skipping its directory.
- **Mutation** on the changed lines of `vault.rs`, `seal.rs` and `keyring.rs`: 19 mutants, 12 caught, 7 unviable, none missed.
  - The first pass missed three: `|` to `^` between the open's flags.
  - Those bits are disjoint, so the two are the same flags.
  - The flags are now a `union`, as `hatchling-sys` writes them.
