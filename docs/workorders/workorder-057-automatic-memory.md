# WO-057: Automatic memory, every memory traceable

**Status:** IN-PROGRESS
**Area:** `crates/hatchling-broker` (memory keep, the run's taint, the review queue), `crates/hatchling-core` (UI and session protocol), `crates/hatchling-agent` (memory_propose), `frontend/` (the Memory page, chat notices, the setting)
**Issue:** [#767](https://github.com/undergroundrap/hatchling/issues/767)
**Trust boundary:** yes (what's kept in the owner's memory without their click)

## Problem

Today nothing is kept in memory without the owner approving its words on a card. WO-055 made that one card per answer, but for an always-on agent it's still friction: the owner has to stop and decide, or the memory is lost.

Claude Code keeps its memories without asking. The maintainer wants the same here (2026-11-27), as long as every memory stays traceable and the security holds.

The one real threat is **memory poisoning**: text the agent read from outside (a web page, a file, an email) that gets kept, and then steers every later run as something the owner said. A memory is a standing instruction, so a poisoned one outlives the page that planted it.

## The rule

The broker, never a model, decides whether a memory is kept automatically.

A model asked "is this safe to keep?" can be talked out of it by the injected text it's judging. A rule about what the conversation read can't be.

**Kept automatically** when all of these hold:
1. **The owner chose Automatic** (the setting, below).
2. **It's a chat run:** the owner was there, and the memory comes from what they said. A scheduled or background run's proposals go to the review queue, never kept automatically.
3. **The conversation is clean:** no run in this conversation, from its start to now, read anything from outside.
   - **Clean:** recalling memory, asking to remember, reading goals, listing schedules.
   - **Taints the conversation:** a web search or page, the browser, reading messages, a network request, the agent's computer (its files and commands), a skill, a subagent, a shared space, an artifact or a file sent in.
   - **Recalling memory or reading goals taints it too, where runs may write them.** The install grants runs neither, so they stay clean there. With a write grant, a background run that read a page could have left its words where a clean chat reads them.

   Once tainted, the conversation stays tainted until a new one starts.
4. **Under the day's cap:** at most 20 memories kept automatically a day. Past it, proposals go to the card as today.

**Otherwise** the proposal goes where it goes today:
- the card, for a chat run in Ask me mode or past the cap;
- the review queue (WO-054), for a tainted conversation or a background run. That's non-blocking: the agent isn't waiting for it, and the owner decides on the Memory page when they look.

**Unchanged:** corrections replace what they correct, retiring it but keeping it (WO-048). A memory denied or skipped isn't kept, automatically or otherwise: the broker checks the owner's denials before it keeps without a card. Each proposal is checked as today (its kind, its reasons, dates, its size).

## Threats, and what stops each

| Threat | What stops it |
| --- | --- |
| **Poisoning by injected text:** a page, file or email gets kept as the owner's word. | Kept automatically only from a conversation that read nothing from outside, as the broker's trail records it. One that did goes to the review queue, for the owner's eyes. |
| **Laundering through the conversation:** the agent read a page earlier, wrote its words into an answer, and proposes them in a later turn that used no tools. | The taint is the **conversation's**, from its start, not the turn's. |
| **Unattended writes:** a background run keeps things nobody saw. | Background and scheduled runs never keep automatically: their proposals go to the review queue. |
| **Runaway writes:** a looping or compromised agent fills the memory. | A daily cap of 20, counting those since replaced, so correcting in a loop can't pass it. Past it, every proposal is a card again. |
| **Laundering through what runs wrote:** a background run read a page and left its words in memory or a goal's tracking, and a clean chat reads them. | Where runs may write memory or goals, reading them taints the conversation too. |
| **The owner doesn't know what was kept.** | Each automatic memory is said in the chat ("Remembered: …"), its record says **kept automatically**, which chat and run, and when, and the Memory page lists and filters them, each forgettable in one click. |
| **A model hallucinates a fact about the owner** in a clean conversation. | It's kept, visible in the chat at once, and one click from forgotten. This is the same trade Claude Code makes. The record makes it findable. |

## Scope

**In scope:**
- **The broker:**
  - the conversation's taint, from the trail of each of its runs;
  - the mode (Automatic or Ask me, the owner's, kept in the owner directory beside the model's choice);
  - keeping a proposal at once when the rule holds, otherwise the card or the review queue;
  - the daily count.
- **The record:** `KeptRecord` says who kept it, the owner's approval or automatic, with the chat and run as today.
- **The agent:** `memory_propose` is told "kept" when it was kept at once, so it says "I'll remember that", not that it asked.
- **The app:**
  - the chat notice "Remembered: …" with a Forget link to the memory;
  - the Memory page's "Kept automatically" label and filter;
  - the setting, Memory: Automatic or Ask me, in Settings and on the Memory page.
- **Docs:** `local-api.md`, `session-protocol.md`, the CHANGELOG.

**Not in scope:**
- Automatic approval of anything else: sites, messages, money. Standing grants already cover sites.
- The memory loop (WO-054 slice 5), which builds on this rule.
- Judging a memory's content with a model.

## Wire shape (approved by the maintainer, 2026-11-27)

```text
UI schema v17:
  manage_memory
    + {"op": "mode"}                          -> {"result": "mode", "mode": "automatic" | "ask"}
    + {"op": "set_mode", "mode": "automatic"|"ask"}   the owner's alone
  a kept memory's record
    + kept_by: "owner" | "automatic"
  review queue items
    + kind "remember": a memory proposed in a tainted conversation or a background run

Session protocol v40:
  MemoryResult
    + Kept { id }       kept at once; the agent says it'll remember
```

## Default (the maintainer's decision, 2026-11-27)

**Automatic**, for a new install and for an existing one on upgrade. The upgrade says so once in the chat ("Memory is now automatic: …, change it in Settings"). With the conversation gate, nothing read from outside is kept without the owner, so the default gives up no security.

## Acceptance criteria

- [ ] In Automatic, a clean chat conversation's proposal is kept at once. The agent is told "kept", the chat says "Remembered: …", and its record says kept automatically, with chat, run and time.
- [ ] Once any run in a conversation read from outside, that conversation's proposals go to the review queue, through to the end of the conversation, even in turns that used no tools.
- [ ] A scheduled or background run's proposals go to the review queue.
- [ ] In Ask me, every proposal is a card, as today.
- [ ] Past 20 in a day, proposals are cards.
- [ ] A denied or skipped memory isn't kept automatically, and corrections retire what they replace.
- [ ] Only the owner sets the mode. The Memory page labels and filters what was kept automatically, and forgets in one click.
- [ ] Each slice has its evidence record, with hand-breaks of each rule above.

## Gates

All must pass before the WO closes:

- [ ] `bash scripts/check.sh` green on the pinned toolchain **and** 1.85.0.
- [ ] Real-cell tests ran rather than skipped (`HATCHLING_REQUIRE_CELL=1`).
- [ ] **Mutation, kill floor 1.0** on each slice's change.
- [ ] `cd frontend && npm ci && npm run check`.
- [ ] Wire types regenerated and committed.
- [ ] Docs updated in the same change.
- [ ] A real-model trial: a clean conversation keeps at once; one that read a page queues.

## Evidence

- Slice 1, the conversation's taint, the mode and keeping at once: [wo057-taint.md](../evidence/wo057-taint.md).
- Slice 2, a memory that waits on the owner's Review: [wo057-routes.md](../evidence/wo057-routes.md).
- Slice 3, the app: [wo057-app.md](../evidence/wo057-app.md).

## Notes

### Where the taint lives (found 2026-11-27)

As built in slice 1:

- **A broker-wide table of the chat's runs** (`conversation_taint`), as the chat's follow-ups are: the chat registers each run it starts with its conversation's taint, and the run's session marks it the moment it asks for an outside tool, before it's served. A run not in the table is never clean.
- **The chat holds the conversation's flag** and takes each run's mark into it when the run ends, answered or not. The conversation store's index keeps it across a restart.
- **The mode file** is `memory-mode.json` in the owner directory. Absent is Automatic; one that doesn't read is Ask me.

Found while designing:

- **The main chat's conversation.** The broker serves one chat, `local`. Side chats are the app's alone until #735 and 0.13. So "the conversation" is the conversation store's, and a new conversation (`start_new`) clears its taint with everything else.
- **A sealed flag in the store's index** (`#[serde(default)]`, so older stores read as clean). The Library already keeps a sealed per-conversation number that a new conversation clears, and that's the pattern.
- **Set when a run ends, kept or not:**
  - The run record (WO-053) carries a failed run's doings to the next run, so a run that read a page and then failed must still taint the conversation.
  - The flag is set for the conversation the run was sent in, never one the owner has started since.
- **The run's own uses, as the trail counted them:** any use of an outside tool, however it ended. A refused web page read nothing, but a failed request's error text can carry words. Counting every use is the conservative choice.
- **Files sent in taint at hand-over.** The trail can't tell an inbox read from other use of the agent's computer, so the chat marks the conversation when it gives a run the inbox's notes.


### Slices

1. **The conversation's taint and the mode,** in the broker, with automatic keeping, the record's `kept_by`, the daily cap, and `Kept` to the agent (session v40), so it never says it asked when it didn't. Until slice 2, everything else is a card, as before.
2. **The routes:** the review queue for tainted conversations and background runs (UI v17, the review kind `remember`), with the owner's decision applied as it's made: approved, kept on their word; rejected, its words refused from then on.
3. **The app:** the chat notice's Forget link, the Memory page's label and filter, and the setting. They come after slice 2's UI v17, so the record's `kept_by` and `manage_memory`'s `mode` and `set_mode` are UI v18: the approved shape, one version on.

### Found building slice 3 (2026-11-27)

- **The Forget link needs the memory's id.** The approved shape has no way for the chat to know which memory a notice is about: the UI renders notices as plain text, and matching their words would break when the wording changes. So the notice carries it, `remembered` on the `chat_message` (v18, optional, the listed id), set only by the broker on its own notice. It's a field beyond the approved shape, so it needs the maintainer's yes before slice 3 merges.
- **Said once, as it happens.** The upgrade's notice ("Memory is now automatic …") comes with the first memory kept automatically, and keeping it writes Automatic as the owner's choice, so it isn't said again. A new install hears it the same way, at the moment it matters.
