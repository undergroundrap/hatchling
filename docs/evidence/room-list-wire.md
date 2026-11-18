# Evidence: a folder in the Room listed (WO-043, slice 1)

What was tested, how, and what came back, for the change that lets the
spawner ask the Room's supervisor what a folder in the workspace holds,
a page at a time, for a folder to be handed to the owner. Walking a
folder and handing it over is a later slice: nothing asks for a listing
yet.

| | |
|---|---|
| Base commit | `19dc4164` (`fix(broker): spend a hand-over before its file is checked (#504) (#506)`) |
| Tested tree | `2ab9c1f2f6c5267f8ff95ad76dab173054dffac1` (the gate); the breaks, the mutation and the installed Room on `68f5f088`, whose code is the same file for file (the two differ only in `CHANGELOG.md` and `docs/arch/trust-boundary.md`) |
| Date | 2026-11-17 to 18 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox; the Room's tests as root outside it |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| The supervisor, built static | sha256 `4b78e3d23a79eb117e01ab671531b1fe45f9f7e3d34be82b9a7780dc61c62e61` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What changed

- **The Room's wire** (`hatchling-core`): `Ask::List` (a folder, and the
  name to go on after), `Say::Listed` (the folder's inode, change time
  and count of names, and at most 64 of its names, each with what it is:
  a file and its length, a folder, a link, another mount, or something
  else) and `Say::NoList` (no such folder, denied, not a folder, a link,
  outside the workspace, too many names, changed, failed). `check_list`
  says what may be asked for: the workspace itself, or a path under it
  with no empty, `.` or `..` step. `Listed::check` says what a page may
  be, and `check_entries` what its names may be: names, in the order of
  their bytes, after the one asked for, an inode only for a file or a
  folder and a length only for a file.
- **The supervisor** (`hatchling-room`, `list.rs`): opens the folder as
  a file to take is opened, with the opener the two now share
  (`take::beneath`, a file or a folder, read only): beneath the
  workspace through no link and onto no other file system. It reads all
  the folder's names (at most 1024), sorts them, looks at those after the
  name asked for (at most 64) with `statx`, following no link and
  mounting nothing, and reads the names again: a name read twice, or
  names, inode, change time or mount not the same after as before, is a
  folder that changed. A name on another mount than the folder's is a
  mount, whatever it is. What the kernel says is taken only when it
  filled in all that was asked. Its reads sit behind a small trait, so
  races are tested without one.
- **The spawner's session** (`room_session.rs`): a page is asked in its
  turn, as a piece of a file is, and held to the name it was asked
  after. A page that can't be the one asked for ends the session;
  whoever waits is told why once the Room is down. Nothing is read for a
  caller that has gone.
- **The spawner** (`spawner.rs`): `Request::RoomList` and
  `Reply::RoomListed`. It puts the pages together as root: the same
  inode, change time and count of names from the first page to the last,
  every page but the last whole (so at most 16), all within two minutes,
  the Room's start among them. Then the listing goes to the broker as
  JSON in frames; `Spawner::room_list` reads it at the length said, never
  past 512 KiB, and checks it again.
- `docs/arch/trust-boundary.md` and the changelog.

## Before the code, and after it

The design was reviewed independently before any code, and the diff
again before the runs. Neither found anything serious. From the design
review:

- **An honest supervisor could send a page that fails its check**: a
  name read twice (a job recreating it mid-read) is now a change, and a
  name over 255 bytes a failure, never sent.
- **The time didn't cover a Room that starts first**: two minutes, as a
  take's, and the broker waits three.
- **Short pages would make the count of pages only bounded by time**:
  every page but the last must be whole.
- **A change time is as fine as the kernel's clock**: the names are read
  twice a page as well, and the module says what this is: honesty and
  liveness, not a boundary.
- Also: an inode only where there is one; `EACCES` on a name is denied
  and `ENOENT` a change; the folder read through its own descriptor; dot
  steps refused; the frame and listing bounds pinned by tests.

From the diff review:

- **The spawner's own time had no test through the real path**: it is a
  parameter now, and a test holds a listing given no time to being let
  go before its second page.
- **The shared opener took any flags**: it takes a file or a folder,
  read only.
- **An honest race was blamed on the Room**: a later page of the wrong
  length is said as the folder changing.
- The time's documents say the real ceiling: two minutes and one page's
  wait, the broker's three minutes over all.
- Left: the broker's read timeout has no test, as a take's hasn't;
  `AT_NO_AUTOMOUNT` and the folder's open without waiting have no test
  that could see them; the supervisor doesn't check the path again, as
  it is the Room's own.

And one change to the design, from the work order: the workspace itself
may be listed, its inbox seen as another mount, since a job starts in
the workspace and the agent's project may be all of it.

## Tests

- **The wire** (`room::tests`, 5): what a name may be (`..`, `/`, NUL,
  256 bytes, and `0xff`, a newline, 255 bytes allowed); what may be
  listed; the most a page carries fits one frame, asked or answered; the
  wire form, with unknown fields, kinds and reasons refused; and a page
  held to where it was asked, each rule broken alone.
- **The supervisor** (`hatchling-room`, 9): a real folder listed with
  each kind, its inodes and lengths, after a name there and one gone;
  names as bytes, in their order (`B` before `a`); 130, 128 and 1024
  names in whole pages, and 1025 refused; links, files, `..` and the
  workspace's own name; a folder whose names can't be looked at, denied;
  a stand-in folder for each race (names, inode, change time and mount
  each changed alone, a name twice, a name gone) and for another mount;
  what the kernel said half of; and the kernel's words.
- **The session** (`room_session::tests`, 8): a page in its turn, held to
  where it was asked; every way a page can't be the one asked for; pages
  owed when the Room goes down; callers gone; a held session; the words
  for each reason; and a session started for a listing and kept.
- **The spawner** (`spawner::tests`, 8): a folder listed whole through
  the session, and one changed between pages; a folder that may not be
  asked for never reaching the spawner; the listing the broker reads
  held to its length, its bound and its names; the longest listing
  within its bound; whole pages, and how many are asked; a folder
  changed, short or long between pages; late, through `listing` and
  through the spawner's own clock.
- **The installed Room, as root** (`room_disk.rs`, in
  `the_installed_spawner_hands_the_verified_image_to_its_mount_helper`):
  a real folder through the static supervisor under the Room's filter:
  each kind, 130 and 1024 names, 1025 refused, a link, a file and a pipe
  refused as folders, the workspace listed with its inbox a mount, and
  the inbox itself refused as outside.

## Breaks by hand

**45 breaks: 44 run in the sandbox, 0 survived; 1 run as root, caught.**
Each named its tests, and each of those had to fail; the tests passed
before and after them. None failed to build.

- The supervisor (18): a name read twice passing, what can't be a name
  passing, another mount unseen, the names not read again, the folder
  not looked at again, the name asked after included, a page unbounded,
  too many names read, the folder or a name taken half said, a link
  looked through, a pipe a file, the workspace not its own `.`, not a
  folder said as none, a name gone said as failed, a link opened
  through, a folder opened as a file, and the folder opened as anything.
- The wire (8): names repeating, an inode or a length for anything, a
  page past a page, a folder past the bound, dot steps passing, `..` a
  name, anything after at the workspace.
- The session (9): a page unchecked; callers gone launched for or asked
  for; listers not wanted or busy; a page owed dropped; a launch unsaid;
  a change said as a failure; a listing unchecked before a session.
- The spawner (9): a folder changed between pages; only the inode held;
  a first or later page short; never late; the time never running out;
  the listing taken unchecked, unbounded or past its bound.
- **As root**, against a static supervisor built with the break:
  `another-mount-opened-into` (the open crossing onto another mount).
  The installed test failed: a file in the inbox was handed back. Take
  and listing share that one line, and the test reaches take's inbox
  check before the listing's.

The first run of the breaks found one that survived for the wrong
reason: a test's short first page made every page short, so with the
first page's check gone a later one caught it. The test now shortens
only the first page, and the break is caught.

## Mutation

Every mutant on the lines this change adds or changes, each crate with
its unmutated baseline first, its own tests run:

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 50 | 50 | 0 | 0 | 0 |
| `hatchling-room` (the supervisor) | 42 | 31 | 0 | 0 | 11 |
| `hatchling-broker` | 68 | 58 | 0 | 0 | 10 |

Unviable is a function's answer replaced with its type's default, which
it has none of. The first broker run missed 2 and timed out on 3, all in
the tests: a bound's words compared with themselves, a stand-in folder
that answered for ever, and a test that waited for ever on a spawner
that said nothing. The tests now name the words, the stand-in answers no
more pages than a folder can have, and the test reads what was said and
nothing more; the run above is after.

## The gate

On the tested tree, on Rust 1.85.0 and then 1.98.1, the build cache
cleared between: formatting, clippy with `-D warnings` (and with every
feature, the supervisor's among them), the build, the whole workspace's
tests, the seed check, the protocol schema and the repository's audit:
every step exit 0, and on each toolchain 135 test binaries, 4133 tests
passed, 0 failed, 0 ignored. The mutant lists on the tested tree are the
same 50, 42 and 68.

The supervisor's own tests are behind its feature, outside the
workspace's: built static and release as CI builds them, with the
supervisor above, its 61 tests passed and its three proofs under the
Room's filter passed. As root, outside the sandbox, the installed Room's
three tests passed with that supervisor
(`the_spawner_prepares_layout_only_on_the_verified_room_disk`,
`the_installed_spawner_hands_the_verified_image_to_its_mount_helper`,
`an_installed_room_ends_when_its_spawner_is_killed`).
