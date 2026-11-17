# Evidence: a hand-over is spent before its file is checked (#504, WO-043 slice 0)

What was tested, how, and what came back, for the fix to the hand-over's
count: every file checked for secrets spends one of the conversation's
hand-overs first, and a hand-over belongs to the conversation it began in.

| | |
|---|---|
| Base commit | `4e0c903f` (`docs(workorders): queue WO-043, folders and the chat's files (#503)`) |
| Tested tree | `5fa3c15655510138abe63fc6063cb84c2641ef7d` (the gate); the breaks and mutation on `87e33337`, whose `crates/` are the same, file for file (it predates #501's app change, rebased under it) |
| Date | 2026-11-17 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox; the app's check on Windows 11 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## The bug

Found by WO-043's threat model. A hand-over checks a file the agent gives
for the secrets the broker keeps, and one found turns giving off for the
conversation; a conversation may give 16. But the count was spent only
when the file was kept, after the check. A file the Library then refused
as full (by its bytes, near 256 MiB), or one that couldn't be checked,
cost nothing, so an agent could have guesses at a secret checked without
limit. And a new conversation started while a hand-over ran could count,
keep or turn off an old run's file in the new one.

## What changed

- **`Library::spend`** spends one of the conversation's hand-overs, sealed,
  held to the file's real size, before the file is checked. A file the
  Library couldn't keep (too long, off, spent, full, or another
  conversation's) is refused and nothing is spent, and the file is never
  checked.
- **`Library::keep_spent`** keeps a spent hand-over's file without counting
  it again: refused when the conversation has moved on, giving was turned
  off since, or the Library has no room.
- **The Library numbers its conversations**, sealed in its state; a new
  conversation is the next number. A state from before reads as the first.
  The hand-over reads the number before it knows its run is the chat's,
  and holds `spend`, `keep_spent` and `turn_off_in` to it.
- **`Library::keep` and `turn_off`**, which spent nothing before a check,
  are test-only now.
- The security page and the changelog. WO-043 is in progress.

## Before the code, and after it

An independent review of the diff, before the runs, found nothing serious:
every path from reading a file to checking it runs through a sealed
spend; errors fail closed (counted, never not counted); a stale run's
answer is the same whether or not its file held a secret. It found:

- **Turning giving off wasn't bound to the conversation**: a secret found
  by an old run, as the owner started a new conversation, could turn the
  new one off. `turn_off_in` holds it to the hand-over's conversation now.
- **The conversation's order had no test through the hand-over itself.**
  Two do now: a Room whose file is read as a new conversation starts (the
  file is never checked or counted), and checks during which one starts
  (nothing kept or turned off in the new one).
- **`keep` had no caller left but tests**; it is test-only now.
- **A failed write of "off" leaves giving on.** This is older than this
  change, and is [#505](https://github.com/undergroundrap/hatchling/issues/505).
- Left as they are: a refusal before the check (too long, full) is still
  free, as it was, and checks nothing; a run that reads the conversation
  in the moment between the chat's new conversation and the Library's is
  refused once as stale, and its next try works.

## Tests

- **The Library** (`library::tests`): a hand-over spent before its file is
  kept, and kept without counting again; spent to the last, the next
  refused; a spend refused (too long, no room for its bytes, off) spends
  nothing; a hand-over from a conversation since ended neither spent nor
  kept; an old conversation's secret turning the new one nothing off; a
  state from before conversations were numbered read as the first.
- **The hand-over** (`hand_over::tests`): a file the Library couldn't keep
  never checked, twenty times over, nothing spent; every file checked,
  one that couldn't be checked too, spending a hand-over, and none read
  past the last; a file read as a new conversation starts never checked
  or counted; a file checked as one starts neither kept nor turning it
  off; and the earlier tests, now each a checked file spending one.

## Breaks by hand

**12 breaks, 0 survived.** Each named its tests, and each of those had to
fail; the tests passed before and after them. None failed to build.

- `no-spend-before-the-check`, `spent-at-no-size`, `a-spend-counts-nothing`,
  `a-spend-ignores-its-conversation`, `a-keep-ignores-its-conversation`,
  `a-spent-keep-counts-again`, `a-spent-keep-is-refused-as-spent`,
  `a-new-conversation-keeps-its-number`, `an-old-state-is-unreadable`,
  `turning-off-ignores-the-conversation`, `the-conversation-read-at-the-spend`,
  `the-hand-over-keeps-and-counts-again`.

## Mutation

Every mutant on the lines this change adds or changes, with the broker's
unmutated baseline first, its own tests run: **15 mutants, 11 caught, 0
missed, 0 timeouts, 4 unviable** (a function's answer replaced with its
type's default, which has none).

## The gate

On the tested tree, on Rust 1.85.0 and then 1.98.1, the build cache
cleared between: formatting, clippy with `-D warnings` (and with every
feature), the build, the whole workspace's tests, the seed check, the
protocol schema and the repository's audit: every step exit 0, and on
each toolchain 135 test binaries, 4112 tests passed, 0 failed, 0
ignored. The mutant list on the tested tree is the same 15. The app's
check (`npm run check`): 565, 235 and 16 tests passed, 0 failed.
