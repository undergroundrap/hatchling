# WO-054: Background runs: seats that propose, code that applies, and a clock that wakes them

**Status:** QUEUED
**Area:** `crates/hatchling-broker` (the background clock, the seat runner, the apply steps, the review queue), `crates/hatchling-core` (a seat's packet, handles, finish output and the queue's types), `crates/hatchling-agent` (the finish tool and a seat's read tools), `crates/hatchling-prompt` (each loop's kind, routing its waiting sections), `installer/windows` (keeping the distro up)
**Issue:** [#718](https://github.com/undergroundrap/hatchling/issues/718)
**Trust boundary:** yes

## Problem

Hatchling runs when it's asked: a chat turn, a schedule the owner approved, a subagent. Nothing runs on its own between those. Four kinds of background work have their prompt text written and waiting (#714), and none can run yet:
- **memory reconciliation:** keep what the agent remembers honest and compact;
- **goals bookkeeping:** keep the goals it tracks faithful to the owner's projects;
- **alignment:** keep how it works with this owner current;
- **Ideas:** refresh the cards worth acting on.

Two more pieces are missing too.

- **The common machinery.** Each loop is a few model steps between deterministic ones. Built four times, the same guarantees get built four ways, so they're built once here:
  - a model only proposes;
  - every claim is checked against what's really there;
  - one repair, then stop;
  - an honest "nothing to do";
  - code owns every durable write.
- **A clock that actually wakes them.** On Windows, WSL stops a distro about 30 seconds after its last `wsl.exe` exits. Setup's **Keep Hatchling running while I'm logged in** holds one idle process from the Startup folder, but it's off unless the owner chose it, and nothing notices when it's missing. A PC also sleeps. A loop that is due while nothing runs has to catch up, once, when Hatchling is next up, and the owner has to be able to see that.

**The maintainer's decisions (2026-11-26):**
- **Routine memory writes run unattended**, under the loop's own checks: evidence handles, a verifier that reopens them, ungrounded claims dropped.
- **Destructive ones go to a review queue the owner clears when they want.** That means replacing or removing a preference, a boundary or a commitment; facts about who the owner is; and anything a verifier couldn't resolve. The queue never blocks a run.
- **Learned guidance works the same way:** distilling and checking it is the control, and only installing new guidance goes to the queue.
- **Nothing acts on the owner's silence.**
- **Money always needs the owner's yes.**
- **Nothing is published off the machine** until that has its own design and threat model. Location gets one too.
- **The evaluation harness (WO-045) is built alongside,** and measures the loops as they're built.
- **Keeping the distro up belongs to this machinery,** not a separate project; the mechanism is this work order's.

## Threat model

A background run reads the owner's most private material: the conversation, memory, goals. It runs while nobody is there, and what it writes comes back in later runs' prompts. Each threat below is one this design must stop, and its test.

| Threat | What stops it |
|---|---|
| A page, a file or tool output in the conversation plants an instruction, and the memory run makes it standing memory, or a goal | A seat only proposes. Each claim must carry a handle to where it came from, and the apply step resolves it against the snapshot. A claim whose only source is outside content (a page, tool output, someone else's message), not the owner's own words, is never applied: it goes to the queue, or is dropped when it directs the agent. The sections' injection rule says the same to the model; the code doesn't rely on the model following it. |
| A seat writes durable state itself | A seat's cell has no write tool, no Room, no network. Its tools are reads of the packet's own snapshot and one `finish` call. Only the broker's apply step writes, after validation, in one serial pass. |
| A seat invents an id, cites a message that doesn't exist, or targets a goal the owner set | Validation resolves every id and handle against the snapshot taken at the run's start, and refuses the whole output if one doesn't resolve. A tracked goal may be changed; an owner's goal is read-only, by code (the ledger law), not by prompt. |
| A run applies to state that changed under it: the owner edited memory or closed a goal mid-run | The snapshot carries a fingerprint per record. Apply rechecks each one and skips, counted, any record that changed. |
| A loop loses established memory: a model compresses away a boundary | A replace or removal of a preference, a boundary, a commitment or an identity fact goes to the queue, never applied. A demoted line is appended, verbatim, to the dated log before it leaves the projection. |
| A destructive proposal waits forever, or blocks the loop | The queue is append-only and never gates a run. Its items are shown with their evidence, and the owner approves or rejects each. |
| The background runs up the owner's bill, or holds a local model the owner needs | Each loop has a daily token budget inside the owner's day and month limits, which still apply. A loop with nothing new (its evidence probe) spends nothing. Background runs only while no chat run is active, one at a time, and stops at its next step when the owner starts one. |
| A loop speaks to the owner on its own, or sends something outward | A seat has no send tool. Anything the owner should hear goes through the existing delivery (the run's result in the chat), under the delivery rules. Nothing leaves the machine: there's no publishing path. |
| A loop runs when the owner paused everything | The background clock honors the pause (#673) before each run and each step. |
| A run's measurement leaks content | The gate's events hold counts, durations and outcomes, never text. They're kept with the run trail, in the clear, as Activity's are. |
| The owner thinks background work runs when it doesn't | The app says when each loop last ran, when keep-running is off, and when Hatchling was stopped while a loop came due. |

## Design

### The background clock

- **A broker-owned clock, separate from the owner's schedules.** Each loop has a cadence:
  - memory: hourly, when there's new activity;
  - goals: after new conversation, debounced;
  - alignment: nightly, in the owner's machine's local night (the timezone #712 reads);
  - Ideas: daily.
- **Catch up, never pile up.** A loop that came due while Hatchling was down runs once at the next wake, whatever it missed. Due times are kept as wall-clock instants, so a sleep or a suspend shows up as a late start and the run catches up.
- **One at a time, chat first.** Background runs only while no chat run is active. Between steps it checks again and stops, resumable from the step it reached, when the owner starts a chat.
- **Off as a whole.** The owner can turn background runs off as one setting, and each loop separately.

### Keeping it up

- **The Startup entry becomes the default.** Setup offers **Keep Hatchling running while I'm logged in** checked by default, on the reasoning that an always-on agent should be on. The app offers the same switch, so it can be turned on later.
- **It's watched.** The broker can't see Windows or start its own distro, but it knows two things:
  - **Whether keep-running is on.** Setup records it in the Windows marker the broker already reads at start.
  - **Its own gaps.** It notes when it starts, and a loop that came due while it was down shows the gap.

  When keep-running is off, the app says background work runs only while Hatchling is open, and offers to turn it on. After a gap, the app says Hatchling was stopped from when to when, and that background work caught up when it came back.
- **No new persistence.** No scheduled task, no service and no admin. The same Startup-folder shortcut is kept; earlier work measured that it scans clean with Defender. Waking a sleeping PC is out of scope: it catches up on wake.

### The seat runner

- **A background run is a list of steps.** Each is deterministic code in the broker or a model seat. The runner gives each step its input, takes its output, and writes the run's trail.
- **A seat is a run in a cell, with its own kind** (`MemoryReconcile`, `GoalsEvaluate`, and so on), which routes its waiting sections. Its assignment is one message with named sections, fenced as data. Its tools are read tools over the run's snapshot, and `finish`, whose JSON schema is the step's output.
- **One finish call.** Prose outside it is dropped. A `finish` whose output fails its schema is answered with the fields that failed, once; a second failure ends the step with nothing.
- **Evidence handles.** Every claim cites a handle: `message:<conversation>/<id>`, `memory:<id>`, `goal:<id>`, `run:<id>`, `file:<path>`, or a quoted span of one. A handle that doesn't resolve fails the claim, not the step.
- **One bounded repair.** Where a check fails an artifact, its writer gets one repair turn with the exact reasons, then one re-check. A second failure is final for that artifact.
- **`no_change` is a result,** counted like any other. Most runs should end there.
- **The gate step** records what the run produced, rejected, repaired and spent, and how long it took. Its records are the evaluation harness's input.

### The review queue

- **An append-only, sealed store of proposals** a loop may not apply: what would change, why, its evidence handles, and the loop and run that proposed it.
- **The owner approves or rejects each one in the app.** An approval is applied by the same apply step, with its fingerprints rechecked.
- **A proposal whose target changed meanwhile** is shown as stale, and approving it does nothing.

## Scope

In order, each its own pull request with its gate. The loops themselves get their own work orders.
1. **The background clock:** cadences, catch-up, chat first, pause, the setting; with a test that a suspend shows up as one catch-up run.
2. **Keeping it up:** setup's default, recording it in the Windows marker, the app's switch, and the app's notes on keep-running being off and on gaps.
3. **The seat runner:** the step list, a seat's kind and packet, `finish` and its schema check, handles and their resolution, one repair, `no_change`, the gate's events.
4. **The review queue:** the store, the local API, the app's list.
5. **The first loop, memory reconciliation,** in a work order of its own, on WO-048's provenance. It proves the runner end to end. The harness's first slice measures it.

## Acceptance criteria

- [ ] No seat can write durable state. A test gives a seat every tool the agent has and shows the runner offers it only its reads and `finish`.
- [ ] Apply refuses an output that cites an id or a handle that doesn't resolve, or that targets an owner's goal; each is a test.
- [ ] A record changed between snapshot and apply is skipped and counted.
- [ ] A destructive memory change reaches the queue and is never applied without the owner's approval; a test tries each kind.
- [ ] A claim sourced only from outside content is never applied.
- [ ] The clock runs a missed loop once after a gap of many periods, and never while a chat run is active.
- [ ] The pause stops the next run and the next step.
- [ ] The app says when each loop last ran, when keep-running is off, and when Hatchling was stopped while a loop came due.

## Gates

The usual: fmt, clippy on both toolchains, the whole workspace's tests, the audit, hand-breaks for each guarantee above, and mutation on the changed lines. Each slice also gets the harness's measurement once there's a loop to measure.

## Evidence

Each slice commits its evidence under `docs/evidence/`.

## Notes

- **Placement.** The memory loop needs WO-048's provenance: who said a thing, where it came from, what it supersedes. So this work order and the first loop fit v0.11, "Memory you can trust", beside WO-048 and WO-052. That's the maintainer's call.
- **What isn't here.** Publishing lessons off the machine, place signals and location, connectors, and the proactive composer each wait for their own design.
