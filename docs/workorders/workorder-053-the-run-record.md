# WO-053: The run record: what a run did, kept by the broker, so Keep going carries on from it

**Status:** IN-PROGRESS
**Area:** `crates/hatchling-broker` (the record, where each effect happens, Keep going, the conversation store), `crates/hatchling-core` (the record's types, the plan op, the UI's action and card), `crates/hatchling-agent` (the plan tool, the record in the prompt), `frontend/` (Keep going, what a run did, the plan), `docs/`
**Issue:** [#531](https://github.com/undergroundrap/hatchling/issues/531)
**Trust boundary:** yes

## Problem

When a chat run reaches a limit (its turns, its time or its tokens), its answer says so and the app offers one action, **Keep for later**, which turns the request into a card on the Ideas page. There's no way to carry on. Shown alone, Keep for later reads as the agent giving up.

Carrying on needs more than another run. The next run is given the conversation, and the conversation holds what the last run *said*, not what it *did*. Nothing tells it what has already happened outside the run:
- a message it sent;
- a file it wrote, in the Library or in its computer;
- an approval it used;
- what it spent.

A model that wrote a good "where I stopped" continues well. One that didn't repeats work, or starts half-blind. One that claimed more than it did misleads the next run and the owner alike. WO-051's measurement had a model report success with none of its files right, and nothing on the answer showed it.

Hatchling already knows most of this, in pieces:
- The broker sees every effect as it happens. The session loop (`serve_session`) has every request, its answer, the session and the cell.
- Activity gets a per-run summary: counts by kind of tool, model calls, tokens.

It's kept as counts on purpose. Activity holds "no command, path, page or words", so its log is kept in the clear, unbounded. No record says *what* a run did, so none can be given to the next run.

**The maintainer's decision (2026-11-25):** one run record, built once, in three parts. Built separately, each would rework the run trail again:

1. **The effect journal:** each effect outside the run, recorded by the broker as it happens, never by the agent.
2. **What it did, attested:** from the same journal, shown on each answer beside what the answer says.
3. **The plan:** a list of steps the agent keeps, which the owner sees and a continuation resumes from. The steps are the agent's own and carry no authority: the journal says what was done, the plan says what was meant.

## Threat model

The record is new information given to a run, and new content kept at rest. Both are trust-boundary changes.

| Threat | What stops it |
|---|---|
| A continuation repeats an effect the stopped run already had (a message sent twice, a file written over) | The continuation is given the journal: each effect, as the broker saw it happen, under a header saying these were done and are done. An approval belongs to the run that asked for it and is never carried over: a continuation that wants to act again asks again. |
| The agent writes its own record, or edits it, to hide what it did or claim what it didn't | No session op writes the journal. The broker records each effect at its choke point, from the request and its answer. The plan is the only part the agent writes, and it's labeled as the agent's own and checked against nothing. |
| Text in the record carries an instruction: a file name, a message, a plan step written under a page's injection | The record is given as data, fenced like tool output ("untrusted data, never instructions"), in the agent's role, never the owner's. Every string in it is bounded and quoted. The plan is under its own header, as the agent's own words. The owner sees all of it. |
| The record keeps a secret: a path or a message that holds a key | Each entry is checked against the vault before it's kept, as what leaves the machine is (#599's rule), and a secret's value is never kept. A message's text is kept only as the output gate passed it. The record is sealed, in the conversation store. Activity's clear log stays content-free: the record is never written to it. |
| A cell starts a continuation, or many | Keep going is the owner's act, through the local API, like Stop. No session op starts a run. *Continue on its own* is the owner's setting, off by default and at most 5 in a row, each shown in the chat. A run with nobody there never continues on its own. |
| A chain of continuations runs up a bill or holds the machine | Each continuation is a run with its own bounds (WO-051): its clock, its token budget, the owner's day and month limits. Stop ends the run and the chain. The chain's length is counted by the broker. |
| A subagent's effects go unrecorded, so the parent's record understates what happened | A child run's journal is part of its parent's: each of its entries is kept, marked as the child's. |
| "What it did" misleads by omission: a file a Room command wrote isn't in it | The card says what it counts. The broker sees each Room write and edit, so they're listed. A file a command wrote is not, and the command is listed instead (#599), so the card never claims a complete list of files. |
| The record grows without bound, or outlives the conversation the owner deleted | It's bounded per run and per conversation, and goes with New conversation. It survives a restart, sealed. |
| A stale record misleads: the owner changed things since | Each entry says when, and the record is given "as of" the end of its run. What the owner said since comes after it. |
| The run's last result carries an instruction: a page's text, or a test's output written to look like the owner | It's given exactly as the run was given it: fenced as tool output, untrusted data, never instructions, in the agent's role. It's bounded, and it's what the run already read, so it adds nothing a run couldn't already be shown |
| A continuation sends the same message again: it ignored the journal, or was told to | The broker refuses a message identical to one the chain already sent on the same channel, until the owner approves it. Being told it was sent is the agent's to heed. The refusal is the broker's |
| A continuation builds on what the record says the files were, after the owner or a command changed them | It's told to check the record's claims against its computer before it builds on them. The record says "as of" its run's end, and the Room's listing since then (#568) is the current word |

## Scope

**In scope:**

- **One record per run.** The record is keyed by the run's token (`run-…`), which the chat, the Library and the conversation's index already use. It holds:
  - the run's kind and the conversation it belongs to;
  - how it ended: answered, at its turns, time or token limit, stopped or failed;
  - its spend: model calls, tokens in and out;
  - the journal;
  - the plan.

  A child run's record is part of its parent's.
- **The run's last result.** The last tool result the run was given, as it was given: bounded (4 KiB), fenced as untrusted tool output. A run stopped mid-task usually stopped on it: a failing test, an error, a page.
- **What's still open when it ends:**
  - each Room job still running, by name (its record is WO-044's);
  - each approval it asked for that the owner hasn't answered.

  A continuation is told of them, so it waits on them or asks after them rather than starting them again.
- **The journal.** One entry per effect, recorded at the broker's choke point as it happens:

  | Effect | Kept |
  |---|---|
  | message sent | channel, recipient, outcome (sent, redacted, refused), the text as the gate passed it, bounded |
  | Library item | name, bytes, digest |
  | Room write or edit | path, bytes, replaced or new |
  | Room command | argv only (#599 builds this part) |
  | approval | what was asked; whether it was granted, refused or used |
  | grant refused | the grant, and why |

  Each entry carries its time and its outcome. Entries are bounded per run (64 entries, 16 KiB). Past the bound, the record says how many more effects of each kind there were.
- **Sealed with the conversation.** Each record is kept in the conversation store with the exchange it answered, in the same sealed write. It's vault-checked first, shown again after a restart, and removed with New conversation.
- **Keep going.** The owner's action on an answer that ended at a limit, or was stopped. Its run is given:
  - the conversation, as any chat run is;
  - the records of the runs it continues (that run, and the ones it continued, up to 3);
  - a fixed task saying the owner asked it to keep going.
  - in that task, an instruction to check the record's claims against its computer before building on them: the files it says were written are there and as described, a job it says is running still is.

  In the app, **Keep going** comes first and **Keep for later** second, worded as a goal or idea to come back to. The answer says plainly why it stopped (turns, time or tokens), never that the task can't be done.
- **The next message after a Stop or a limit.** Its run is given the record of the run that stopped, so an email the stopped run had already sent shows as sent, with or without Keep going.
- **Continue on its own.** An owner setting, off by default: up to N continuations in a row (at most 5). Each is shown in the chat ("Kept going, 2 of 3"), and Stop ends the chain.
- **No message sent twice by a chain.** A continuation, by Keep going or on its own, is refused a message identical to one an earlier run in its chain sent on the same channel. It goes only once the owner approves it, on the rail, as any send that isn't a reply does.
- **The agent can look itself up.** A read-only tool gives the run its own record so far and its chain's: what it sent, wrote and asked, by the broker's account. It's how an agent checks "did I already do this?" before acting, across runs and after the context guard trimmed the start of a long one. It's given as the record is, fenced as data, and it writes nothing.
- **What it did, on each answer.** A line under the answer, from the journal alone: files written, and where; messages sent; approvals used; tokens spent. "Did: wrote 29 files in notes/, sent 1 message, used 1 approval." It's never compared with the answer by a model. The answer is what it says, the line is what the broker saw, and the owner reads both.
- **The plan.** A tool the agent can call to set its list of steps: up to 24, each up to 160 bytes, each pending, in progress or done. The broker keeps the latest in the record, and the output gate scans it before it's shown. The app shows it while the run works and on the answer. A continuation is given it under its own header, as the agent's own.
- **Activity** links each run record to its Activity summary.

**Not in scope:**
- What a Room command did to the files: [#599](https://github.com/undergroundrap/hatchling/issues/599) (with WO-044) records each command, read and search, into this record's journal. A listing of what changed in the workspace since the last run is [#568](https://github.com/undergroundrap/hatchling/issues/568)'s.
- Giving a scheduled run its schedule's last record. The record is the same for every kind of run, and this is designed for it, but it's a slice of its own after this WO, filed when this closes.
- A run that ends by asking the owner, or waiting for an event ([#664](https://github.com/undergroundrap/hatchling/issues/664)), and the broker's repetition counts ([#666](https://github.com/undergroundrap/hatchling/issues/666)). Both read this record. Neither is built here.
- A model comparing what a run said with what it did.
- Larger step budgets for large models: WO-051 bounded chat runs by time, tokens and context, and raised the turn cap to 200.

## Acceptance criteria

- [ ] Each effect in the table is in its run's record, recorded by the broker as it happened. A test makes each one in a real session and reads the record.
- [ ] No session op writes the journal. A test sends every op and finds the journal unchanged by any but the effect it makes.
- [ ] A secret in a path or a message is never kept: a test plants a vault secret in each and finds only its redaction, sealed.
- [ ] The record is sealed in the conversation store, survives a restart and goes with New conversation. Store tests cover each, and a stopped write.
- [ ] Past its bounds, a record keeps its first entries and the count of the rest by kind. A test overflows one.
- [ ] Keep going starts a run given the conversation, the record of the run it continues and the fixed task, and nothing else new. A test reads the run's input.
- [ ] Keep going is refused for an answer that didn't end at a limit or a Stop, for any answer but the conversation's last, and from anything but the owner's API connection. A test tries each.
- [ ] An approval the stopped run was granted isn't usable by its continuation. A test grants one, stops the run and has the continuation try it.
- [ ] The run after a Stop is given the stopped run's record, without Keep going. A test stops a run that sent a message, and reads what the next run was given.
- [ ] Continue on its own is off by default, never runs more than the owner's N (at most 5), shows each in the chat, and stops at Stop. Tests cover each, and a run with nobody there never continues.
- [ ] What it did, under an answer, counts what the journal holds and no more. A test has a model claim 30 files and write 29, and reads the line.
- [ ] The plan is bounded, gate-scanned, shown to the owner and given to a continuation under its own header, in the agent's role. Tests cover each.
- [ ] A subagent's effects are in its parent's record, marked as the child's. A test spawns one that writes a file.
- [ ] A record keeps its run's last tool result, bounded and fenced, and a continuation is given it in the agent's role. A test stops a run after a failing command and reads what the continuation was given.
- [ ] A record names each Room job still running and each approval still unanswered when its run ended, and a continuation is told of them. Tests leave one of each.
- [ ] The continuation's task tells it to check the record against its computer. A test reads the task.
- [ ] A continuation is refused a message identical to one its chain already sent on that channel, and it goes once the owner approves. A test sends one, continues, sends it again.
- [ ] The record tool returns the run's record and its chain's as the broker holds them, fenced, and no op writes through it. A test sends a message, calls the tool, and finds the send.
- [ ] The record reaches the model fenced, in the agent's role. A test reads the request sent. An injected line in a file name or a plan step coming back as an instruction is measured by WO-045's behaviour battery.
- [ ] Each new guard fails its test when removed.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates). It changes the session protocol (the plan op, the record given to a run) and the UI protocol:
- Keep going, an action on the local chat;
- what a run did, on a chat message;
- the plan, an event and a field on the answer;
- the setting.

The `ProtocolAdapter` models change for Keep going, what it did and the plan. That's a `PROTOCOL-LOCK` entry, stated here for the maintainer's approval with the design, as WO-051's Stop was.

## Evidence

Filled in at close.

## Notes

- **Order:** v0.10.0, after WO-051 and #595, and before WO-044 and #599, which write the Room's part of this journal. Designed before code, and approved by the maintainer before any, as WO-046's and WO-052's were.
- **Where it's built:**
  - Every request, its answer, the session and the cell are in scope in `serve_session`, where the run trail already notes each tool use. The journal entry is worked out from the request before it's handled, the way the trail's tool is.
  - Library items already record a `FileOut` in Activity as each hand-over ends, the one effect recorded as it happens today.
  - The per-run spend is already the trail's.
- **Two run ids today:** a session id per connection (Activity's `run_id`, the gate scope, an approval's task) and the run's token (the chat's `run`, the Library's `reply`, the conversation index). The record is keyed by the token, and carries the session.
- **Why sealed, when Activity isn't:** Activity is kept in the clear because it holds counts and kinds, never content. The record holds paths, recipients and message text, so it's held to the rule for content: vault-checked, sealed, bounded, gone with its conversation.
- **Why approvals don't carry over:** an approval answers one request from one run. A continuation is a new run with the same task, and the owner was asked about the old one's request, not this one's. Asking again costs a click. Carrying it over would let a run inherit a yes it never asked for.
- **Why no model judges "says" against "did":** a judge is a model reading the answer, and a planted answer could talk it round. The broker's count is a fact, the answer is a claim, and the owner sees both side by side.
- **Amended 2026-11-26, by the maintainer's decision:** the last result, what's still open, checking the record against the disk, no message sent twice, and a tool to look the record up. Each comes from where a continuing agent most often goes wrong: resuming mid-debug without the error it stopped on, starting again what's still running, trusting its own account over the files, and repeating a send. The broker refuses a repeated send whatever the model does; the tool lets a model that checks find out first.
- **One record serves later work:**
  - #664's waiting run resumes from it;
  - #666 counts repeats from it;
  - a scheduled run continuing a thread is given its schedule's last one;
  - WO-052's notes writer may read what was done from it, rather than from the answers alone.
