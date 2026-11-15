# Evidence: the agent is told a program isn't done until it has been run

What was tested, how, and what came back, for a change to what the
agent is told about the programs it writes. Every number here is read
from a file in the results bundle, which is attached to the release
that carries this change.

| | |
|---|---|
| Base commit | `b704c631` (`fix(app): show ideas a chat's run suggested on the Ideas page (#460)`) |
| Tested tree | `ba932baa8a1fe744bb14a055ad914741e863c3fa` |
| Date | 2026-11-15 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-run-what-you-write-results.tar.gz`, 355786 bytes, sha256 `01000bc0e58c8e84c1d70b53502022e537fee217b718572f13cc922425dbcaf7` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

On an install of main `19ab6890`, a chat was sent "Write small
Rust/Go/Python binaries for routine editor tasks: a file-tree pruner,
duplicate finder, code-snippet extractor", once to each of two local
models.

- `qwen3.5:9b` wrote a pruner in Python, a duplicate finder in Go and
  the pruner again in Rust, and no snippet extractor. Its computer has
  no Go or Rust compiler. The build commands it gave don't exist. The
  broker's log has eight files written, two read, and two commands,
  `mkdir` and `rm`: no program it wrote was run.
- `gemma4:12b` wrote all three in Python and said where they were. The
  log has three files written and no command at all.

Both answered that the tools were ready. The `run` tool's description
tells the model to check the exit code before saying something worked;
nothing told it that a program it wrote has to be run at all.

## What changed

Words, in three places the model reads (`hatchling-agent`):

- **The note about its computer** (`capabilities.rs`), given only to an
  agent that has one: "A program you write or change there isn't done
  until you have run it and seen what it does: run it before you say it
  works, and if you couldn't run it, say so."
- **The `write` tool's description** (`tools.rs`): "A program you write
  isn't done until you have run it with run and read what it printed:
  write it in a language this computer runs (python3, node or sh; it
  has no compilers), run it, and only then say it works."
- **The `edit` tool's description**: "After you change a program, run it
  again before you say it works."

And the changelog.

## What this is, and isn't

These are words a model reads, and a model can ignore them. Nothing in
the loop or the broker holds back an answer about a program that was
never run. The change is the smallest that could be right; whether it
is enough is for an install to show, and the changelog says so.

## Tests

No new test; three widened, one assertion for each sentence.

- **The note** (`capabilities::tests`): an agent with a computer is told
  the sentence and to say so when it couldn't run; an agent with none
  isn't told it.
- **The tools** (`tools::tests`): the `write` description has the rule,
  the languages and "only then say it works"; the `edit` description
  has its sentence.
- **The loop** (`tests/loop.rs`): the model's first message, as the loop
  builds it, carries the note's sentence.

## Breaks by hand

Seven, each one sentence or part of one taken out or put where it
doesn't belong, the tests run on each, and each failing test named
(`unit-breaks.log`, `unit-*.log`, `rw_break.py`). All seven were
caught, and the tests passed before and after.

| Break | Caught by |
|---|---|
| The note doesn't say a program must be run | the note's test; the loop's test |
| The note doesn't say to say so when it couldn't be run | the note's test |
| An agent with no computer is told to run what it writes | the note's test |
| The `write` tool doesn't say a program must be run | the tools' description test |
| The `write` tool doesn't say which languages run here | the tools' description test |
| The `write` tool doesn't say when it may say it works | the tools' description test |
| The `edit` tool doesn't say to run a changed program | the tools' description test |

## Mutation

`cargo mutants --in-place --in-diff` on the change to the agent's
source (`mut.log`, `mut-agent-outcomes.json`, sha256
`4e18a125f27511d692d110c19ce13b0104d0c5f9ac490744fdfae83c152d033a`):

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-agent` | 6 | 6 | 0 | 0 | 0 |

The six replace the note and each of the two descriptions with nothing,
and with a word that isn't theirs.

## The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3915 passed, 0 failed | exit 0: 135 binaries, 3915 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

The tests of what the change touches were run as root too, as CI runs
them: 181 in the agent and 10 in its loop (`root/as-root.log`).

The whole chain was first run on tree `36deac48…`, before another
change was merged under this one, with the same results. Every run in
the bundle is of the tested tree.

## What wasn't tried

- **Any model against the new words.** No chat has been sent since they
  were written. Whether either model now runs what it writes, writes in
  a language its computer runs, or says so when it couldn't, isn't
  known.
- **What the words cost.** They are in every request an agent with a
  computer makes: about ninety words more. Nothing here measures
  what that does to a small model's other habits.
- **A wall.** Nothing checks that a program was run before an answer
  about it. If the words don't change what a model does, that is the
  next thing to weigh.
