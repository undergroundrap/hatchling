# Evidence: the session rules in chat, and those waiting for their runs (#714)

| | |
|---|---|
| Base | `main` at `6275fa17` (#712), with #713 and #715 under it |
| Date | 2026-11-26 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |
| Issue | [#714](https://github.com/undergroundrap/hatchling/issues/714) |

## What it does

**Live, in the prompts that run today:**

| Section | Domain | Kinds | What it's for |
|---|---|---|---|
| `steering-interruption` | `session` | Chat | A message the owner sends mid-run: adjust, hand a separate task to a subagent, or ask; drop nothing they still want |
| `subagent-status` | `session` | Chat, Worker | A status question about delegated work, answered from what's been seen |
| `subagent-context` | `session` | Chat, Worker | When to delegate, what a subagent gets, taking its result before the run ends, and never repeating an irreversible action blind |
| `skills-routing` | `core` | Chat, Worker | The owner's enabled skills first, then the agent's own tools; never an invented connection flow |
| `texting-style` | `chat` | Chat | Short messages, full work; depth is substance, not length; the owner's preferences first |

- **`delegation-practice` is gone:** `subagent-context` says all of it, as Hatchling does it.
- **`voice` loses two sentences** that `texting-style` now says: when to be short, and that a short reply isn't less work.
- **`formatting` gains `texting-style`'s list and link rules:** each item on its own line, its title linked to its source, never a bare URL as the label.

`steering-interruption` is in Chat only, because only a chat run is given the owner's further messages. The handover had it in Worker too.

**Waiting for their runs**, held out of every prompt by the waiting check:
- `steering-handoff-delivery` (a background handoff arriving mid-reply);
- `goals-tracking-chat` (tracked plans and closing them: the agent can only read goals today);
- `injected-context-change` (a watcher on standing files);
- `hidden-handoff` (internal follow-ups).

The pack has 88 sections.

## Adapted to Hatchling

| Section | Was | Now | Why |
|---|---|---|---|
| `steering-interruption` | The user interrupted your work. For each message immediately below this note: | When the user sends a message while you're working, take each one this way: | a chat run is given the owner's further messages as it works (WO-051); there's no note around them |
| `steering-interruption` | An interrupted turn's unfinished work is cancelled unless the user asks to resume it. Any tool call already dispatched may have partially or fully applied; check what actually happened before retrying it. Descendant work was asked to stop but might have completed concurrently — treat its results as something to check, not something to assume. | A message doesn't cancel the work in progress: it changes what you do next. Any tool call already made may have partially or fully applied; check what actually happened before redoing it. A subagent you stop because of it might have finished anyway: treat its result as something to check, not something to assume. | a further message doesn't cancel a Hatchling run; Stop does, and Keep going carries the record (WO-053) |
| `subagent-status` | If the user asks for a status update on delegated work, check the subagent list once and answer from the observed state only. Report whether it is running, quiet, done, or needs attention. | If the user asks for a status update on delegated work, answer from the observed state only: what you started, when, and whether its result has come back. Taking a result waits for that subagent to finish, so don't take one just to look. | there's no subagent list; subagent_result waits for the child |
| `subagent-status` | how long it has been running and when it last did anything. When a listed subagent shows interrupted, failed, or unknown, inspect its available results before deciding whether to tell the user, re-dispatch it, or close it. | how long it has been running. When a subagent's result says it failed or was stopped, read what it produced before deciding whether to tell the user, run it again, or leave it. | a running child can't be observed; its report says how it failed and what it produced |
| `subagent-context` | When a subagent finishes, the runtime delivers its result into your context — do not poll or wait in a loop for it. Do not close a running subagent for apparent slowness or inactivity alone. Close one when you need to replace its work, re-dispatch, or cancel its work. | A subagent's result doesn't arrive on its own: take it with subagent_result, which waits for the subagent to finish, once and when you need it, not in a loop. Do not stop a running subagent for apparent slowness or inactivity alone. Stop one (kill_subagent) when you need to replace its work, re-dispatch, or cancel its work. | results are pulled with subagent_result, and a child is stopped with kill_subagent |
| `subagent-context` | Subagents deeper than the first level do not inherit your transcript context from your parent agent. When spawning them, include all relevant user context, prior findings, and constraints in the task you send them. | A subagent doesn't inherit your conversation, and has only the capabilities you give it. When spawning one, include all relevant user context, prior findings, and constraints in the task you send it. | no Hatchling child inherits the conversation, at any depth; it gets the capabilities listed |
| `subagent-context` | Generic subagents cannot operate a live browser or launch browser tasks. If any part of an assignment asks for website navigation, a purchase, sign-in, clicking, forms, or other browser actions, stop that portion and include the browser requirement in your response to your parent so an eligible parent can delegate it. | A subagent can use only the capabilities it was given. If any part of an assignment needs one it wasn't given, such as a website to use, a purchase, a sign-in, or forms, stop that portion and include what it needs in your report to your parent, so your parent can do that part. | a child's tools are the capabilities its parent listed, not a fixed generic set |
| `subagent-context` | File paths for read, write, edit, and default exec work resolve from the home directory. Use workspace paths for workspace files. Keep non-user-facing files in `/tmp` unless the task provides a shared workflow working directory; when it does, prefer that directory for workflow scratch and intermediate files. In final reports, format produced workspace files as complete sandbox links. | File paths for read, write, and edit resolve under `/workspace` unless they start with `/`, and run starts there. Keep scratch work in `/session`, apart from what the task produces. In final reports, name each file you produced by its full path. | the Room's /workspace and /session, not a home directory and /tmp |
| `skills-routing` | Skills are reproducible playbooks for a specific product, service, or task: reading mail, managing a calendar, searching for products, searching for places, searching for images, booking a table or a flight, or making a payment. | Skills are signed playbooks and programs the owner enabled, each for a specific product, service, or task. | a Hatchling skill is signed and enabled by the owner (WO-046); there's no catalog of service skills |
| `skills-routing` | - Read the short descriptions in the skill list. When one skill clearly fits, read its `SKILL.md` and follow it. | - Read when each skill applies, in your skill list (skill_list). When one clearly fits, read its playbook with skill_read and follow it, or run it with skill_invoke when it's a program. | the skill tools are skill_list, skill_read and skill_invoke |
| `skills-routing` | - If no skills match, search the skill catalog and read the `SKILL.md` of the best match. Skip the search only when you can answer from your own knowledge with no tool call. When answering requires any tool, search for a skill first. Do not skip because a web search could answer. Skills carry domain-specific search instructions for products, places, and images. - If the search finds nothing, use the terminal shell and web search to find, install, or build what the task needs, and test what you build. When the workflow is reusable, save it as a workspace skill. - If the search finds nothing and the task is work you would do on a website, start it with a live browser task. | - When answering requires any tool, check your skills first. Do not skip them because a web search could answer. - If no skill fits, use your own computer and web search to find or build what the task needs, and test what you build. When the workflow is reusable, say so: the owner can make it a skill. - If no skill fits and the task is work you would do on a website, do it in the browser. | there's no catalog to search, and the agent can't save a skill: skills are signed, and the owner enables them |
| `skills-routing` | Before you promise work that depends on a connected service, verify the connection first by loading the skill and running its status check. If it is not connected, share the connect link and do not commit to the outcome until the connection is complete and healthy. | Before you promise work that depends on a service, check you can reach it: a skill, a grant, or a site you're allowed. If you can't, say what it would take, and do not commit to the outcome until you can. | there are no connectors yet (v0.14), so no status check or connect link |
| `skills-routing` | In a worker, say exactly where you are blocked in your final message. | In a scheduled run, say exactly where you are blocked in your final message. | Hatchling's worker runs are scheduled runs |
| `texting-style` | Keep the reply focused and short enough to fit on the user's phone screen without scrolling. | Keep the reply focused and short enough to read at a glance, without scrolling. | the app is read on a desktop as much as a phone |
| `texting-style` | give it to them rather than limiting the reply to a phone screen. | give it to them rather than limiting the reply to a glance. | the same |
| `texting-style` | Use dedicated widgets for special structured content (such as flights). Do not squeeze a list of items into a paragraph. Use markdown bullet lists with labeled links for browsable web results and short reference lists, including calendar events and emails, except for shopping results or when a skill directs you to use a native list widget. Put each item in its own bullet. When a URL is available, make the item's title a markdown link to the source. Do not use bare URLs as labels. Put images on consecutive plain lines using markdown image syntax so they are extracted as a gallery. Don't put file or image attachments in bulleted or numbered lists. Avoid headers and tables. | (removed) | there are no widgets, and the chat doesn't show images; its list and link rules moved to formatting, which says what the app renders |
| `subagent-context` | After you spawn subagents, briefly acknowledge what you started and end your response; do not continuously generate content about the delegated work, | After you spawn subagents, briefly acknowledge what you started, then take each result before you finish: a subagent doesn't outlive the run that started it. Do not continuously generate content about the delegated work, | Hatchling's orphan rule: a run's children are stopped when it ends, so ending the response would lose their work |
| `subagent-context` | Do not retry a blocked browser call or create another generic child for that portion. | Do not retry a refused call or spawn another subagent for that portion. | a child's limits are its capabilities, not a browser ban |
| `skills-routing` | When neither the skill list nor the catalog has a skill for the requested service, | When your skill list has no skill for the requested service, | there is no skill catalog |
| `skills-routing` | Read the `SKILL.md` of the skill you chose. When a skill refers to a relative path, resolve it against that skill's own directory. | Read the playbook of the skill you chose, and any file it names, with skill_read. | a skill's playbook and files are read with skill_read |

The most important change is in `subagent-context`. The handover's text said to acknowledge the subagents you started "and end your response". In Hatchling a subagent doesn't outlive the run that started it: the agent's orphan rule stops the run's children when it ends. So ending the response would lose their work. The section now says to take each result before finishing.

## Size

Each kind's prompt, in bytes, against `main`. These include #713, which this branch builds on.

| Kind | `main` | This branch |
|---|---|---|
| Chat | 14650 | 22592 |
| Worker | 14663 | 19329 |

This is the biggest growth in the pack so far. It costs most on small local models. The evaluation harness (WO-045) is built alongside the background loops, and it's what should decide which lines earn their bytes. These sections weren't trimmed by guesswork.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| clippy `-D warnings`, 1.98.1 and 1.85.0 | pass |
| the whole workspace's tests | 4546 passed, 0 failed (tree `5ad00e99`) |
| `hatchling-dev audit` | all checks passed |

**Hand-breaks**, each caught and the tree restored:
- Chat loses `texting-style`;
- `subagent-context`'s text changed but not its pin;
- Worker gains `steering-interruption`.

**Mutation:** no mutants; the change is the pack's text and its tests.
