# Evidence: an off that can't be written is owed (#505), and the console's device names (#511)

What was tested, how, and what came back, for two fixes in the hand-over
paths.

| | |
|---|---|
| Base commit | `ebbc1a29` (`fix(app): show the agent's files on the Library page, and keep them up to date (WO-043) (#525)`) |
| Tested tree | `50a03cfac32f6a023c74a8aa283bf1f18ff74e72` |
| Date | 2026-11-19 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |

This record was added after the runs, in its own commit. It changes no code:
the tested tree is the tree of the commit before it.

## #505: the attack, and the defense

**The attack.** The agent gives files, and each is checked for the secrets
Hatchling keeps. A hit is a confirmed guess: the agent is told the file was
withheld because it held a secret, and giving is turned off for the rest of
the conversation, so no further guess can be checked. Turning it off was one
write to the Library. If that write failed (a disk error, a vault write that
couldn't be made), the broker only logged it, and giving stayed on: the agent,
having just confirmed a hit, kept the rest of its hand-overs, each a further
guess.

**The defense.**

- When the write that turns giving off fails, the off is **owed**: held in
  memory, by the Library and the conversation, under the Library's lock, so
  no hand-over comes between. It's owed too when the Library can't even be
  opened to write it.
- **Before anything else the Library does**, in the one function every
  operation goes through, an owed off is written. Whether that write succeeds
  or not, the conversation is off: every hand-over is refused as `Off` before
  any file is read.
- Once written, the debt goes, so no later operation writes it again.
- An off owed to a conversation the owner has since left is let go: it never
  turns off a new conversation in which no secret was found.

**What isn't covered.** A broker restart before the owed off is ever written
loses it, since it's held in memory. The case needs a write that fails, then a
restart, then a broker that can write again. Recorded here, not hidden.

## #511

`CONIN$` and `CONOUT$` (the console) and `CLOCK$` are names Windows keeps for
devices, as `CON` and the others are. Whatever their case, and whatever
follows their first dot, they now refuse a folder at any depth of its paths.
A single file's name never kept a `$`: the broker's name for a file is made of
letters, digits, `.`, `_` and `-`. A test now holds that, so a file named
`CONIN$.txt` is kept as a name that is no device's.

## Tests

In the Library's tests, with the vault's fault switch failing the write
that turns giving off:

- **Owed, and written first:** the conversation is off while nothing is
  written; the next look writes it and refuses; nothing is owed after, so no
  later look writes it again.
- **Still unwritable:** three looks, each with the write failing again, each
  refuses as `Off`; a spend fails the same way; the next look writes it.
- **Let go:** another Library owes nothing; a new conversation starts clean;
  an off owed to a conversation since left neither turns the open one off nor
  stays owed; and one still unwritten when the owner starts a new
  conversation doesn't hold the new one off.
- **#511:** `CONIN$`, `conout$.txt` and `Clock$` refuse a folder at any depth
  ("device"); a file named `CONIN$`, `conin$.txt`, `CONOUT$`, `Conout$.log`,
  `CLOCK$` or `clock$.x` is kept by a name with no `$`, and no device's.

## Hand-breaks

8 breaks, each with the tests that must fail named. All 8 caught, the
Library's and hand-over's 96 tests green before and after.

| Break | Caught by |
|---|---|
| never owed | owed and written first; still unwritable |
| never settled | owed and written first; still unwritable |
| off only when written | still unwritable |
| owed to any conversation | let go |
| owed by any Library | let go (and 33 others) |
| `CONIN$` kept, `CONOUT$` kept, `CLOCK$` kept | the folder paths' rules |

**The first run, and what it found:**

- **A test gap.** "Owed to any conversation" survived. An off owed to a
  conversation since left only arises when the Library couldn't be opened,
  and no test made that case. A test-only way to owe it now does, and the
  break is caught.
- **A lint.** `clippy` on both toolchains objected to the latch's lock held
  across the vault write. The lock is now taken only to read and to clear.

## Mutation

`cargo mutants --in-place --in-diff` on the change to `library.rs` and
`hand_over.rs`, with the broker's library tests:

- **9 mutants:** 8 caught, 1 unviable, 0 missed, 0 timeouts.
- `outcomes.json` sha256 `2caf4b6c25c9abda0324df76f0b0a9c355a83a3c07406338830cdc6c597e723b`.

An earlier run missed one: the paid debt never let go. It showed in nothing
but a vault write on every later operation, so a test-only check that
nothing is owed after now holds it.

## The gate

On the tested tree, with each step's exit code read directly:

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --check` | 0 | 0 |
| `cargo clippy --workspace --all-targets -D warnings` | 0 | 0 |
| the same, `--all-features` | 0 | 0 |
| `cargo test --workspace --no-fail-fast` | 135 binaries, 4,212 passed, 0 failed | 135 binaries, 4,212 passed, 0 failed |
| `hatchling-dev seed-check`, `protocol-schema`, `audit` | 0 | 0 |
