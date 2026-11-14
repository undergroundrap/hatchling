# Evidence: a slow model is said to be slow

What was tested, how, and what came back, for a change to what the
broker says when a model's answer doesn't come in the time it waits.
Every number here is read from a file in the results bundle, which is
attached to the release that carries this change, unless it says
otherwise.

| | |
|---|---|
| Base commit | `2dc41b5a` (`docs(evidence): correct the goals store finding, record the fix on its install (#441)`) |
| Tested tree | `068dd22399b451b31923b5eaa7e78e334bc9befc` |
| Date | 2026-11-14 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-slow-model-said-results.tar.gz`, 1185980 bytes, sha256 `9b3b95c3964c15cfd8719b09a9dcc06f87fd74ddd569d50bacd021f0c7d6da4f` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

On a real install, a hosted model was asked about a file whose first
line is longer than a page. It read the page, and no reply came. After
334 seconds the broker logged "the egress authority can't be reached:
… Resource temporarily unavailable (os error 11)", and the chat said
"Hatchling couldn't get an answer from its model … Check that it is
running and reachable". Asked again some hours later, the same model
answered in three to four minutes, by writing some three thousand
characters of that line back. (The update drill's record and the `ls -l` fix's have
both runs.)

Neither message was so: the model was running, reached, and answering.
The egress authority waits 300 seconds on each read from a model's
server. The broker waits 330 for each of the authority's frames. A
hosted model's server sends bytes to keep the connection open while it
works, so each of the authority's reads was answered and it went on
waiting, with nothing yet to give the broker. The broker's wait ran out
first, and any frame it couldn't read was taken for an authority that
couldn't be reached.

## What changed

- **The link to the authority** tells a frame not read because the wait
  ran out from one not read for any other reason. The link is lost
  either way, as before: a late answer could be read as the next one's.
- **The refusal** of such a request has a kind of its own, `no-answer`.
  The operator's verdict on it is the same as for any failure.
- **For a model call**, the broker remembers the cell (the most recent
  64), and forgets it when the run's failure is worded.
- **The chat** then says the model was still working on its answer after
  more than five minutes, that a very long answer takes this long too,
  and, of a model whose name ends in `:free`, that free hosted models
  are slow when many people are using them. A usage limit that is
  reached is still told first.

The agent is told what it was told before, that its request failed, and
nothing of why. Nothing about the waits themselves changed.

## Tests

Five, in the broker.

- `egress_authority::tests::an_answer_that_does_not_come_in_time_is_said_as_that`:
  a stand-in authority takes a request and answers nothing; the link
  says no answer came, is lost, and sends nothing more. One that says an
  answer is coming, whose body doesn't, is said the same. One that is
  gone is still one that can't be reached.
- `serve::gate_scope_tests::a_model_call_with_no_answer_in_time_is_remembered_for_its_owner`:
  the cell is remembered for a model call and not for another request,
  and the agent is told a failure both times.
- `machine::tests::a_quiet_model_is_remembered_until_it_is_told`: told
  once, each cell its own, and the oldest dropped past the bound.
- `chat_run::tests::a_model_still_answering_is_said_to_be_slow_and_not_missing`:
  the words, the free model's by its name's end alone, the limit first,
  other endings as always, and "more than five minutes" held to the
  broker's own wait.
- `chat_run::tests::a_chat_run_whose_model_was_still_answering_is_told_so_once`:
  a chat run that fails in a remembered cell is told so; one in another
  cell, and the next in the same, aren't.

## Breaks by hand

Thirteen, each one part of the change taken out or bent, the tests of
what it touches run on each, and each failure read (`unit-breaks.log`,
`unit-*.log`). All thirteen were caught, and the 89 tests passed before
and after.

| Break | Caught by |
|---|---|
| A wait that ran out is taken for an authority that can't be reached | the link's test: "unavailable" where "no answer" was expected |
| A link whose answer didn't come goes on being used | the link's test: "a request was sent after it was lost" |
| A body that stops coming is taken for an authority that can't be reached | the link's test |
| No answer in time isn't remembered against the cell | the request path's test |
| Any request no answer came for is remembered | the request path's test: "a listing isn't a model call" |
| The agent is told it was denied | the request path's test: `Denied` for `EgressError` |
| A quiet model is told as one that can't be reached | both of the chat's tests |
| A quiet model is told of whatever the run's ending | the words' test: the slow words where "used all its steps" was expected |
| A quiet model is told before a limit that is reached | the words' test: the slow words where the limit's were expected |
| Any model with ":free" in its name is said to be a free one | the words' test |
| The chat doesn't ask what the broker knows of the cell | the chat run's test |
| Asking doesn't forget | the record's test, and the chat run's: "told once" |
| No bound on what is remembered | the record's test: "the oldest was dropped" |

The runs were made three times. The first two stopped at a break the
runner counted as not caught when it was: once because the break list
named a test's module wrongly, and once because the test runner's line
for the failed test came out mixed with another thread's words. The
runner now takes the test's own panic line too. The third run is the one
in the bundle.

## Mutation

`cargo mutants --in-place --in-diff` on the change to the broker's
source, the broker's library tests on each (`mut.log`,
`mut-broker-outcomes.json`, sha256
`6561bd176ce24dc40318332e27feb915cc3251640b14e79071a63ad0d1c74120`):

| Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|
| 39 | 21 | 0 | 0 | 18 |

The unviable ones:

- `chat_run.rs:410:9: replace Chat<R, K>::step -> Step with Default::default()`: unviable, build failure
- `egress_authority.rs:558:9: replace EgressLink::exchange -> Result<T, Refusal> with Ok(Default::default())`: unviable, build failure
- `egress_authority.rs:660:5: replace receive -> Result<Reply, Error> with Ok(Default::default())`: unviable, build failure
- `egress_authority.rs:696:5: replace unread -> Error with Default::default()`: unviable, build failure
- `egress_path.rs:81:9: replace Refusal::no_answer -> Self with Default::default()`: unviable, build failure
- `machine.rs:168:5: replace quiet_models_now -> MutexGuard<'static, VecDeque<String>> with MutexGuard::new()`: unviable, build failure
- `machine.rs:168:5: replace quiet_models_now -> … with MutexGuard::from_iter([VecDeque::new()])`: unviable, build failure
- `machine.rs:168:5: replace quiet_models_now -> … with MutexGuard::new(VecDeque::new())`: unviable, build failure
- `machine.rs:168:5: replace quiet_models_now -> … with MutexGuard::from(VecDeque::new())`: unviable, build failure
- `machine.rs:168:5: replace quiet_models_now -> … with MutexGuard::from_iter([VecDeque::from_iter([String::new()])])`: unviable, build failure
- `machine.rs:168:5: replace quiet_models_now -> … with MutexGuard::new(VecDeque::from_iter([String::new()]))`: unviable, build failure
- `machine.rs:168:5: replace quiet_models_now -> … with MutexGuard::from(VecDeque::from_iter([String::new()]))`: unviable, build failure
- `machine.rs:168:5: replace quiet_models_now -> … with MutexGuard::from_iter([VecDeque::from_iter(["xyzzy".into()])])`: unviable, build failure
- `machine.rs:168:5: replace quiet_models_now -> … with MutexGuard::new(VecDeque::from_iter(["xyzzy".into()]))`: unviable, build failure
- `machine.rs:168:5: replace quiet_models_now -> … with MutexGuard::from(VecDeque::from_iter(["xyzzy".into()]))`: unviable, build failure
- `serve.rs:1372:5: replace handle_egress_op -> Result<EgressResult, Error> with Ok(Default::default())`: unviable, build failure
- `serve.rs:1468:5: replace refused_reply -> EgressResult with Default::default()`: unviable, build failure
- `serve.rs:1537:5: replace egress_verdict -> EgressVerdict with Default::default()`: unviable, build failure

Mutation doesn't reach inside the `matches!` that tells a wait run out
from any other failure: the first break by hand is what holds it.

## The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3809 passed, 0 failed | exit 0: 135 binaries, 3809 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

The tests of what the change touches were run as root too, as CI runs
them: 89 passed (`root/as-root.log`).

## What wasn't tried

- A real model that takes more than five and a half minutes: the tests
  stand in for the authority with a short wait. The install the fault
  was found on hasn't been updated to this change.
- A scheduled run. Its notice still says the old words: it doesn't carry
  which run it was, so the broker's record of the cell isn't asked.
- A model on this machine that is slow. The authority's own wait on a
  server that sends nothing is shorter than the broker's, and ends
  first, as a failure: that path is as it was.
