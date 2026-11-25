# WO-051: A chat run bounded by time, tokens and context, with Stop

**Status:** IN-PROGRESS
**Area:** `crates/hatchling-loop` (the budget, the last step), `crates/hatchling-agent` (its bounds, the context guard), `crates/hatchling-model` (usage as reported), `crates/hatchling-broker` (run limits by kind, a run's token budget, Stop), `crates/hatchling-core` (the Stop action), `frontend/` (the Stop button), `docs/`
**Issue:** [#647](https://github.com/undergroundrap/hatchling/issues/647), for [#533](https://github.com/undergroundrap/hatchling/issues/533)
**Trust boundary:** yes

## Problem

A chat run gets 25 model turns (`CHAT_MAX_STEPS`), whatever its window, its time or its cost. A small model that writes one file a call spends them fast: the maintainer's `qwen3.5:9b` stopped "out of steps" at 24K of its 64K tokens, with room to finish. A large model would have finished in a few turns. A turn count is the wrong bound for a run whose owner is there.

Nothing else bounds a chat run's work today except the broker's wall clock, ten minutes for every kind of run, and the owner's day and month token limits. Nothing watches the context window during a run. Past it, a local model's server silently drops the oldest of the prompt, and the system prompt, with its rules and the fences around what the agent read, goes first. A hosted call fails the run instead. And the owner can't stop a run: the Stop button is in the demo only.

## Threat model

More turns give a prompt-injected run more tries while the owner is there. A run with nobody there keeps its 10. What bounds a try is unchanged at turn 200 from turn 2:

| Threat | What bounds it today | What this WO adds |
|---|---|---|
| An injected run spends its turns trying to send a secret out | Egress grants by exact destination, deny by default; the agent holds surrogates, never a secret; fetched secrets are redacted from every model request; a secret seen in a web read, a search, a page or a harness's traffic ends the whole run at once (held check); every reply to a person passes the output gate | Nothing new needed: each try meets the same guards. The turn cap stays as a backstop against loops |
| It floods the owner with approval requests to get one yes | The approvals board: 5 pending, 1 per task, 20 an hour | Unchanged |
| It runs up a hosted bill, or holds the machine, while the owner looks away | The 10-minute clock; the owner's day and month token limits | A per-run token budget, enforced by the broker; a chat clock; Stop |
| The prompt outgrows the window, and a local server silently drops its start, rules first | Nothing | The context guard: no request is sent larger than the window, less room kept for the answer. A run that fills it ends with an answer that says so |
| The agent lies about its own budget, or ignores it | n/a | Every bound is the broker's: its clock kills, and its egress refuses a model call past the token budget. The agent is told its bounds so it can finish gracefully, but nothing depends on it doing so |
| The agent stops itself, or forges a Stop | n/a | Stop is the owner's action in the app, through the local API. Nothing in a cell reaches it |

The context guard is a security fix as well as a quality one: today an injected page that fills a local model's window pushes the rules out of the prompt.

## Design

1. **Turns.** A chat run's cap becomes 200, a backstop against loops, not a budget. A run with nobody there (a schedule's, a hook's, a follow-up's, a child's) keeps 10, and the subagent caps are unchanged.
2. **Time.** The broker's clock is set by kind: a chat run gets **30 minutes** (proposed), and a run with nobody there keeps 10. The broker tells the cell its deadline. Inside the last 90 seconds, the loop's next step is its last, with no tools: it says where it got to. The broker still kills at the deadline, as it does today.
3. **Tokens.** A run has a token budget, counted by the broker from the usage each model server reports, as the owner's day limit is counted. A chat run gets **2,000,000 input tokens** (proposed), summed over its calls. That's about $6 at a frontier model's list price, and room for 200 turns at a 10K prompt. A run with nobody there gets 200,000. The broker refuses a model call past the budget. The agent is told its budget and reads the usage too, and takes its last step when the next call would cross it. The owner's day and month limits stay as they are.
4. **Context.**
   - **Room for the answer.** The agent keeps room for the answer: an eighth of the window, at least 1,024 tokens.
   - **Measuring the next request.** Before each call, it sizes the next request: the prompt tokens the last reply reported, plus a third of the bytes added since. A server that reports no usage is sized at a third of the whole request's bytes.
   - **When it won't fit.** If that leaves less than the room kept, this step is the last, with no tools, and says it ran out of room. If even that wouldn't fit, the run ends as out of room, without sending.
   - **Tool output.** A tool's output longer than a quarter of the window is cut before it joins the history, and the cut says so.
   - **Nothing summarized.** No message is summarized or dropped: WO-026's rule holds, and #595 sizes the history a run starts with.
5. **Stop.** While a chat run works, the app shows **Stop**: a new owner action, `stop_run`, on the local API. The broker ends the run's whole tree at once, by the path a held secret already uses. Nothing the run says after is delivered, and the chat says "You stopped it." The Activity view keeps what it did. This adds a UI protocol action: a change to `frontend/src/protocol` and `PROTOCOL-LOCK.json`, which merging this design approves.
6. **Ending on a limit.** A run that ends on any limit says which one, plainly. Its answer can be kept for later, as an out-of-steps answer can now. #531 (*Keep going*) is next.
7. **Measured.** The same tasks run before and after, with a small local model (`qwen3.5:9b`) and a large hosted one. Each run records turns used, time, tokens, and whether it finished. The results are in the evidence.

## Scope

**In scope:**
- the chat turn backstop;
- the clock by kind, with its deadline told to the cell and a last step before it;
- a run's token budget, the broker's and the agent's;
- the context guard, and tool output cut to the window;
- Stop, in the protocol, the broker and the app;
- the owner's words for each limit;
- the docs that state these limits (the trust boundary, `docs/storage.md` and the app's help where they name a turn count or a run's time);
- the measurement.

**Not in scope:**
- *Keep going* (#531);
- the history a run starts with (#595);
- the context meter (#535);
- a model-written summary of anything (WO-026);
- an operator setting for each bound. The defaults are the product; settings come if they're asked for.

## Slices

1. This design, the threat model, and the maintainer's defaults.
2. The context guard: usage read from each reply, the next request sized, the last step for room, tool output cut, and the owner's words. The largest gain for small local models.
3. Time and tokens: the clock by kind, the deadline and the budget told to the cell, the broker's refusal past the budget, the last step before either, and the chat cap at 200.
4. Stop: the action, the broker's end of the run tree, and the app's button.
5. The measurement, and the close.

## Acceptance criteria

- [ ] A chat run isn't stopped by a turn count before 200. A run with nobody there still stops at 10, and tests show both.
- [ ] No model request is sent larger than the window less the room kept for the answer. A run that would need one ends with an answer that says it ran out of room. A test fills a window to show it, at a server that reports usage and at one that doesn't.
- [ ] A tool's output longer than a quarter of the window is cut before it joins the history, saying so.
- [ ] A chat run's clock is 30 minutes and a run with nobody there keeps 10. A run inside its last 90 seconds answers instead of being killed, and a test shows it.
- [ ] The broker refuses a model call past a run's token budget, whatever the agent does. The agent, told its budget, answers before crossing it. Tests show both.
- [ ] The owner can stop a running chat from the app. The run's whole tree ends at once, nothing it says after is delivered, and the chat says so. Tests cover the action, the broker and the button, and only the owner can send it.
- [ ] Each limit the owner meets is named in plain words, and its answer can be kept for later.
- [ ] Each new guard fails its test when removed.
- [ ] The measurement before and after, on the same tasks with both models, is in the evidence: turns, time, tokens, and whether each finished.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates). Slice 4 changes the UI protocol: `protocol-schema --write` and `protocol:generate`, with both outputs committed.

## Evidence

- Slice 2, the context guard: [context-guard.md](../evidence/context-guard.md).
- Slice 3a, the clock by kind and the deadline: [run-clock.md](../evidence/run-clock.md).
- Slice 3b, the token budget: [run-token-budget.md](../evidence/run-token-budget.md).
- Slice 4, Stop: [stop-run.md](../evidence/stop-run.md).
- The measurement found Stop and the clock waiting for a model call in flight ([#657](https://github.com/undergroundrap/hatchling/issues/657)), fixed before the close: [stop-ends-in-flight-call.md](../evidence/stop-ends-in-flight-call.md).

The acceptance criteria are ticked at close, against these and the later slices'.

## Notes

- **Decisions for the maintainer, made by merging this design or by saying otherwise:**
  - a chat run's clock (30 minutes);
  - a chat run's token budget (2,000,000 input tokens, and 200,000 for a run with nobody there);
  - Stop ends the run at once, rather than asking it to wrap up;
  - the new protocol action.
- **Why the clock and the budget are the broker's:** the agent runs in a cell the broker doesn't trust. A bound the agent keeps is a courtesy; the broker's is the bound.
- **Why a third of the bytes:** tokenizers differ by model, and none runs in the cell. Three bytes a token undercounts English prose, which runs nearer four, so the estimate errs toward stopping early. A server's reported usage replaces the estimate as soon as there is one.
