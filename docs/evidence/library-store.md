# Evidence: the Library's store (WO-041, slice 3, the store)

What was tested, how, and what came back, for the sealed store the files
the agent hands its owner will be kept in. Nothing hands a file over yet:
the checks, the agent's tool and the events are the next change, and the
tool is offered only once the app can download and remove a file.

| | |
|---|---|
| Base commit | `e6532985` (`feat(room): read a file out of the Room whole, to hand it over (WO-041) (#493)`) |
| Tested tree | `ea131299d1d85a953885bf4a4320a47d774af320` |
| Date | 2026-11-17 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What changed

- **`Store::LIBRARY`** (`hatchling-vault`), the eighth store the keeper
  hands the broker, the most one handoff carries; and a test that every
  store the broker can have fits one handoff.
- **`HATCHLING_LIBRARY_DIR`** (`scheduler.library`): off unless set; it
  needs the scheduler, and is kept apart from the vault, the schedules and
  their runs, the build store, the spaces and the conversation's store,
  and out of the cell's reach. The conversation's setting and the
  Library's are read by one function now (`parse_chat_store`), with the
  conversation's behaviour unchanged.
- **The store** (`library.rs`): a file as sealed chunks of 32 KiB, an
  index (id, name, length, SHA-256, chunks, when) and a state (what is
  pending, and the open conversation's count and off). Its operations:
  `keep`, `refuses`, `turn_off`, `new_conversation`, `standing`, `files`,
  `fetch`, `remove`.
- The sweep keeps the Library's directory; backups take it; `reset agent`
  and `reset everything` remove it, `reset conversation` keeps it.
- **The vault's fault switch** (tests only) can now fail the nth time a
  step of a write is reached, not only the next.
- `docs/storage.md`, `docs/backup-and-reset.md`, the changelog, WO-041.

## Before the code, and after it

The design was written first and reviewed independently against the code
before any of it. The review found that the planned scan would have
reported every file as holding a secret on a default install (no review
policy is set), that numbered conversations restart at 0 with the broker
so an off kept by number would be lost, that recovery couldn't tell a
keep cut short from a removal cut short, that a file's name went
unchecked into Activity, that an unreachable keeper would be told as a
secret found, and that a tool nothing could empty the Library after
would be offered before the app could. Each changed the design. Those
for the hand-over are the next change's; this store keeps one count and
one off for the open conversation, recovers by the kind of change, and
counts a hand-over in the same write as its pending.

A second independent review, of this diff, before the runs below, found:

- **A keep could fail with its file kept.** A write that fails after its
  rename leaves its new state visible: an index write failing at its
  directory sync, or the state cleared after it, had `keep` say it
  failed, and the next operation then finished it. Now what the index
  says after a failed write decides, and once it names the file, the keep
  says it was kept. A removal, once its pending is written, is made.
- **Nothing tested the order inside `keep` and `remove`.** The crash
  tests wrote a state by hand. The vault's fault switch now fails any
  step of any write, and every write a keep and a removal make is failed
  before its rename and after it.
- **A pending change's chunks weren't bounded.** Now at most a longest
  file's.
- Smaller: `reset agent` taking the Library had no test of its own; the
  docs said a file is read back to its length and hash, where the hash
  alone is checked (a file cut short has another); a full index with the
  longest names wasn't shown to fit one sealed entry; nothing pinned the
  hash's form for the app's download to come.

Each was changed before the runs. Left as they are: the vault logs each
chunk it writes or removes as a line of its own, so an 8 MiB file is 256
lines; and the Library, like the conversation's store, isn't checked
against the memory, goals or web stores' directories.

## Tests

- **What it keeps, and gives back** (`library::tests`): files of 0, 1,
  32 KiB less one, 32 KiB, 32 KiB and one, three chunks and seven, and
  8 MiB, each read back whole; one byte past 8 MiB refused and nothing
  written or counted; the same bytes kept twice as two files; the hash
  as SHA-256 in lowercase hex, by a known answer.
- **Its bounds**: names (plain ASCII, at most 64 bytes, not hidden, no
  path, no bidi or NUL); 16 a conversation, and the 17th refused until a
  new conversation; off, kept through another Library opened on the same
  store as a restart would open it, until a new conversation; 64 files,
  with the longest names, then full, nothing evicted, and room again
  after a removal; every refusal in order and at each bound, by bytes to
  the byte and by files.
- **What it reads back**: a chunk of another file in a file's place, the
  last chunk cut short, a middle one cut short, the first gone, and the
  bytes changed at the same length, each read as damaged, and the other
  files as they were.
- **What it holds to**: an index or a state that can't be one (an id that
  isn't 64 lowercase hex, too short or the right length; a name that
  isn't one; past 8 MiB; chunks that don't fit the length; 65 files; a
  count past 16; a pending change naming no id, or more chunks than a
  file has) is refused; the bounds themselves are one.
- **A crash, at any point**: every write a keep makes (the pending and
  its count, each chunk, the index, the state cleared) failed before its
  rename and after it: up to the index the file isn't kept, is said not
  to be, leaves no chunk, and still counts; from the index on it is kept,
  and said to be. Every write a removal makes failed the same way:
  before its pending is visible the file stays; after, it is removed,
  and said to be. A keep cut short with chunks written, a keep cut short
  after its index, and a removal cut short before and after its index,
  each finished or undone by the next operation.
- **At once**: four threads keeping six files each keep sixteen and are
  refused the rest.
- The setting (`config::tests::the_library_is_placed_apart`), the handoff
  (`vault_ops::tests::the_brokers_stores_fit_one_handoff`), the label,
  the sweep and the reset each have theirs.

## Breaks by hand

**35 breaks, 0 survived.** Each named its tests, and each of those had to fail; the tests passed before and after them. A build that failed would have been counted on its own, never as caught: none did. Each break, by its name:

- `a-hidden-name-is-kept`.
- `a-long-name-is-kept`.
- `any-name-is-kept`.
- `any-id-is-one`.
- `a-file-past-the-bound-is-kept`.
- `off-isnt-looked-at`.
- `one-more-is-handed-over`.
- `one-more-file-is-kept`.
- `one-more-byte-is-kept`.
- `a-keep-isnt-counted`.
- `turning-off-turns-nothing-off`.
- `a-new-conversation-stays-off`.
- `a-new-conversation-keeps-its-count`.
- `any-bytes-are-read-back`.
- `an-index-may-say-any-chunks`.
- `an-index-may-say-any-length`.
- `an-index-may-say-any-name`.
- `a-state-may-say-any-count`.
- `a-state-may-name-any-file`.
- `a-keep-cut-short-is-left`.
- `a-keep-done-is-undone`.
- `a-removal-cut-short-keeps-its-file`.
- `a-crash-is-recovered-every-time`.
- `a-chunk-gone-stops-a-removal`.
- `the-library-isnt-handed`.
- `the-library-isnt-read-from-its-setting`.
- `the-library-may-overlap-the-conversation`.
- `the-sweep-takes-the-library`.
- `the-library-has-no-label`.
- `a-kept-file-is-said-not-kept`.
- `a-written-removal-is-said-failed`.
- `a-pending-may-say-any-chunks`.
- `the-count-comes-last`.
- `the-chunks-come-before-the-pending`.
- `the-hash-is-another`.

## Mutation

Every mutant on the lines this change adds or changes, crate by crate, each with its unmutated baseline first, only that crate's own tests run.

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| hatchling-vault (with its test switches) | 13 | 12 | 0 | 0 | 1 |
| hatchling-broker, `library.rs` | 101 | 91 | 0 | 0 | 10 |
| hatchling-broker, the rest (config, the stores, the sweep) | 8 | 4 | 0 | 0 | 4 |

The vault's and the rest of the broker's were run on tree `f8caaabb7de0cc8421a6daf9b58ce088bc8308ef`, whose code differs from the tested tree's only in the Library's tests. That run missed three in `library.rs`: an id's length and its digits, the bound on a pending change's chunks, and what the Library says of itself; the tests now hold each, and every mutant of `library.rs` was run again on the tested tree, above.

## The gate

On the tested tree, on Rust 1.85.0 and then 1.98.1, the build cache cleared between: formatting, clippy with `-D warnings` (and with every feature), the build, the whole workspace's tests (135 binaries, 4064 passed, 0 failed, on each), the seed check, the protocol schema and the repository's audit: every step exit 0.

## What wasn't tried

- **A store of 256 MiB through a backup and a restore.** Backups take the
  Library file by file, as every store; no backup of a full one was made.
- **A real crash**, the power cut kind: the writes were made to fail at
  each step, in a test, not interrupted.
- **Many files at the speed a chat would hand them over.** A keep is
  about two writes and two syncs a chunk, under the store's lock.
