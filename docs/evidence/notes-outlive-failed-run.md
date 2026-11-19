# Evidence: a file's note to the model outlives a run that failed (#498)

What was tested, how, and what came back, for giving the broker's notes back
when the run they were given to ends without an answer.

| | |
|---|---|
| Base commit | `d28093ac` (`build(release): build the Room's supervisor from the image's own snapshot (#538)`) |
| Tested tree | `752d2006` |
| Date | 2026-11-19 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox, as an unprivileged user |
| Toolchains | Rust 1.98.1 and 1.85.0 (rustup) |

This record was added after the runs, in its own commit. It changes no code.

## What was wrong

Found in WO-041's acceptance run (`docs/evidence/files-acceptance.md`). The
model is told of a file the owner gives it by a line of the broker's, in the next
run's task alone. The chat took those lines as a run started, and gave them back
only when the run couldn't start. A run that started and then failed (the first
after a local model's server started, whose handshake ran while the model was
still loading) took the lines with it. The next run wasn't told of the file, and
the model looked for it by the owner's name for it.

## What changed

- **The chat** keeps the notes the run in flight was given, and gives them back
  when that run ends without an answer. A run that answered keeps them, as before.
  The agent's exit status can't say whether a failed run reached the model (a
  failed handshake and a model gone mid-run are both `ModelUnavailable`), so any
  failed run gives them back. A note told twice tells of a file still in the inbox.
  A note lost sends the model looking in the wrong place.
- **The hub** remembers the keys of the notes it last gave out. It gives back only
  those, and of them none forgotten since. A file the owner removes from the inbox
  while the run is working isn't told of afterwards.
- `docs/local-api.md` and the changelog say so.
- The other half of #498, the handshake failing while a local model loads, is a
  different fix, in the agent: #539.

## Results

| Check | Result |
|---|---|
| `chat_run`, `hub` and `file_in` tests | 61 and 16 passed, then 17 with the hub's new test, on 1.98.1 and on 1.85.0 |
| Hand-breaks | 5 broken, 5 caught, each by the tests named for it (below) |
| Targeted mutation, `hatchling-channel` | 9 mutants: 9 caught |
| Targeted mutation, `hatchling-broker` | 2 mutants: 2 unviable (`step_noted` and `start` returning a `Default` that `Step` has none of): the broker's lines are held by the hand-breaks |
| `cargo fmt --check` | 0 |
| clippy `-D warnings`, default and all features | 0 on 1.85.0 and on 1.98.1 |
| `cargo test --workspace`, `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` | 135 binaries, 4,214 passed, 0 failed, 0 ignored, on each toolchain |
| `seed-check`, `protocol-schema`, `audit` | 0 on each toolchain; "all checks passed" |

### Hand-breaks

| Break | Caught by |
|---|---|
| A failed run's notes dropped, as before | `a_run_that_fails_gives_its_notes_to_the_next` |
| A run that answered gives its notes back too | that, and `a_run_is_given_the_brokers_notes_and_the_owners_words_stay_theirs` |
| A note forgotten while taken comes back | that, and `only_notes_taken_and_not_forgotten_are_given_back` |
| Anything given back comes back, taken or not | `only_notes_taken_and_not_forgotten_are_given_back` (and the chat's) |
| What was taken not remembered: nothing comes back | both of those, `notes_are_kept_by_key_taken_once_and_bounded`, and the chat's first notes test |

## Residual

- The notes are held in memory, as before: a restart loses them, and the files
  stay in the inbox to be listed.
- A failed run that had reached the model gives its notes to the next run as well,
  which is told of the same files again.
