# Evidence: the Library's items (WO-043, slice 2)

What was tested, how, and what came back, for the change that lets the
Library keep a folder as one item, grouped by the conversation and the
reply it came from, within bounds on items, bytes and files, and quietly
in the broker's log. Giving a folder is a later slice: nothing keeps a
folder yet but the tests.

| | |
|---|---|
| Base commit | `c949629f` (`feat(room): list a folder in the Room, a page at a time (WO-043) (#507)`) |
| Tested tree | `004f6fbf4f61843d6661d64429d4b51011e99c0c` (the gate, the breaks and the last mutation run); the first full mutation run on `90a711d0`, which differs from it in `library.rs`'s `at_start` and one comment, `api.rs`'s call to it, and the tests |
| Date | 2026-11-18 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What changed

- **Items** (`library.rs`): an item is a file or a folder. A folder's
  files are kept back to back in 32 KiB chunks, with a sealed list of
  each one's path, length and SHA-256, its hash in the index; each file
  is read back only whole and only to its own hash, so damage to one
  leaves the others readable. The list is read back only to the index's
  hash, at most 589,826 bytes, its count and lengths the item's and its
  paths the broker's rules'. The index says, per kind, what an item may
  be, and an item it can't be is damage.
- **Bounds**: the Library holds 64 items, 256 MiB and 4096 files; a file
  at most 8 MiB; a folder at most 32 MiB, 512 files, 16 steps deep, a
  path at most 1024 bytes and a step 255. A folder's paths are printable
  ASCII but `< > : " \ | ? *`, no space at either end of a step and no
  dot at its end, no device or reaching name Windows has at any depth,
  no two equal but for case, and no file where another path has a
  folder (`check_paths`).
- **Spending** (`spend`, `Spent`): a hand-over is spent, sealed, before
  anything is read, as #504 made it, and the spend returns a receipt the
  keep takes by value: a keep is refused unless it keeps exactly what was
  spent, in the conversation it was spent in. Besides its 16 hand-overs,
  a conversation reads at most 128 MiB out of the Room: a file's bytes
  count once read, kept or not; a folder's count up front, from its
  listing. A Library full by its bytes refuses before anything is read.
- **Grouping**: each item records the conversation it came from (the
  Library's own number for it) and its reply (the chat run's cell id,
  `run-` and 24 hex digits): both the broker's, never the Room's word.
  At the broker's start, once, a conversation the chat doesn't resume is
  grouped apart (`at_start`, `regroup`); a restart gives nothing back:
  the count, the bytes read and off are carried until the owner starts a
  new conversation.
- **The vault's log** (`hatchling-vault`): `store_quietly` and
  `remove_quietly` write and remove at debug; the Library uses them for
  its chunks and logs one info line per item kept, removed or undone,
  by id, kind, files and bytes, never a name. A store of secrets still
  logs every write and removal.
- **The API's Library use** counts every item, a folder among them; the
  list still serves files only, until slice 4.
- `docs/storage.md`, `docs/local-api.md`, `docs/your-security.md` (and
  its text in the app), `docs/arch/trust-boundary.md` and the changelog.

## Before the code, and after it

The design was reviewed independently before any code, and the diff
again before the runs. From the design review, two serious findings,
both taken:

- **The reply was the chat's session id**, the same for every run of a
  session: it is the chat run's cell id, fresh for every run, held by the
  hand-over and checked as one.
- **A folder could be spent a little at a time** as it was read: it is
  spent once, up front, from its listing's totals, every bound checked
  before anything is read.
- Also: every byte read counts, kept or not; quiet writes and removals,
  never for a store of secrets; what each kind of item may be in the
  index; no agent-chosen path in any error or log line; the owner's use
  counted over all items; the path rules; the list's own bytes bounded;
  and the measurements below.

From the diff review, one serious finding, taken:

- **A restart gave a conversation back its budget**: the start-up rule
  cleared the count, the bytes read and off whenever the chat didn't
  resume its conversation, so a restart mid-conversation gave back 16
  hand-overs and a conversation turned off. A restart now only groups
  apart; only the owner's new conversation clears them.
- Also: a keep bound to its spend (`Spent`, taken by value); a new keep
  names a reply; a Library full by its bytes refuses before reading; the
  time an item was kept is printable ASCII; the start-up decision tested.
- Left, with reasons: `at_start`'s once is spent even when grouping
  fails (the cost is grouping only, never a budget, and it is logged at
  error); concurrent hand-overs can each pass the check before reading
  and read past 128 MiB by at most one file each (bounded); `CONIN$`,
  `CONOUT$` and `CLOCK$` aren't devices to the shared rule (older than
  this slice; a follow-up).

Filed, older than this slice: a backup taken during a keep can hold
chunks without the pending keep that names them
([#508](https://github.com/undergroundrap/hatchling/issues/508)); one
directory sync per chunk
([#509](https://github.com/undergroundrap/hatchling/issues/509)).

## Measured

On ext4 in the sandbox, release build, three runs each:

- Keeping a 32 MiB folder (1,024 chunks): about 8.5 s, the Library's
  other operations waiting meanwhile. Reading it back: 70 ms.
- Filling the Library to 256 MiB: about 60 s more. On disk: 256.3 MiB in
  8,195 entries. Removing it all: 1.0 s. Peak memory: 25 MB.

## Tests

`library::tests` (40), with the hand-over's, the API's, the
conversation store's and the run launcher's touched:

- A folder kept and each file read back to its own hash, across chunks,
  an empty file among them; one damaged file leaving the others
  readable; a list that disagrees with its item (count, lengths, hash,
  paths, case), padded past its bound, and at its bound.
- The path rules, each broken alone (`CON .txt` among them); a folder
  spent whole or not at all, at each bound; every byte read counting; a
  keep cut short and recovered; a pending change of the most chunks.
- A keep with a reply, a time or a list that can't be one keeps nothing,
  and one at the list's bound keeps; a keep of what wasn't spent, or
  spent in another conversation, refused.
- Quiet chunks, loud items; an old index and an old state still read;
  the largest index and list fit where they are kept; a Library full by
  its bytes refuses before reading; a store of secrets still logs.
- At start: a conversation not resumed grouped apart once for the
  broker's life, however often asked, one resumed not, and a restart
  giving nothing back.
- The vault (`seal::tests`): a quiet store and removal do what the loud
  ones do.

## Breaks by hand

**54 breaks, 0 survived.** Each named its tests, and each of those had
to fail; the tests passed before and after them. None failed to build.

- Items and the index (11): any reply, hash or time kept; a time of any
  text; a file of many files; a folder of any count or list; the
  Library's bytes or files unsummed; a state of any read; a pending list
  of any length.
- The path rules (12): dot steps, long steps, not ASCII, the forbidden
  characters, spaces and dots, devices, reaching names, long and deep
  paths, case twins, a file where a folder is, a folder of any files.
- Spending and the budgets (10): an empty folder; the bytes read
  unchecked; the files in all unbounded; a refused file, or a spent
  hand-over, read for free; a new conversation keeping its bytes; a
  stale keep; a keep of anything spent; an empty reply kept; a Library
  full by its bytes still reading.
- Start-up (3): every start a new conversation; a restart giving back;
  an empty conversation going on.
- Folders read back (10): a folder read as a file; folders listed as
  files; a list of any length, count, hashes, paths or size; every file
  read from the start; no list kept; a list left behind.
- The log (5): chunks logged; removals logged; quiet loud; a secret
  written or removed quietly.
- The API and the hand-over (3): the API listing folders; the Library's
  use counting files only; no reply kept.

The first round left two alive, both now caught: the Library's bytes
unsummed (no test had too many bytes in few files) and folders listed as
files (the API filtered them too; each is now its own break).

## Mutation

Every mutant on the lines this change adds or changes, each crate with
its unmutated baseline first, its own library tests run:

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| `hatchling-vault` | 5 | 5 | 0 | 0 | 0 |
| `hatchling-broker` | 246 | 214 | 4 | 0 | 28 |
| `hatchling-broker`, the changed functions again | 14 | 13 | 0 | 0 | 1 |

Unviable is a function's answer replaced with its type's default, which
it has none of. The broker's full run missed four, each a test missing:

- `||` to `&&` in the keep's reply and time check: no test kept with a
  reply that isn't one but isn't empty.
- `>` to `==` and to `>=` on the list's bound in the keep: no test kept a
  list at its bound or past it. A folder's own list can't reach it (the
  path rules bound it, a test pins that); the check is the keep's last
  word, now tested at the bound and one byte past it.
- `at_start` with nothing done: its once lived inside it, so only one
  test in a process could call it. The broker now keeps the once and
  passes it in; the test calls it twice with one once.

The run after those tests, of the mutants in the functions changed
(`keeping`, `at_start`, `start_unless`), is the last row.

Earlier runs, all before the tree above, found five timeouts in two
loops that advanced an index by hand (the path rules' folder check and
the chunk writer), now counted loops, and two misses on the pending
record's bounds, now tested at the most chunks a folder and its list
take.

## The gate

On the tested tree, on Rust 1.85.0 and then 1.98.1, the build cache
cleared between: formatting, clippy with `-D warnings` (and with every
feature), the build, the whole workspace's tests, the seed check, the
protocol schema and the repository's audit: every step exit 0, and on
each toolchain 135 test binaries, 4152 tests passed, 0 failed, 0
ignored. The 28 lines of its log that say "skipped" are test names, the
same as the last slice's. The mutant lists on the tested tree are the
same 5 and 246 as the full runs'.

The app's own checks, on the same change: 565, 235 and 16 tests
passed, none failed.
