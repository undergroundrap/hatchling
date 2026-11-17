# Evidence: the Library on the local API (WO-041, slice 4, the broker's side)

What was tested, how, and what came back, for the local API's Library:
the list, a file read out in parts, and removal, the owner's alone; and
the hand-over's refusal of files Windows acts on when their folder is
shown. The app's card, its Library in Settings, its download and the
packaged install turning the Library on are the next change.

| | |
|---|---|
| Base commit | `0446aff2` (`feat(broker): the agent hands the owner a file, checked, into the Library (WO-041) (#495)`) |
| Tested tree | `dde9a010171118bcde73cad72efed7f69c82f0ce` |
| Date | 2026-11-17 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox; the app's tests on Windows 11 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0, Node for the app |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What changed

- **UI schema v7.** Owner-only actions, each its own audit kind:
  `list_library` (answered `library`: the files newest first, and the
  Library's `standing` against its bounds and the open conversation's),
  `begin_file_out` and `file_out_part` (answered `file_out_begun` and
  `file_out_part_read`), and `remove_library_file` (answered
  `library_file_removed`, and every app told by `library_changed`).
  `FileOutRefusal`: `gone`, `damaged`, `busy`, `unknown_download`,
  `out_of_step`. `LibraryFile` gains `kept_at`.
- **The held copy** (`file_out.rs`). A file is read back whole and to
  its hash once (`Library::read_out`, which tells a file gone from one
  damaged, built in a buffer wiped when dropped and never grown past the
  index's length), and held while its parts are asked in order. At most
  two copies at once in the broker; one a connection. A copy goes after
  its last part, when its connection begins another download or ends,
  when its file is removed, and a minute after its last part was asked,
  by a sweeper that looks every five seconds. A busy broker refuses
  before it reads anything.
- **The audit trail** keeps a download's beginning and a removal, one
  line each, and no line for a part answered.
- **The hand-over** refuses `desktop.ini` and names ending `.url`,
  `.website`, `.scf`, `.lnk`, `.pif`, `.library-ms`, `.search-ms`,
  `.searchconnector-ms`, `.theme`, `.themepack` and `.deskthemepack`,
  whatever their case: such a file can name a share on another machine,
  which Windows connects to, with the owner's sign-in, when the folder it
  was saved in is opened. The agent is told to give an archive instead.
- **The app** knows the new events and results, and ignores them until
  the next change; its hello is v7.
- docs/local-api.md, the protocol pages, the example client, the
  changelog.

## Before the code, and after it

The design was written first, as a proposal checked against the code by
an independent review, which changed it: a domain refusal that keeps
`damaged` apart from `gone`; the audit's parts exempt; when a file was
kept, and the newest first; a file of no bytes as no parts; the held
copy's bounds; the Windows folder-view names refused rather than
flagged; the Library's use shown against its bounds.

A second independent review, of this diff, before the runs below, found:

- **The held copy wasn't let go after a minute.** It was let go only
  when another download began or its connection ended, and a connection
  can last days; nor did removing a file let go of a copy being read.
  Now a sweeper lets go of an idle copy, and a removal lets go of its
  file's copies.
- **Two guards had no test that failed without them**: a part asked on
  another connection with the first's real id, and a connection's next
  download letting go of its last. Each has one now.
- **The tests shared the broker's holder at its bound.** Each now holds
  its downloads apart.
- **Plaintext outside the wiped copy**: the read built a plain buffer,
  dropped unwiped when damaged, and could grow past its length. Now it is
  wiped, and refused as damaged before it would grow.
- **The file was read before the room check.** Now a busy broker reads
  nothing.
- **Three more Windows names**: `.website`, `.pif`, `.deskthemepack`.
- **Docs**: a minute's hold was claimed before it was true; a part after
  the last is `unknown_download`, not `out_of_step`; `unavailable` for a
  Library that can't be read; the 16 MiB bound as a number; the wiping
  claim narrowed to the held copy.

Left as they are, and said: the audit trail records that a download or a
removal was made, never which file; the vault logs each chunk it reads as
a line of its own (256 for an 8 MiB file), a decision for the next
change.

## Tests

- **The held copy** (`file_out::tests`): a file read out in order whole
  and let go after its last part; parts whole but the last, 256 for
  8 MiB; a file of no bytes is no parts and holds nothing; a part out of
  order lets the copy go; two at once, the third busy, still busy just at
  the minute, room once one is idle; a part keeps a copy from going idle;
  a sweep lets go of idle copies alone; the sweeper itself lets go of a
  copy left idle, with no other call; a download let go, or its file
  removed, is let go.
- **The actions** (`api_ops::tests`): the list newest first, with its
  standing, and a file read out whole in order and let go after its last
  part; a file of no bytes; one gone; a damaged file said damaged, a
  removed one gone, every app told once, and a copy being read let go
  with its file; a download is its connection's, and its next lets go of
  its last; past the bound, busy, until one ends; nobody but the owner
  lists, reads out or removes, a caller holding grants among them; no
  Library, `unavailable` for each; a connection that ends lets go of its
  download.
- **The audit** (`api::tests`): a download's beginning is a line, a part
  answered isn't, a part refused is.
- **The Library** (`library::tests`): a damaged file reads out damaged,
  not gone; one not kept, gone.
- **The names** (`hand_over::tests`): every Windows folder-view name, in
  any case, refused; names that only contain one of them, given.
- **The wire** (`ui::tests`): every action, result, refusal and the
  event in their wire form; the schema the app is generated from,
  checked.

## Breaks by hand

**25 breaks, 0 survived.** Each named its tests, and each of those had to
fail; the tests passed before and after them. A build that failed would
have been counted on its own, never as caught: none did. Each break, by
its name:

- `idle-at-the-bound`.
- `copies-never-go-idle`.
- `one-more-copy-is-held`.
- `there-is-always-room`.
- `the-last-part-is-kept`.
- `parts-in-any-order`.
- `no-sweeper`.
- `the-sweep-sweeps-nothing`.
- `the-sweeper-never-sweeps`.
- `a-removed-file-stays-held`.
- `a-part-keeps-nothing-fresh`.
- `any-connection-reads-a-download`.
- `the-next-keeps-the-last`.
- `a-removal-keeps-the-copy`.
- `a-removal-tells-nobody`.
- `anyone-reads-the-library`.
- `a-closed-connection-keeps-its-copy`.
- `damaged-is-said-gone`.
- `the-list-is-oldest-first`.
- `parts-are-audited`.
- `a-damaged-copy-reads-as-gone`.
- `desktop-ini-is-given`.
- `names-that-reach-out-are-given`.
- `reaching-names-by-case`.
- `website-is-given`.

An earlier run had one survive: the owner's check, tested with a caller
holding no grants at all, which the API refuses before the Library is
reached. The test now uses a caller holding grants who isn't the owner.
The run on the tested tree is the one counted.

Two guards are kept that no answer shows, and so no test or break
reaches: the room check before a file is read (a busy broker is refused
the same either way; the check saves the read), and the read refused as
damaged before it would grow past its length (the hash would refuse it
too; the check keeps an unwiped copy from being left behind).

## Mutation

Every mutant on the lines this change adds or changes, crate by crate,
each with its unmutated baseline first, only that crate's own tests run,
on the tested tree.

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| hatchling-core | 1 | 0 | 0 | 0 | 1 |
| hatchling-broker | 71 | 55 | 0 | 0 | 16 |

Every unviable mutant is a function's answer replaced with its type's
default, which has none. A run on the tree before the tested one missed
one: the sweeper's thread replaced with nothing, where the test only
looked that it was started. The test above watches it let a copy go.

## The gate

On the tested tree, on Rust 1.85.0 and then 1.98.1, the build cache
cleared between: formatting, clippy with `-D warnings` (and with every
feature), the build, the whole workspace's tests (135 binaries, 4103
passed, 0 failed, on each), the seed check, the protocol schema and the
repository's audit: every step exit 0. The app's check (`npm run check`):
531, 207 and 16 tests passed, 0 failed.

## What wasn't tried

- **A download through the app**, or on an install: the next change's,
  with its acceptance on a real install.
- **The sweeper over a broker's life**: it was shown to let a copy go
  with short times in a test; the broker's minute and five seconds weren't
  waited out.
- **Memory**: the bound (two copies of at most 8 MiB) is the code's, not
  measured.
