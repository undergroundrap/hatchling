# Evidence: a skill's files in the agent's computer (WO-046 slice 3b)

What was added, and how it was tested, for WO-046's Room mount.

| | |
|---|---|
| Base | `main` at `8ea545fe` (slice 3a) |
| Date | 2026-11-23 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, umask 022), toolchains 1.98.1 and 1.85.0, and as root there for the Room's own proofs |

This record was added after the change, in its own commit. It changes no code.

## What it is

- **Where the files are:** each skill the owner enabled is copied, read-only and
  `noexec`, to `/workspace/skills/<name>/` in the agent's computer, so a
  playbook's helpers run there through an interpreter
  (`python3 /workspace/skills/<name>/outline.py`). The path is the
  maintainer's choice: `/workspace/skills` needs no new Room image.
- **On the room disk:** a `skills` folder of root's at the encrypted volume's
  root, beside `inbox`, attached read-only at the workspace's root-owned
  `skills` attachment. The Room's mount check (`verify_view`) covers it. A
  skills folder others could write refuses the disk, as the inbox does.
- **Written by the spawner, as root,** through the volume's pinned root:
  never by a path from it, through a link or across a mount.
  - A skill is written whole as `.new-<name>`, every file `0444` and synced,
    then renamed into place.
  - What a put or a removal that didn't finish left is taken out first.
  - Each skill keeps its digest in `.digest`.
  - Names and paths keep to ASCII letters, digits, `.`, `_` and `-`, no step
    starting with a dot, at most eight steps.
  - At most 64 skills of 64 files and 1 MiB each.
- **Brought in line before each chat run with a Room:**
  - The broker takes out a skill no longer enabled, or enabled at another
    digest, and puts in one the Room lacks.
  - What it puts is its own verified copy, each file read and checked
    against its signed hash as it is read.
  - A skill's program isn't copied: it runs in a cell of its own.
  - A skill the Room can't name or hold is left out, and logged.
  - A refresh that fails never stops the run.
- **Moved aside, never removed:** a `skills` the agent had made in its own
  workspace before is renamed `skills.moved-<n>` the first time the disk
  opens, so the attachment can be root's.
- **The spawner protocol:** one request, `RoomSkills { op }`, to list, put or
  remove. A put's files follow in frames, held to the bounds before they are
  read. It goes through a live Room session when one holds the disk, as the
  inbox's requests do.
- **The prompt:** a skill with files says where they are in the agent's
  computer, and the agent's note about its computer names
  `/workspace/skills`.

## Tests

| Check | 1.98.1 | 1.85.0 |
|---|---|---|
| `cargo fmt --all --check` | pass | |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pass | pass |
| `hatchling-dev audit` | pass | |
| `hatchling-sys --lib` (`room_skills`) | 9 passed | |
| `hatchling-broker --lib` | 1806 passed | |
| `scripts/room-mount-proof.sh`, `full` and `writable`, as root | pass | |
| `hatchling-broker --test room_disk`, as root, with a cgroup | 11 passed | |

The root runs use binaries built from the tree under test after the
hand-breaks were restored, since a hand-break run leaves its last broken
build at the same paths.

No protocol or frontend change: the skills reach the Room through the
spawner's own protocol, which isn't the session's.

Among the new tests:
- **On the room disk (`room_skills`):**
  - a skill is put whole, every file `0444`, and listed with its digest;
  - put again, it replaces the old one;
  - taken out, it's gone, and what an interrupted put or removal left is
    tidied away;
  - names and paths keep to their grammar;
  - a skill the Room doesn't take is refused with nothing written;
  - the bounds are pinned, and a skill exactly at them is put: 64 files,
    a 1 MiB file, an eight-step path;
  - at most 64 skills;
  - a tree left deeper than a skill can be is refused, and one at the
    bound is taken out;
  - a skills directory others could write, or one reached by a link, is
    refused.
- **The refresh (`skill_room`):**
  - out first, then in, and a skill already held as wanted is left alone;
  - it stops at the first failure and says what it was;
  - what's wanted is each enabled skill's verified files, without its
    program;
  - a bad name, and a file changed since it was loaded, are left out;
  - a skill exactly at the Room's bound is given it;
  - a chat run brings the Room in line only when the run has one, there
    is a Room to put skills in, and skills to put.
- **The spawner protocol:**
  - each request asked and answered as the spawner answers;
  - a put held to its bounds and its frames;
  - each result answered done or refused.
- **As root:**
  - in the Room, a skill's file reads, and a write to `/workspace/skills`
    gives `EROFS`;
  - a skills folder of mode `0775` refuses the volume;
  - the spawner lays the disk out with the skills folder `0755` and the
    attachment `0700`, both root's;
  - it puts, lists and takes out a skill, and refuses `../out`;
  - opened again, a root-owned attachment is kept;
  - a `skills` the agent made is moved aside with what it holds;
  - a skills folder others could write refuses the disk.

**Hand-breaks**, each applied to the tree, its tests built and run, and
the tree restored. Both root suites pass unbroken.

| Break | Caught by |
|---|---|
| A skill's files written `0644` | `a_skill_is_put_whole_read_only_and_listed_with_its_digest` |
| Leftovers of a put or removal not tidied | `a_skill_is_taken_out_and_what_an_interrupted_one_left_is_tidied`, `whats_left_deeper_than_a_skill_can_be_…` |
| A name starting with a dot allowed | `names_and_paths_keep_to_their_grammar`, `what_the_room_doesnt_take_is_refused_…` |
| A skill no longer wanted never taken out | both `a_refresh_…` tests |
| A skill's program copied to the Room | `what_the_room_is_given_is_each_enabled_skills_verified_files_but_its_program` |
| A file not checked against its signed hash | the same |
| The skills attached writable | the full mount proof: the Room's mount check refuses `/workspace/skills` |
| The skills folder's ownership not checked | the full mount proof: a `0775` skills folder is let through |

A ninth break, taking `/workspace/skills` out of the Room's mount check,
failed nothing. The check only refuses a mount that's wrong, and this one
isn't: the mount itself is proven read-only by the `EROFS` write and by
the break that attaches it writable. The check's other lines have the same
gap, except the line for `/`. It stays, as a second check after the mount.

**Mutation**, on the lines changed against `8ea545fe`, in the sandbox's
jail (not root):

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| `hatchling-sys` (library), first run | 90 | 58 | 26 | 0 | 6 |
| `hatchling-sys` (library), with the new tests | 87 | 62 | 19 | 0 | 6 |
| `hatchling-broker` (library), first run | 79 | 35 | 27 | 0 | 17 |
| `hatchling-broker` (library), with the new tests | 79 | 62 | 0 | 0 | 17 |
| `hatchling-core` (library) | 2 | 2 | 0 | 0 | 0 |
| `hatchling-agent` (library) | 2 | 2 | 0 | 0 | 0 |

The first runs' misses in the library code are covered by tests now:
- the bounds at their edges;
- the spawner's requests and replies;
- the chat run's decision, taken out of the run into `before_chat_run`.

The 19 `hatchling-sys` misses left are code only root runs: the Room's
entry points, the attachment, the move aside, the mount check, and the
volume's prepare, list, put and remove. Each was applied by hand, exactly
as cargo-mutants wrote it, and run as root:

| Mutant | Caught by |
|---|---|
| `RoomMounts::enter` returns `Ok(())` | the mount proof: a planted inbox target isn't refused |
| `enter_verified` returns `Ok(())` | the mount proof: the Room's root isn't the verified image |
| `enter_verified_volume` returns `Ok(())` | the mount proof: a substituted source isn't refused |
| `enter_handed_off_volume` returns `Ok(())` | the mount proof: mismatched launch inputs aren't refused |
| `verify_view` returns `Ok(())` | `writable_root_fails_room_mount_verification`, outside the jail, where `/` is writable |
| `skills_anchor` returns `Ok(())` | `the_spawner_puts_the_owners_skills_on_the_room_disk`: no attachment |
| its two `&&` made `\|\|` | the same: the agent's folder kept, not moved aside |
| its `==` made `!=`, three places, and `&` made `\|` or `^` | the same: a root-owned attachment moved aside on opening again |
| `move_aside` returns `Ok(())` | the same: nothing moved aside |
| `room_skills::prepare` returns `Ok(())` | the same: no skills folder |
| `room_skills::held` returns `Ok(vec![])` | the same: the skill put isn't listed |
| `room_skills::put` returns `Ok(())` | the same: the skill's file isn't there |
| `room_skills::remove` returns `Ok(true)` or `Ok(false)` | the same: the skill is still listed |

The unviable mutants replace a result with a `Default` its type doesn't
have.
