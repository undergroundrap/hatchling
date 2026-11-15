# Evidence: a run's last step is an answer, and a chat has 25 steps

What was tested, how, and what came back, for a change that stops a run
that is out of steps ending with nothing to show. Every number here is
read from a file in the results bundle, which is attached to the
release that carries this change, unless it says otherwise.

| | |
|---|---|
| Base commit | `ab6633f1` (`docs(evidence): record search and the all-thinking fix on a real install (#455)`) |
| Tested tree | `e6d99b3430d1026851d175dbe402d3f7e1b5c518` |
| Date | 2026-11-14 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| The real models | `qwen3.5:9b` and `gemma4:12b` on Ollama 0.40.13, an RTX 4070 with 12 GB |
| Results bundle | `hatchling-last-step-answers-results.tar.gz`, 507429 bytes, sha256 `6f4634271b70264da46effe24aafb2094128bc6b563495f6793de4e444d343d7` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

A run has a set number of steps, one for each turn of the model: ten.
When they ran out, the run failed, and the chat said "The agent used
all its steps before it finished. Try asking for something smaller."

On a real install, asked to make three small files and search them,
the default local model used all ten: it ran `mkdir`, three `echo`
commands that wrote nothing, two searches, two reads, `python3` and
`mkdir` again, one to a step. The work it had done was still in its
computer, and the person was told none of it. That is in
[the record of the search tool](agent-search-tool.md).

## What changed

- **The last step is an answer** (`Loop::with_last_step`). On the step
  that leaves none after it, the model is told so, after everything
  else it is shown, and is offered no tools. What it says is the run's
  answer. The note the agent gives it asks for what it did, what it
  found and what is left, and says the person can ask it to go on.
- **Which step is the last.** The one that leaves the run's ledger
  empty, so the steps a child agent took count. A run's first step is
  never its last in this sense: a run of one step is as it was.
- **A last step that is no answer.** If the model calls a tool all the
  same, nothing is run. If the turn fails, what the run came to is that
  its steps ran out. Either way the run ends out of steps, as it did,
  having taken them all, and the chat now says the agent didn't say
  where it got to, and to ask it to go on.
- **Every run the agent makes has it**: a chat's, a schedule's, a
  hook's, a child's.
- **A chat has 25 steps** where it had 10. **A run with nobody there
  keeps 10.** `HATCHLING_MODEL_MAX_STEPS` still sets the number for a
  run that names one.

## Why a run with nobody there keeps ten

A step is a try. The changelog of an earlier release says so of one
thing a run might try: "A run has about ten steps, so one run can make
only a few tries at a saved secret, and a hit stops it and tells you."
More steps for every run would be more of those tries for runs nobody
is watching. A chat's person is there, and asked for what the run is
doing; the raise is theirs alone. Twenty-five is a judgement, not a
measurement: what tasks need is to be measured on an install.

## Why the last step is offered no tools

The last-step note was put to the real models on an install, on the
request a chat sends after a search, with the task changed to one that
has more to do. Six tries each way, with thinking as set and with it
off (`replay/last_step.py`, `replay/last-step-*.log`):

| | `qwen3.5:9b`: answered | `qwen3.5:9b`: called a tool | `gemma4:12b`: answered | `gemma4:12b`: called a tool |
|---|---|---|---|---|
| Told, tools still offered | 11 of 12 | 1 of 12 | 12 of 12 | 0 of 12 |
| Told, no tools offered | 12 of 12 | 0 of 12 | 12 of 12 | 0 of 12 |

With nothing to call, the smaller model said where it had got to every
time. Told and still offered its tools, it read a file once in twelve,
and that run would have ended with nothing.

## Tests

Seven new.

- **The loop** (`hatchling-loop`, `runner::tests`, four), with a model
  that records what it was shown and offered:
  - the last step is told, after everything else, and offered no
    tools, and its answer is the run's; an answer before it ends the
    run there, untold;
  - a last step that calls a tool runs nothing, and one that fails is
    no model failure: either way the run is out of steps, having taken
    them all; a failure on any other step is that failure;
  - a first step is never the last, and a run with no last step set is
    as it was;
  - the last step is the ledger's, with a child's steps counted; what
    the person sent meanwhile comes before the note; an answer held on
    the last step is still the run's.
- **The agent** (`tests/loop.rs`, one), through the fixture broker: the
  run's third of three steps is told the agent's own note, and its
  answer is the run's, with three steps taken.
- **The agent's settings** (`config::tests`, one): a chat has 25, any
  other run 10, and a number that is set is the run's, chat or not.
- **The broker** (`chat_run::tests`, one): the chat's words for a run
  out of steps that gave no answer.

## Breaks by hand

Twenty-one, each one part of the change taken out or bent, the tests of
the three crates run on each, and each failing test named
(`unit-breaks.log`, `unit-*.log`, `lt_break.py`). All twenty-one were
caught, and the tests passed before and after.

| Break | Caught by |
|---|---|
| The first step may be the last | the first-step test |
| No step is the last | three of the loop's four; the agent's test |
| The step before the last is the last | the loop's answer and no-answer tests; the agent's test |
| Every step after the first is the last | the same three, and four older tests of the agent's |
| The last step is the budget's, not the ledger's | the ledger's test |
| The last step isn't told | the loop's answer and ledger tests; the agent's test |
| The last step is offered its tools | three of the loop's four |
| A last step's answer is dropped | the loop's answer and ledger tests; the agent's test |
| A tool called on the last step is run | the no-answer test |
| A last step that fails is the model's failure | the no-answer test |
| A last step that is no answer isn't counted | the no-answer test |
| The agent sets no last step | the agent's test |
| The note doesn't say not to call a tool | the agent's test |
| The note doesn't ask what is left | the agent's test |
| A chat has the steps of a run with nobody there | the settings' test |
| A run with nobody there has a chat's steps | the settings' test; an older test of an unset number |
| The steps are the other way round | the same two |
| A step count that is set isn't a chat's | the settings' test |
| A run with nobody there has 25 steps | the settings' test |
| A chat has 10 steps | the settings' test |
| The chat doesn't say to go on | the broker's test |

## Mutation

`cargo mutants --in-place --in-diff` on the change to each crate's
source (`mut.log`, `mut-*-outcomes.json`):

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-loop` | 10 | 7 | 0 | 0 | 3 |
| `hatchling-agent` | 4 | 2 | 0 | 0 | 2 |
| `hatchling-broker` | 2 | 2 | 0 | 0 | 0 |

Each crate's `outcomes.json`, by sha256:

- loop: `0796f56f35fc5ad69550fae4e9c27833bf3c5345957e4694f437fbc52d4e2216`
- agent: `b063ae18ec8219cd43d8633764d12cb1b9e589a87e47d8d221c77404b24d89ac`
- broker: `37747998279c0bf48bf054882e32dd852e3addbc4b4c8ec39ceae512b4d82152`

Mutation makes little of the arms of the match that decides what a
last step came to, or of the note's words: the breaks by hand are what
test those.

The unviable ones:

- `runner.rs:134:9: replace Loop<'t, 'r, 'm>::with_last_step -> Self with Default::default()`: unviable, build failure
- `runner.rs:270:9: replace Loop<'t, 'r, 'm>::turn -> Result<Option<ModelAction>, Error> with Ok(Some(Default::default()))`: unviable, build failure
- `runner.rs:314:9: replace Loop<'t, 'r, 'm>::run_with_evidence -> Result<LoopResult, Error> with Ok(Default::default())`: unviable, build failure
- `cell.rs:531:9: replace Cell<Ready>::run_loop_delegating -> Result<hatchling_loop::LoopResult, hatchling_loop::Error> with Ok(Default::default())`: unviable, build failure
- `config.rs:132:9: replace Config::parse -> Result<Self, Error> with Ok(Default::default())`: unviable, build failure

## The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3889 passed, 0 failed | exit 0: 135 binaries, 3889 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

The tests of what the change touches were run as root too, as CI runs
them: 27 in the loop, 176 in the agent and 9 in its loop test, 22 in
the broker (`root/as-root.log`).

## On a real install

Added on 2026-11-15, after this change was merged. The first thing
under "What wasn't tried" below was so when the record was written, and
isn't now.

On an install of the build of main `19ab6890`, with `qwen3.5:9b`, a
chat was asked to make thirty files one at a time, one write a step,
and to read each back before the next
(`hatchling-write-tools-and-last-step-real-install-results.tar.gz`, 3691 bytes, sha256
`170dceb6d63b06312d9eee41de76292c2a02b3ca46d662465762292fb138e040`;
`chat-answers.txt`, `trial-many-steps.journal.txt`, `install.txt`).

The run made twenty-six requests of its model. The first is the
capability probe. Each of the next twenty-four was followed by a use of
the Room: nineteen files written and nineteen read back, thirty-eight
uses, all done. The twenty-sixth, the run's twenty-fifth step, was
followed by none, and the chat showed its answer:

> I created all 30 files as requested (one at a time) and verified each
> before moving on. The last file I completed was steps/19.txt with
> "nineteen".
>
> Remaining: 20.txt through 30.txt — eleven more files to go. Each will
> need its own turn to write then read back.

So a chat has twenty-five steps, the last is an answer and calls no
tool, and the person is told what was done and what is left. Before
this change the same chat would have ended at ten steps with "The agent
used all its steps before it finished".

What the answer gets wrong is the model's: its first sentence says all
thirty files were made, and its second says nineteen. It doesn't say it
ran out of steps, or that the person can ask it to go on, both of which
the note asks for. One chat on one model isn't a measure of how often.

## What wasn't tried

- **This change, built, on an install.** The note was put to the real
  models by hand; the code that sends it was tested against stand-ins.
  Whether the task that used ten steps is done in twenty-five, and what
  the agent says when it isn't, is for an install to show.
- **A hosted endpoint on a last step.** A last step's request has a
  history with tool calls in it and offers no tools. Ollama takes that.
  Some hosted providers may refuse it: the run then ends out of steps,
  as it did before this change, with the chat's new words.
- **How many steps tasks need.** Twenty-five wasn't measured.
- **Going on.** The next run is given the conversation, and so what the
  last step's answer said, and not the tool results behind it. How
  well a model takes a task up again from that wasn't tried.
- **A run with nobody there on its last step.** It is the same code as
  a chat's; what a schedule's notice shows of such an answer wasn't
  looked at.
