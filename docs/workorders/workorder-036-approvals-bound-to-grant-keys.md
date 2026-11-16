# WO-036: Approvals bound to grant keys

**Status:** QUEUED
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
- **A pipeline is covered only if every program in it is.**
- **A call that carries a warning** (such as `rm -rf`, `sudo`, `curl | sh`) is never covered by a standing grant: it asks every time. The broker decides which calls carry one, by a list it keeps; never the model.
- An approval stays bound to the exact request shown: a standing grant covers a later request only when its grant key is the same, and the request is shown in Activity either way.
- Sites' existing per-task and always choices move onto the scheme, with what's kept today carried over.

**Not in scope:** which tools ask at all (each tool's own work order decides that); taint signals on approval cards (a later WO).

## Acceptance criteria

- [ ] Every approval card shows the grant key it would make and offers only the scopes its kind allows.
- [ ] A request whose grant key a standing grant covers runs without a card, and is in Activity with the grant that covered it.
- [ ] A pipeline with one uncovered program asks, naming it.
- [ ] A call carrying a warning asks every time, whatever is granted; the list of warnings is the broker's, and a test proves an agent can't add to or remove from it.
- [ ] The owner lists standing grants with their scope and age, and revokes any one alone; a revoked grant covers nothing from then on, a running task included.
- [ ] A 24-hour grant stops covering anything 24 hours after it was made, by the broker's clock.
- [ ] Sites kept as always today are kept, as always grants on the scheme.
- [ ] Each guard is hand-broken and the test that names it fails for the right reason.

## Gates

As [TEMPLATE.md](TEMPLATE.md), with the frontend and protocol gates (the card and the grants page change).

## Evidence

Filled in at close.

## Notes

Follows #474 (fix(approvals): run only what the owner was shown), which established that each kind binds today and closed its two gaps.
