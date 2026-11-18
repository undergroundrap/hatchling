# Evidence: the chat's cards across a restart (WO-043, slice 5)

What was tested, how, and what came back, for the change that shows
each file or folder the agent gave under the reply that gave it, after
a restart too, and says when one was removed since.

| | |
|---|---|
| Base commit | `bab5ed65` (`feat(api): list, page, read out and remove the Library's folders (WO-043) (#513)`) |
| Tested tree | `5c2be94186d25915bea518c7c5a7ded9a35799ea` |
| Date | 2026-11-18 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox; the app's checks on Windows 11, Node 22, from Git Bash |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What changed

- **The conversation store** keeps, with each kept exchange, the cell
  id of the run that answered it. They go in the index's `runs`, in the
  same write as the exchange. The list is either empty (an older store)
  or exactly as long as the exchanges, and each id is held to the run
  grammar on write and on read. A restart shows each kept answer with
  its run (`ChatMessage.run`). The chat passes the run with the keep.
- **The Library** keeps what the open conversation gave and the owner
  removed (`removed`), at most 16, in the same state write that makes
  the removal pending, so it survives a crash.
  - It is cleared by a new conversation and by a restart's regroup.
  - It is never written for an item kept before items had replies.
  - It is dropped, never damage, when it can't be one.
  - It is hidden while the index still names its item.
- **A new conversation is two steps.** First the Library moves to its
  next conversation with handing over off. Then the chat moves on, and
  once it has, the Library turns handing over back on. One the Library
  can't be told of doesn't start. If either step fails, handing over
  stays off.
- **The wire** (v8, unreleased): `list_library` answers `removed`; a
  kept folder is said as `folder_offered`, as a file is.
- **The app:**
  - The live adapter keeps the chat's runs, a folder's notice and the
    removed items.
  - The chat draws what a reply's run gave under it: a file with
    Download, a folder by its files, one removed since.
  - What the open conversation gave under replies no longer shown sits
    apart, under none, never under another's reply.
  - An item from before replies is never placed. An item a live notice
    showed isn't repeated.
  - The app reads the Library again after a new conversation.
  - The frozen contract is untouched.
- **Docs:** `docs/local-api.md`, `docs/arch/trust-boundary.md` and the
  changelog, with a downgrade note.

## Before the code, and after it

The design was reviewed independently before any code, and the diff
again before the runs.

From the design review, five serious findings, all taken:

- **Items from before replies** would have filled the orphan block and
  broken the 16 bound: never a card or a tombstone.
- **A failed `library.new_conversation`** left the Library on the old
  number while the chat moved on: the Library is told first.
- **A folder had no live notice**, so a run still answering left it
  orphaned: `folder_offered`.
- **The tombstone belongs in the pending write**, not the best-effort
  final one.
- **The runs list kept in the chat could drift**: the store appends it
  in its own write.

From the diff review, no wrong-card path. Three serious findings, all
taken:

- **Telling the Library first cleared "off" before the chat moved on.**
  In that window a run still in a conversation where a secret was found
  could hand one thing over. Now two steps, off until the chat has
  moved.
- **The fallback after a failed chat store failed open**, untested: the
  two steps remove it.
- **A tombstone that can't be one locked the Library**, and with it the
  owner's new conversation: dropped and logged instead.
- And tests for each guard a one-line change would have broken unseen:
  - the restart seam, from the store's run through to the replayed
    answer;
  - the tombstone surviving each removal failure;
  - the app's runs wiring, proven with a listed item;
  - a removed item's id;
  - the tombstone's bounds;
  - the API's `removed`.

Left: the chat view's own placement (`features/chat/chat.ts`) has no
unit test of its own. Its helpers are tested directly
(`file-out.test.mjs`); the view has no test harness yet.

## Tests

- **The store:**
  - each exchange kept with its run, the oldest going with its exchange;
  - a run that names none, or can't be one, kept as none;
  - an older index read with none for each;
  - runs that aren't the exchanges' read as damage;
  - each answer shown again with its run, through the start's read too.
- **The chat:** the run passed with each kept exchange.
- **The Library:**
  - what the open conversation gave and was removed, only its own and
    never an item from before replies;
  - cleared by a new conversation and by a restart;
  - at most 16, the oldest going;
  - never said while kept;
  - one that can't be, dropped;
  - a new conversation begun off and finished on;
  - the removal's tombstone through each failure.
- **The API:**
  - `removed` listed;
  - a new conversation the Library can't be told of refused, the chat
    staying;
  - one the chat store refuses leaving handing over off.
- **The hand-over:** a kept folder said as `folder_offered`.
- **The app:**
  - cards joined by run;
  - live notices not repeated;
  - the orphan block, by conversation;
  - folder notices kept and removed;
  - the adapter's runs wired;
  - the chat's helpers;
  - 100% of the live adapter's lines, branches and functions, by the
    gate run from bash with CI's quoting.

## Breaks by hand

**19 in the broker, 0 survived; 9 in the app, 0 survived.**

- **The broker:**
  - a new conversation past the Library;
  - begun with giving on;
  - finished for any conversation, or never finished;
  - no run kept, or any run kept;
  - runs of any count or grammar;
  - answers shown with no run;
  - the chat saying no run;
  - removed from any conversation, or before replies;
  - removed unbounded;
  - removed across a new conversation, or a restart;
  - removed unheld, or said while kept;
  - removed unlisted;
  - a folder said as a change.
- **The app:**
  - a card from before replies;
  - a noticed item or removal again;
  - any reply joining;
  - the orphan block from any conversation;
  - a removed item's name or id unchecked;
  - a folder notice unchecked;
  - the runs unwired.

## Mutation

cargo-mutants on the lines this change made, against its base
(`--in-diff`), each crate's library tests, on 1.98.1, in the sandbox.

- **hatchling-core:** no mutants. Its changed lines are wire types and
  their tests.
- **hatchling-broker, first run:** 70 mutants: 61 caught, 2 missed, 0
  timed out, 7 unviable. Both misses were equivalent, and each was
  removed by making the code plainer, not by excluding it:
  - `keep_if`'s `kept > 0` as `>= 0`. The newest run was pushed only
    when an exchange was kept. With none kept, the `take(kept)` after it
    drops whatever was pushed. The guard did nothing, so the push is
    now unconditional.
  - `start_new`'s `next` deleted from `Index { next, ..Index::default() }`.
    This change had rewritten the literal with a base; carrying `next`
    across a new conversation is WO-026's and no reader can tell it from
    0, since every slot is cleared. The literal is back to its explicit
    fields, with `runs` added, as on main.
- **hatchling-broker, the store again on the tested tree** (`--re
  conversation_store`; no other file changed): 25 mutants, 24 caught, 0
  missed, 0 timed out, 1 unviable.
- **The store's five hand-breaks again on the tested tree:** none
  survived, the tests passing before and after.

The seven unviable mutants replace a function returning a type with no
`Default` (`UiActionResult`, `RoomResult`, `Shelf`, `LibraryRemoved`,
`Step`, `Resumed`), so they can't build. On the tested tree the slice's
mutants number 0 for the core and 66 for the broker: the first run's 70
less the four on the two lines removed.

## The gate

On the tested tree, on Rust 1.85.0 and then 1.98.1, the build cache
cleared between: formatting, clippy with `-D warnings` (and with every
feature), the build, the whole workspace's tests, the seed check, the
protocol schema and the repository's audit: every step exit 0, and on
each toolchain 135 test binaries, 4206 tests passed, 0 failed, 0
ignored. The 28 lines of its log that say "skipped" are test names, as
before.

The app's checks, on its final code with the protocol schema regenerated
and its types generated from it: 578, 242 and 16 tests passed, none
failed, no lint or design error, and the seam check: the frozen
contract's four files unchanged. The live adapter's coverage gate, run
from bash with CI's quoting: 100% of its lines, branches and functions.
The app's code didn't change after these ran.
