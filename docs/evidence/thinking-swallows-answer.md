# Evidence: a reply that is all thinking is asked again with thinking off

What was tested, how, and what came back, for a change that stops a
thinking model's answer being lost when it lands in the model's
thinking. Every number here is read from a file in the results bundle,
which is attached to the release that carries this change, unless it
says otherwise.

| | |
|---|---|
| Base commit | `c301605d` (`feat(agent): search the files in the agent's computer (#453)`) |
| Tested tree | `e74d67b7d314534b3a79e5e0dd6d929f319c2996` |
| Date | 2026-11-14 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| The real models | `qwen3.5:9b` and `gemma4:12b` on Ollama 0.40.13, an RTX 4070 with 12 GB, on an install of main `c301605d` |
| Results bundle | `hatchling-thinking-swallows-answer-results.tar.gz`, 660331 bytes, sha256 `ebc91b0c2f947bd0df222bb6c683a763c5e8500713240e7528af85eb8480cfe4` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

The first chats that used the new `search` tool on a real install, with
the default local model, ended three times running in "The model
(qwen3.5:9b) answered with nothing, no text and no tool call, each time
it was asked". The broker's log showed the model had done the work each
time: it ran the command, it searched, both ended as done, and then
three requests in a row came back with nothing to use, about a second
each. That was read from the install's log at the time, and isn't in
the bundle.

The model had answered. Its whole answer was in the reply's `reasoning`
field, where Ollama puts a model's thinking, and `content` was empty.

## The fault, repeated by hand

The request a chat really sends after a search was written out by a
throwaway test (`replay/dump_request.rs`: the agent's own system prompt
and its nine tools, 5,710 tokens as the model counts them) and put to
the real model on the install, six times each way
(`replay/replay.py`, `replay/replay-*.log`):

| What the request carried | `qwen3.5:9b`: empty replies | `gemma4:12b`: empty replies |
|---|---|---|
| nothing added, as a chat sent it | 6 of 6 | 0 of 6 |
| `"reasoning": {"enabled": false}`, what `disabled` sent | 6 of 6 | 0 of 6 |
| `"reasoning_effort": "none"` | 0 of 6 | not asked |
| `"reasoning": {"enabled": false, "effort": "none"}`, what `disabled` sends now | 0 of 6 | 0 of 6 |

Each of the twelve empty replies had the answer, whole, in `reasoning`.
With thinking off the same model answered in `content` every time, in
38 to 73 tokens.

Asked a short question with one tool and a one-line system prompt, the
same model answered in `content` 31 times of 31, with thinking on
(tried by hand; not in the bundle). It is the full request that tips
it, and nothing about what a search found.

## What changed

- **A reply that is all thinking is its own thing.** A task reply with
  text in `reasoning` or `reasoning_content` and no answer and no tool
  call is `ThoughtOnly`, where it was an empty reply.
- **It is asked again with thinking off.** The provider sends the same
  request again with thinking off, inside the three tries a request
  already has, and every request after it in that run goes with
  thinking off. The model's thinking is never taken for its answer.
- **An endpoint that refuses is left alone.** If the request with
  thinking off is answered by an HTTP status, thinking goes back to how
  it was set for the rest of the run, isn't turned off again, and the
  model's own reply, nothing, stands.
- **The failure has its own words.** A model that only thinks, with
  thinking off too, ends the run as `RunFailure::ModelThoughtOnly`
  (exit 74), and the chat says it thought about its answer and gave
  none, in place of "answered with nothing".
- **`disabled` works with Ollama.** It sends
  `"reasoning": {"enabled": false, "effort": "none"}`, where it sent
  `{"enabled": false}`.
- `docs/model-thinking.md`, which said there was no thinking-mode
  fallback, says what the one fallback is. `docs/install.md` and the
  changelog.

What doesn't change: thinking the operator set off is never touched;
the capability probe never changes the mode; a request is tried three
times and no more; a transport failure isn't asked again.

## Tests

Nine new, and four widened.

- **The provider** (`hatchling-model`, `provider::tests`, seven):
  - a reply that is all thinking is asked again with thinking off, the
    request otherwise the same, and the next turn goes with it off;
  - thinking is read from either field, only when it is text and not
    blank, and an answer or a tool call beside it is the reply;
  - a model that only ever thinks fails as that, in three requests, and
    the last try is never spent turning thinking off;
  - an endpoint that refuses leaves thinking on, and a status at any
    other time is the transport's failure;
  - a request lost after thinking was turned off is the transport's
    failure, and thinking stays off;
  - thinking set off is off from the first request and never turned
    off again;
  - the probe turns nothing off.
- **The broker** (`chat_run::tests`, one): the chat's words for a model
  that only thought, and that a silent model isn't told as one.
- **The agent, end to end** (`tests/local_model.rs`, one), against a
  scripted model server through the broker's egress: the answer after
  the ask made again is the run's, only that ask carried the field, and
  a model that only thinks exits as `ModelThoughtOnly`.
- **Widened**: the provider's two tests of the setting and the agent's
  test of it through egress expect the new field; the agent's test of
  which failures name themselves has the new one.

The canned transport the provider's older tests use now fails after
twelve requests. See the mutation section for why.

## Breaks by hand

Twenty-four, each one part of the change taken out or bent, the tests
of the four crates run on each, and each failing test named
(`unit-breaks.log`, `unit-*.log`, `ts_break.py`). All twenty-four were
caught, and the tests passed before and after.

| Break | Caught by |
|---|---|
| Thinking off is asked only as `enabled: false` | the fallback's test; both tests of the setting; the agent's egress test |
| Thinking off is asked only as `effort: none` | the same four |
| The setting doesn't turn thinking off | both tests of the setting; the set-off test; the agent's egress test |
| A reply that is all thinking is an empty one | six of the provider's seven; the agent's end-to-end test |
| Thinking is read from one field only | the fields' test |
| Blank thinking is thinking | the fields' test |
| No thinking is thinking | the fields' test |
| Thinking is never turned off | the fallback's, only-thinks, refusal and lost-request tests; the agent's end-to-end test |
| The last try is spent turning thinking off | the only-thinks test |
| Turning thinking off doesn't hold | the fallback's, only-thinks, refusal and lost-request tests |
| Turning thinking off uses no try | the only-thinks test |
| A run turned off goes on thinking | the fallback's, only-thinks, refusal and lost-request tests |
| Thinking set off is turned off again | the set-off test |
| Thinking left on is turned off again | the refusal test |
| A refusal is the transport's failure | the refusal test |
| A refusal leaves thinking off | the refusal test |
| Any lost request is a refusal | the lost-request test |
| A status at any time is a refusal | the refusal and set-off tests |
| The loop is told the model said nothing | the only-thinks and refusal tests; the agent's end-to-end test |
| The agent says the model said nothing | the agent's end-to-end test |
| The agent exits as for a silent model | the agent's failures test; its end-to-end test |
| The failure has the silent model's code | core's test of the codes |
| The failure isn't read back from its code | the broker's test of the chat's words |
| The chat says the model answered with nothing | the broker's test |

A first run of these, on tree `d6e237d9…`, reported one as survived:
with thinking never turned off, the agent's end-to-end test still got
its answer, from the plain second try. The four provider tests caught
the break; the agent's test was listed as a guard and couldn't tell the
two apart. It now reads which request carried the field.

## Mutation

`cargo mutants --in-place --in-diff` on the change to each crate's
source (`mut.log`, `mut-*-outcomes.json`):

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 2 | 2 | 0 | 0 | 0 |
| `hatchling-model` | 39 | 36 | 0 | 0 | 3 |
| `hatchling-loop` | 0 | 0 | 0 | 0 | 0 |
| `hatchling-agent` | 3 | 1 | 0 | 0 | 2 |
| `hatchling-broker` | 2 | 2 | 0 | 0 | 0 |

The loop's change is one error variant, with nothing in it to mutate;
`mut-loop.log` says so.

Each crate's `outcomes.json`, by sha256:

- core: `96e82b03f0a3d0f41d20641402d758ddf11938fef2356ac58221ce77345c88eb`
- model: `12fc24bf85614d69e9c5b97dd4f5b29e9a15283307580c71eab27ffe8713d5d1`
- agent: `9785608f15be30d82eaef0c5c4886438e6e00a300bae4eda59e764cc0486fce7`
- broker: `9f8dfba577f47b6abb75d02cb6c48293b4834ef67c9596ee81bc75f7362a89f2`

A run before this one, on tree `5276e558…`, had two mutants time out in
the provider, both on the line that counts a stray reply's tries: the
guard made always true, and `+=` made `*=`. Either makes the provider
ask without end, and an older test's canned transport answered without
end, so the test hung where it should have failed. That transport now
fails after twelve requests, and both mutants are caught. Only tests
changed between that tree and the tested one, and every run in the
bundle is of the tested tree.

The unviable ones:

- `provider.rs:261:9: replace ChatCompletionsProvider<T>::next_action_inner -> Result<ModelAction, Error> with Ok(Default::default())`: unviable, build failure
- `provider.rs:441:9: replace ChatCompletionsProvider<T>::parse_response -> Result<ModelAction, Error> with Ok(Default::default())`: unviable, build failure
- `provider.rs:530:9: replace <impl Model for ChatCompletionsProvider<T>>::next_action -> Result<ModelAction, LoopError> with Ok(Default::default())`: unviable, build failure
- `error.rs:536:9: replace Error::run_failure -> Option<hatchling_core::run_failure::RunFailure> with Some(Default::default())`: unviable, build failure
- `model.rs:136:9: replace Cell<Ready>::run_task -> Result<LoopResult, Error> with Ok(Default::default())`: unviable, build failure

## The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3882 passed, 0 failed | exit 0: 135 binaries, 3882 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

The second run, on tree `5276e558…`, failed `clippy` on both
toolchains at one thing: a test function two lines over the length the
lints allow. The agent's new cases moved into a test of their own.

The tests of what the change touches were run as root too, as CI runs
them: 192 in core, 44 in the model crate, 175 in the agent and 9 in its
local-model test, 21 in the broker (`root/as-root.log`).

## On a real install

Added on 2026-11-14, after the change's own commit and before it was
merged. The commit's message says the built change hadn't run on an
install and the new field hadn't gone to a hosted endpoint: both were
so when it was written, and neither is now.

The build of the pull request's head (merge commit `94dd4755`, whose
tree is the tested tree and this record) was put on an install, in
place of main `c301605d`, and the chats that had failed were sent again
(`hatchling-search-and-thinking-real-install-results.tar.gz`, 3860 bytes, sha256
`48574721602a9dcbe3adcdbb4f30f6e62b42c0e3930caae9a6b2fcfd0a3b2825`;
`chat-answers.txt`, `qwen-before.journal.txt`,
`qwen-after.journal.txt`, `hosted-after.journal.txt`, `install.txt`):

| Chat, `qwen3.5:9b` | Before | After |
|---|---|---|
| Search the workspace for TODO | once: "answered with nothing"; exit 65; 5 model requests | twice: both matching lines, as `path:line:text`; 4 model requests each |
| Run one command that makes three files, then search | twice: "answered with nothing"; exit 65; 6 model requests each | once: the two files and lines with TODO, and that the third has none; 5 model requests |

The requests counted include the capability probe. Each run after the
fix has one more than its probe, its tool calls and its answer need:
the reply that was all thinking, asked again. Each run before it has
three such replies and no answer. The matching lines in the answers are
what ripgrep found in a real Room.

**The hosted endpoint.** One chat was sent through the install's hosted
model (`nvidia/nemotron-3-ultra-550b-a55b:free` at openrouter.ai, set
to `disabled`), so its probe and its task request each carried
`"reasoning": {"enabled": false, "effort": "none"}`. Both were answered
with status 200, and the chat showed the answer asked for.

## What wasn't tried

- **A hosted model that must reason.** OpenRouter's documentation says
  a model that mandates reasoning rejects a request that turns it off.
  With `disabled` set that is the operator's choice and was so before;
  with `provider-default`, the fallback's request would be refused and
  thinking left as it was. No such model was tried.
- **Other model servers.** llama.cpp's own server, vLLM and LM Studio
  have their own ways of turning thinking off and may put thinking in
  other fields. A server that ignores the field leaves the model
  thinking, and the chat then says it thought and gave no answer.
- **A model that thinks only at the capability probe.** It is refused
  there as it was, as a model that doesn't call tools.
- **What turning thinking off costs the answers.** The model's tool
  calls with thinking on were right in the chats that showed the fault;
  whether its work is worse with thinking off, after the first reply
  that is all thinking, wasn't measured.
