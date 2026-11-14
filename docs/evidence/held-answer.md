# Evidence: an answer waits for the programs the agent started

What was tested, how, and what came back, for a change to when a run's
answer is taken as its answer. Every number here is read from a file in
the results bundle, which is attached to the release that carries this
change, unless it says otherwise.

| | |
|---|---|
| Base commit | `6922f56b` (`docs(evidence): record the chat's working line in the real app (#445)`) |
| Tested tree | `8c71a6b835d1f2ae5b0702233a66e0a4cee39e52` |
| Date | 2026-11-14 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-held-answer-results.tar.gz`, 401731 bytes, sha256 `96db398679effcb10637676ff98ed94dfc13a39c11ac5fc9cc6cbb35db1391ad` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

On a real install, with a model on that machine, the agent was asked to
run `sleep 12; ls -la /workspace` in its computer and say what it
printed. A program still going after ten seconds comes back to the agent
as running, with a name to ask after it by. The agent answered: "The
command is still running. I will check the status in a few seconds."
(The record of the chat's working line has the run.)

Nothing checks later. A reply that has ended is over, and a program
still running then is stopped with it. The owner was never told what the
program printed, and was told something would happen that couldn't.

## What changed

- **The loop** can be given a question to ask each time its model
  answers: what the run started and hasn't settled. An answer given with
  something left isn't the run's answer yet. The model is told what is
  left, in a note placed after its own words, and asked again.
- **The note** is a message kind of its own. It goes to the model as a
  user-role message, as recalled memory and gathered sources do, and
  never as the system prompt.
- **Two limits.** An answer is held twice at most; the third is taken as
  it is, and what is left isn't asked after again. And if the run's steps
  run out while an answer is held, that answer, the last one held, is the
  run's. Holding an answer never turns a run that answered into one that
  failed.
- **The agent** keeps the programs a run has going, from the Room's own
  replies to `run`, `run_check` and `run_stop`: running when a reply says
  so, or says one may have started all the same; ended when a reply says
  it ended, or that the run ran none by that name; and all of them ended
  when the Room closed. Every run's loop asks it: a chat run, a scheduled
  run and a subagent alike.
- **A program still running** is told to the model, there and then, with
  what to do before it answers: wait for it or stop it, because it is
  stopped when the model answers and nothing looks at it afterwards.

The model's words in a held answer stay in the run's history, so the
model sees what it said and why that wasn't taken. Nothing is added to
what the owner sees: the answer shown is one the model gave.

## Tests

Twelve.

In the loop (`hatchling-loop`):

- `runner::tests::an_answer_given_with_something_left_is_held_and_the_model_told`:
  the first answer is held, the model's next turn ends with its own words
  and then the note, and the second answer is the run's.
- `runner::tests::an_answer_with_nothing_left_is_taken_at_once`: asked
  once, taken in one step, no note.
- `runner::tests::an_answer_is_held_no_more_often_than_the_limit`: with
  something always left, the third answer is the run's, the question was
  asked twice, and the two notes are in the history in order.
- `runner::tests::a_held_answer_is_the_runs_when_the_steps_run_out`: the
  held answer is the run's; of two held, the second; and with none held
  the run fails as it always did, and the question was never asked.
- `runner::tests::debug_shows_whether_the_loop_asks_what_is_left`.

In the provider (`hatchling-model`):

- `provider::tests::recalled_memory_and_evidence_travel_as_user_role_data`,
  extended: a held answer and its note go as an assistant message and
  then a user one.

In the agent (`hatchling-agent`):

- `room::tests::a_program_is_running_until_its_end_is_seen`: the words
  the model is told, whole, and none once the program has ended.
- `room::tests::each_running_program_is_named`: two are named in order;
  one ending leaves the other; a name the run never ran is forgotten.
- `room::tests::what_may_have_started_counts_and_a_closed_room_runs_nothing`:
  a program that may have started counts; a reply about no program
  changes nothing; the Room closing ends them all.
- `tools::tests::the_run_tools_note_what_is_running`: each of the three
  tools, against a stand-in broker; a session that fails leaves the
  program as it was.
- `tools::tests::a_running_program_is_told_with_what_to_do_before_answering`.
- `cell::tests::a_runs_answer_waits_for_the_programs_it_started`: a
  cell's own loop, against a stand-in broker. The model starts a program,
  answers that it is still running, is told, asks after it, and its next
  answer is the run's. One note, four steps, two Room ops.

Three older tests match on every kind of message, and name the new one.

## Breaks by hand

Eighteen, each one part of the change taken out or bent, the library
tests of the three crates run on each, and each failure read
(`unit-breaks.log`, `unit-*.log`, `ah_break.py`). All eighteen were
caught, and the 226 tests passed before and after.

| Break | Caught by |
|---|---|
| What is left is never asked | the loop's three tests of holding, and the cell's: the first answer was the run's |
| The answer is held and the model isn't told why | the loop's test of holding and of the limit, and the cell's: no note in the history |
| An answer is held as often as something is left | the limit's test: the model was asked a fifth time |
| The holds aren't counted | the limit's test, the same way |
| A held answer is forgotten | the steps' test: the run failed with its steps used |
| The first answer held is kept, not the last | the steps' test: "first" where "second" was expected |
| The loop's debug form doesn't say whether it asks | the debug test |
| The note goes to the model as the system's word | the provider's test |
| A program that ended is still counted | four tests: the Room's two, the tools', the cell's |
| A name the run never ran stays counted | the Room's test of names |
| A program that may have started isn't counted | the Room's third test |
| The Room closing ends only the program it is told with | the Room's third test: "the Room took j-3 with it" |
| Many programs are told as one | the Room's test of names |
| `run` notes nothing | the tools' test ("run started it"), and the cell's |
| `run_check` notes nothing | the tools' test ("run_check saw it end"), and the cell's |
| `run_stop` notes nothing | the tools' test ("run_stop saw it end") |
| A running program is told without what to do | the words' test |
| The cell's loop doesn't ask | the cell's test: the first answer was the run's |

## Mutation

`cargo mutants --in-place --in-diff` on the change to each crate's
source (`mut.log`; `mut-loop-outcomes.json`, sha256
`d8bdeedc36f4c0b51b4c7562be6c8bdc552bb9bcd496ef62af30d8cf0ecf7730`;
`mut-model-outcomes.json`, sha256
`0d996b91d431bb3bf4cfc672b7ff78dec24767e452871d52b330512ce80600cf`;
`mut-agent-outcomes.json`, sha256
`12a5b32737c448fbb0e0658b305c587391fb9c8cb6c35a1b8746c81df4efa542`).
The loop's and the provider's mutants were run against all of their
crate's tests, the agent's against its library tests.

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-loop` | 10 | 7 | 0 | 0 | 3 |
| `hatchling-model` | 2 | 2 | 0 | 0 | 0 |
| `hatchling-agent` | 13 | 7 | 0 | 0 | 6 |

The unviable ones:

- `runner.rs:123:9: replace Loop<'t, 'r, 'm>::with_unfinished -> Self with Default::default()`: unviable, build failure
- `runner.rs:219:9: replace Loop<'t, 'r, 'm>::opening -> Vec<Message> with vec![Default::default()]`: unviable, build failure
- `runner.rs:246:9: replace Loop<'t, 'r, 'm>::run_with_evidence -> Result<LoopResult, Error> with Ok(Default::default())`: unviable, build failure
- `cell.rs:525:9: replace Cell<Ready>::run_loop_delegating -> Result<hatchling_loop::LoopResult, hatchling_loop::Error> with Ok(Default::default())`: unviable, build failure
- `tools.rs:1134:9: replace <impl Tool for RunTool<'_>>::execute -> Result<ToolOutput, LoopError> with Ok(Default::default())`: unviable, build failure
- `tools.rs:1204:9: replace <impl Tool for RunCheckTool<'_>>::execute -> Result<ToolOutput, LoopError> with Ok(Default::default())`: unviable, build failure
- `tools.rs:1250:9: replace <impl Tool for RunStopTool<'_>>::execute -> Result<ToolOutput, LoopError> with Ok(Default::default())`: unviable, build failure
- `tools.rs:1394:5: replace job_answer -> ToolOutput with Default::default()`: unviable, build failure
- `tools.rs:1415:5: replace room_output -> ToolOutput with Default::default()`: unviable, build failure

Mutation makes nothing of three parts of the change, and the breaks by
hand are what hold them: the note being added and the held answer being
kept (statements, not operators); the three tools noting what they hear
(the one mutant of that function doesn't build); and the cell's loop
being given the question (a line of a builder chain).

## The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3826 passed, 0 failed | exit 0: 135 binaries, 3826 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

The library tests of the three crates were run as root too, as CI runs
them: 226 passed (`root/as-root.log`).

`quick.log` is of the tree before the changelog's line was added
(`29ffed12…`); everything else in the bundle is of the tested tree.

## On a real install

Added on 2026-11-14, after the change was merged (`c097c6e6`). The file
is in a second bundle,
`hatchling-held-answer-real-install-results.tar.gz`, 1422 bytes, sha256
`6f9cadb46540b5e7aec718c6dbfddb8783c6dae4cb839bc1e33ef980b013ef85`.

The install the fault was found on, under WSL2, was updated by hand to
the merged tree and unlocked by its owner. Only the agent's program
differed from what it had. The model was the same one, gemma4:12b on
that machine, and the task was typed in the same words: run
`sleep 12; ls -la /workspace` and say what it printed.

| | Before | After |
|---|---|---|
| The answer | "The command is still running. I will check the status in a few seconds." | "The command printed the following:" and the listing, three lines of it |
| Calls to the model | not read | 4 |
| Uses of its computer | 1 | 2 |

Four calls and two uses are what the loop does when it holds an answer
once: a call that started the program, a call that answered with it
still running, a call that asked after it, and a call that answered. A
model that waited unprompted would have made three. So the answer was
held, the model was told, and it did what it was told. The counts are
from the run's summary in the broker's activity log; the chat's working
line went from "Using its computer" at 28 seconds to "Asking the model"
and to the answer at 33.

It is one run, of one model.

## What wasn't tried

- **Another model**, or the same one more than once. Whether a model
  waits once told is the model's to do.
- **A model that answers a third time with the program still running.**
  Its answer is shown as it is, with whatever it promises. Nothing tells
  the owner the program was stopped.
- **A held answer shown because the steps ran out.** It is the answer
  the model gave before it was told, and says nothing of what the run
  did after.
- **A browser, a subagent or a schedule left going.** Only programs in
  the agent's computer are asked after.
