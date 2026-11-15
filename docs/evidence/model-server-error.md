# Evidence: a model server's error is asked again

What was tested, how, and what came back, for a fix to what the agent
does when its model's server answers with an error. Every number here
is read from a file in the results bundle, which is attached to the
release that carries this change, unless it says otherwise.

| | |
|---|---|
| Base commit | `d2556e6d` (`fix(ideas): the goals tool shows each goal's id (#467)`) |
| Tested tree | `f8a2a352c970ee9636b377eb53a35376c5c55dad` |
| Date | 2026-11-15 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-model-server-error-results.tar.gz`, 216235 bytes, sha256 `41fff72b411202d65f30a27589d95906aa770309d5898154bb1e727b859395cf` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it. No file of
the app is changed, so the app's own check was not run.

## What was wrong

It was found on an install, with a local model. The owner started an
idea, and the run wrote a file and asked its model again. The model
wrote a tool call that was cut short. Its server could not read it,
logged that the tool call failed to parse, and answered HTTP 500. The
agent took that one answer as the end of the run, and the owner was
told: "Hatchling couldn't get an answer from its model. Check that it
is running and reachable." The model was running, and answered the
owner's next message.

An empty reply, or one that was all thinking, was already asked again,
up to three times. A server's error was not.

## What changed

`crates/hatchling-model/src/provider.rs`:

- `server_failed` says whether a status is a server's failure: 500 to
  599. A status that refuses the request as sent (4xx) is not.
- In a turn, a server's failure is asked again, among the same tries
  (`ATTEMPTS`, three) a stray reply gets. Nothing runs before a reply
  is used, so asking again is safe.
- The capability probe is asked again the same way. A server that
  fails it on the last try is the transport's failure.

And the changelog.

## What it is, and isn't

- **Never more than three requests** for one turn, strays and failures
  together.
- **A refusal isn't asked again.** 400, 401, 404 and 429 end the run
  as before: the same request would be refused again.
- **The owner's words when it fails every time are unchanged.** After
  three failures the chat still says to check the model is running and
  reachable. Saying what happened needs a failure of its own through
  the agent and the broker, and is not in this change.

## Tests

Two new, one widened (`provider::tests`).

- **A turn**: 500, 503 and 599 are each asked again and then answered;
  two failures and then an answer; three failures are that failure,
  after three requests and no more; the tries are shared with stray
  replies, in either order; and 400, 401, 404, 429, 499 and 600 are
  the failure at once, after one request.
- **The probe**: asked again after one failure and after two; three
  failures are the transport's failure, with that status; a stray
  after a failure is still a stray, and the model is refused for what
  it said; a refusal ends it after one request.
- **Widened**: the older test of a status that comes a turn after
  thinking was turned off uses a refusal (404) where it used 500,
  which is asked again now.

## Breaks by hand

Twelve, each run against the model crate's tests (`unit-breaks.log`,
`unit-*.log`, `se_break.py`). All twelve were caught, and the tests
passed before and after.

| Break | Caught by |
|---|---|
| A 500 isn't a server's failure | the turn's test; the probe's test |
| A 599 isn't a server's failure | the turn's test |
| A refusal is asked again | the turn's test; the probe's test; two older tests of thinking turned off |
| A 600 is a server's failure | the turn's test |
| No status is asked again | the turn's test; the probe's test |
| A server's failure is asked again without end | the turn's test |
| A server's failure isn't counted among the tries | the turn's test |
| A server's failure ends the run at once | the turn's test |
| A server's failure is asked again only once | the turn's test |
| The probe isn't asked again | the probe's test |
| The probe is asked again only once | the probe's test |
| A server that always fails the probe is a model that calls no tools | the probe's test |

## Mutation

Every mutant cargo-mutants makes on the lines changed against the base
(`mut.log`, `mut-model-*`). None was missed and none timed out.

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-model` | 18 | 17 | 0 | 0 | 1 |

Command, from `mut.log`: `cargo mutants --in-place --in-diff
se-slice-model.diff --timeout 120 -o <out> -- --lib`.

Unviable:

- `crates/hatchling-model/src/provider.rs:282:9: replace ChatCompletionsProvider<T>::next_action_inner -> Result<ModelAction, Error> with Ok(Default::default())`: unviable, build failure.

No equivalence is claimed: there was no missed mutant to explain.

## The gate

On each toolchain, from a cleared build cache (`gate.log`,
`gate-1.*.log`): `cargo fmt --all -- --check`, `cargo clippy
--workspace --all-targets --locked -- -D warnings`, the same with
`--all-features`, `cargo build --workspace --locked`, `cargo test
--workspace --locked --no-fail-fast`, `hatchling-dev seed-check` and
`hatchling-dev protocol-schema`.

| | 1.85.0 | 1.98.1 |
|---|---|---|
| Every step | exit 0 | exit 0 |
| Test binaries with a result | 135 | 135 |
| Tests passed | 3948 | 3948 |
| Tests failed | 0 | 0 |
| Lines saying skipped | 28 | 28 |

As root, outside the sandbox's jail (`root/as-root.log`): 46 tests in
the model crate, and in the agent 10 in its local-model test and 4 in
its model test. All passed.

## Found on the way

- **Run twice.** The runs were first made on tree `77fec13c…`, on the
  base before `d2556e6d`, and all passed. They were made again, whole,
  on the tree above once that change was the base, so the tested tree
  is the one merged.

## What wasn't tried

- **An install.** This fix has not run on one. The failure it answers
  came from a model's own odd reply, which can't be made to happen on
  demand: it will be seen there only when it next occurs.
- **A real server.** The tests answer with scripted statuses. No test
  makes a model server fail to parse a tool call.
- **A pause between tries.** There is none. A hosted server that is
  failing is asked three times at once.
