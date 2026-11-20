# WO-036: Approvals bound to grant keys

**Status:** CLOSED
**Area:** `crates/hatchling-broker` (`approvals`, `approval_board`, `site_grants`, the stores of standing grants), `crates/hatchling-core` (the confirmation's choices), `frontend/` (the approval card, and a page of standing grants)
**Issue:** [#478](https://github.com/undergroundrap/hatchling/issues/478)
**Trust boundary:** yes (what the owner's yes covers)

## Problem

An approval covers exactly the request the owner was shown, once (#474 checked this kind by kind and closed the two gaps it found). Beyond once, what a yes can cover is decided kind by kind: a site can be allowed for a task or always, by its origin; nothing else can be allowed past one use. As the agent gets more tools, the owner will be asked the same thing over and over, or be offered broad yeses that cover more than they meant.

The fix is one scheme for every standing yes: a **grant key** names what is allowed (a tool and its target, such as `shell:git` or `send_email:someone@example.com`), and a **scope** says for how long (this once, for this task, for 24 hours, or always). Each grant is listed and revoked on its own.

## Scope

**In scope:**

- A grant key of tool plus target, made by the broker from the request it freezes, never from the agent's words.
- Four scopes: once, for this task, 24 hours, always. The card offers only the scopes the request's kind allows.
- A store of standing grants, the owner's alone, with when each was made and how it was scoped, and a revoke for each.
- **A call that carries a warning** is never covered by a standing grant: it asks every time. The broker decides which calls carry one, by a list it keeps; never the model. This work order builds that rule; which calls carry a warning is decided with the first tool whose calls can (see Notes).
- An approval stays bound to the exact request shown: a standing grant covers a later request only when its grant key is the same, and the request is shown in Activity either way.
- Sites' existing per-task and always choices move onto the scheme, with what's kept today carried over.

**Not in scope:** which tools ask at all (each tool's own work order decides that); taint signals on approval cards (a later WO); the pipeline rule and the list of commands that carry a warning, which move to the work order of the first tool that asks to run a command (see Notes).

## Acceptance criteria

- [x] Every approval card shows the grant key it would make and offers only the scopes its kind allows. Slice 2b ([grants-v9.md](../evidence/grants-v9.md)): the ledger's one list of scopes, the broker's key; slice 3 ([warnings.md](../evidence/warnings.md)): a signed-in profile's card offers for the task alone, and a warned request once alone.
- [x] A request whose grant key a standing grant covers runs without a card, and is in Activity with the grant that covered it. Slice 2c ([covered.md](../evidence/covered.md)); a record that can't be kept means the owner is asked.
- [x] A request carrying a warning asks every time, whatever is granted: the warning is checked before any grant, the list of warnings is the broker's, and a test proves an agent can't add to or remove from it. Slice 3 ([warnings.md](../evidence/warnings.md)): sites' first warning, an address over 1,024 characters (the maintainer's choice, 2026-11-20).
- [x] The owner lists standing grants with their scope and age, and revokes any one alone; a revoked grant covers nothing from then on, a running task included. Slices 1b ([revokes-reach.md](../evidence/revokes-reach.md)) and 2b ([grants-v9.md](../evidence/grants-v9.md)).
- [x] A 24-hour grant stops covering anything 24 hours after it was made, by the broker's clock, and a clock set back never stretches one. Slices 1 ([grants-kept.md](../evidence/grants-kept.md)), 2a ([day-ends.md](../evidence/day-ends.md)) and 2b; a host's sleep doesn't stretch one either. A WSL clock lagging after the host slept can, by the lag: said in 2a's residual.
- [x] Sites kept as always today are kept, as always grants on the scheme. Slice 1 ([grants-kept.md](../evidence/grants-kept.md)).
- [x] Each guard is hand-broken and the test that names it fails for the right reason. Every slice's record lists its breaks: 15, 17, 17, 12, 9 and the close's.

## Gates

As [TEMPLATE.md](TEMPLATE.md), with the frontend and protocol gates (the card and the grants page change).

## Evidence

- Slice 1, the grants kept and the owner's always kept when chosen (#542): [grants-kept.md](../evidence/grants-kept.md)
- Slice 1b, a revoke reaching every running task: [revokes-reach.md](../evidence/revokes-reach.md)
- Slice 2a, a day grant's end reaching every running task: [day-ends.md](../evidence/day-ends.md)
- Slice 2b, approvals bound to grant keys and scopes, the local UI API v9: [grants-v9.md](../evidence/grants-v9.md)
- Slice 2c, what standing grants covered, kept and shown in Activity: [covered.md](../evidence/covered.md)
- Slice 3, the warning rule and a profile's card said truly, and the close: [warnings.md](../evidence/warnings.md)

## Notes

Follows #474 (fix(approvals): run only what the owner was shown), which established that each kind binds today and closed its two gaps.

**Scope, agreed with the maintainer on 2026-11-19, after the design's review:**

- **No tool that runs commands asks today.** `run` works inside the Room with no approval, by design, and there is no email tool. So the pipeline rule ("a pipeline is covered only if every program in it is") and the list of commands that carry a warning (`rm -rf`, `sudo`, `curl | sh`) have nothing to apply to. Splitting shell text without the real shape of the request is open to bypass (`bash -c`, `$(..)`, `xargs`, git aliases and hooks), so a list written now would give false confidence. They move to the work order of the first tool that asks to run a command, as acceptance criteria there. This work order builds the rule they plug into: a warning checked before any grant, from a list the broker keeps.
- **A signed-in profile is granted for its task only**, and its card says so: a longer grant would let a scheduled or injected run read signed-in pages unasked.
- **The kinds with standing scopes today are sites:** once, for this task, 24 hours, always. Every other kind is once.
- **Slices:**
  1. The broker: grants kept as keys and scopes, the owner's always kept when they choose it (#542), and revokes reaching a running task.
  2. The local UI API v9 and the app: the card's key and scopes, the grants page, and covered actions in Activity.
  3. Profiles said truly, the warning rule, and the close.
