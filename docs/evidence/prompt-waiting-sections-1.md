# Evidence: the sections waiting for their runs, the worker and background runs (#714)

| | |
|---|---|
| Base | `main` at `6275fa17` (#712), with #713 under it |
| Date | 2026-11-26 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |
| Issue | [#714](https://github.com/undergroundrap/hatchling/issues/714), its first part |

## What it does

It adds 40 sections to the prompt pack for the runs Hatchling doesn't have yet: the worker runs, and the background runs' sections. None of them reaches any prompt. Each one is pinned word for word with its domain, and it sits on the waiting list.

A new check holds them out: a section on the waiting list must be written, and no kind may carry it. When its run is built, that run's pull request adds the kind, routes the sections and takes them off the list. No prompt changes today, so the changelog has no line for this.

| Run | Domain | Sections | Bytes |
|---|---|---|---|
| The browser worker | `product:browser (new)` | `browser-identity`, `browser-challenges`, `browser-purchasing`, `browser-security`, `browser-awareness`, `browser-reporting` | 9784 |
| The follow-up after a background result, and its quiet form | `product:handoff (new)` | `handoff-delivery`, `handoff-silence`, `handoff-identity`, `handoff-memory`, `handoff-credentials`, `handoff-tools` | 6225 |
| A lite page builder and editor | `product:spaces` | `spaces-lite-authoring`, `spaces-lite-builder`, `spaces-lite-editor` | 6199 |
| A device sync decision | `product:sync` | `sync-device-decision` | 833 |
| An event hook | `product:hooks (new)` | `hooks-how-it-works`, `hooks-tools` | 2268 |
| An artifact inspector | `product:artifacts` | `artifacts-inspector` | 962 |
| Self-improvement proposals | `shared` | `improvement-proposals`, `improvement-authority` | 1348 |
| A media upload decision | `product:sync` | `sync-media-decision` | 928 |
| A page's task | `product:spaces` | `spaces-task-actions`, `spaces-task-completion` | 3837 |
| The nightly alignment run | `product:alignment (new)` | `alignment-reflect`, `alignment-judge`, `alignment-distill`, `alignment-repair` | 5705 |
| Goals bookkeeping and pulses | `product:goals (new)` | `goals-evaluate`, `goals-pulse`, `goals-ledger` | 5866 |
| People's and groups' records, for alignment and memory | `shared` | `person-records` | 464 |
| A group chat | `product:channels` | `provider-groups` | 918 |
| Browser routing, for the main agent and for a subagent | `product:browser` | `browser-routing-root`, `browser-routing-subagent` | 1531 |
| Memory reconciliation | `product:memory` | `memory-recall`, `memory-write`, `memory-reconciliation`, `memory-injection-defense`, `memory-grounding` | 7657 |

The pack goes from 40 sections to 80. Five domains are new: `product:browser`, `product:handoff`, `product:hooks`, `product:alignment` and `product:goals`. A short-video builder's sections were left out on purpose.

## Adapted to Hatchling

The sections keep their wording. These lines named a mechanism Hatchling does differently, and each changed only where it does:

| Section | Was | Now | Why |
|---|---|---|---|
| `browser-purchasing` | Otherwise, recommend the wallet's virtual card for the merchant's standard card form. | Otherwise, ask your parent agent which card the owner keeps in the vault for this; a card is filled from the vault, never typed by you. | Hatchling has no wallet or virtual card; the vault holds what's used, and the agent never types a card |
| `browser-purchasing` | If the user declined both the wallet and takeover, finish as failed | If the user declined both the vault's card and takeover, finish as failed | the same: no wallet |
| `browser-awareness` | Use the date, time, and user timezone supplied in the latest task message. | Use when your run was told it started, and the date, time, and timezone the latest task message supplies when it gives them. | every Hatchling run is told when it started (#712) |
| `browser-awareness` | Include a weekday only when the task or page supplies it with the date. | Include a weekday only when the task, your run's start, or the page supplies it with the date. | the run's start carries a computed weekday (#712) |
| `handoff-credentials` | A credential the user pasted in chat follows the transient-use rule. | A credential the user pasted in chat isn't passed on: ask them to keep it in the vault, where the task can use it. | vault-only on credentials (maintainer, 2026-11-26): no transient use of a pasted credential |
| `handoff-credentials` | Pass card details the user supplied in chat to the task whose checkout asked for them, with one short sentence saying the user chose this card for this checkout. | Card details the user pasted in chat aren't passed to a checkout: ask them to keep the card in the vault, which fills it for the checkout. | vault-only: a card number never passes through the agent |
| `spaces-lite-editor` | named another product's computer, and a phone webview as the rendering surface | Your computer is the Room, your own machine inside your cell. Your rendering surface is the Hatchling app's view of the page, on a desktop or a phone. | Hatchling's agent computer is the Room, and the page is shown in its app |
| `hooks-tools` | documents the user asked for go under your files directory, or the goal's files directory when they support an active goal. Only documents the user asked for belong there. Transient or per-run reports the task did not ask for do not go in those directories: | documents the user asked for are handed to the Library, named for the goal when they support an active goal. Only documents the user asked for belong there. Transient or per-run reports the task did not ask for are not handed over: | Hatchling delivers documents through the Library, not a files directory |
| `hooks-tools` | Goal working state (scan output, run logs, retry evidence, screenshots) goes under the goal's hidden files instead. | Goal working state (scan output, run logs, retry evidence, screenshots) stays in your workspace, in a folder named for the goal. | goals are broker records, with no files of their own |
| `hooks-tools` | Do not create new files or directories at the home root. | Do not create new files or directories at your workspace's root. | the Room's workspace, not a home directory |
| `memory-reconciliation` | a new fact, preference, boundary, or commitment goes into `~/MEMORY.md` only, and only when it is durable enough to belong in the compact projection. `~/USER.md` gets cleanup only: | a new fact, preference, boundary, or commitment goes into `MEMORY.md` only, and only when it is durable enough to belong in the compact projection. `USER.md` gets cleanup only: | the standing files are the broker's (#708), not files in a home directory |
| `memory-reconciliation` | Remove the superseded meaning while preserving any still-true substance from the old entry. | Remove the superseded meaning while preserving any still-true substance from the old entry. Replacing or removing a preference, a boundary, a commitment, or a fact about who the user is goes to the owner's review queue instead of being applied: propose it with its evidence, and carry on with the rest. | maintainer, 2026-11-26: routine memory writes run under the run's own checks; destructive ones go to a review queue that never blocks the run |
| `person-records` | files under `~/memory/people/` and `~/memory/groups/`, listed in each directory's index. | pages in the owner's people and groups records, each listed in its index. | the people and groups records are #708's, not files in a home directory |
| `memory-write` | use the Secure Vault for supported credential access. | the vault holds credentials, and a credential is used only through the tool that holds it. | Hatchling's vault, and its rule that a credential is used only through its tool |
| `goals-ledger` | Goals the user owns in the Goals product are read-only exclusion evidence: | Goals the user set themselves are read-only exclusion evidence: | names Hatchling's goals as the owner's own, not another product's |

Kept as written, for review when the run is built:
- **The protected one-time-code flow,** and a code the owner types in chat passed once to the step that asked for it.
- **What doesn't exist yet:** the daily logs and standing files the follow-up reads (#708's), the lite builder's audit and taste check, and an artifact's published actions.
- **`memory-write`'s "write it down before you respond"** is shaped for a chat turn. In chat today the agent asks the owner to keep a fact (`memory-discipline`), so this is revisited when the memory run is built.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| clippy `-D warnings`, 1.98.1 and 1.85.0 | pass |
| the whole workspace's tests | 4546 passed, 0 failed (tree `435bc884`) |
| `hatchling-dev audit` | all checks passed |

Two new checks:
- **In `no_section_is_orphaned_and_no_route_dangles`:** every waiting section is written, and no kind carries it.
- **`every_sections_text_is_pinned`:** every section's text matches its pin word for word.

  Before it, a section's text was compared only through the prompts that carry it. The first gate's hand-break, which changed a waiting section's text but not its pin, passed. With this check, it fails.

**Hand-breaks**, each caught and the tree restored:
- `browser-security` routed into Chat: caught by the waiting check;
- a waiting section's text changed but not its pin: caught by the new text pin;
- `handoff-tools` taken off the waiting list: caught as orphaned;
- `memory-reconciliation` routed into Worker: caught by the waiting check.

**Mutation** on `hatchling-prompt`'s changed lines since `main`: no mutants. Everything since `main` is the pack's text and its tests, and `cargo mutants` finds nothing to mutate there.
