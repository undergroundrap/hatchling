# WO-032: Ideas and builds from the chat

**Status:** IN-PROGRESS
**Area:** `crates/hatchling-agent` (the tools, the config), `crates/hatchling-broker` (`scoped_config`, `artifact_ops`, the run's environment, the config), `crates/hatchling-adversarial` if the run fixture's environment changes, `docs/`
**Issue:** none
**Trust boundary:** yes

## Problem

WO-027 delivers a proposed build to the owner, who decides it in the app, and the broker runs exactly what was approved. Nothing the model does can reach it:

- The agent's model loop has no tool that captures an idea or proposes a build. Only a session that sends the op itself reaches an approval, and only the end-to-end test does.
- Every run's session, chat runs included, has artifacts off (`scoped_config`).
- A builder is named by its exact skill id, and a run can't list skills, so a model couldn't name one even with a tool.

So the approvals WO-027 built are never asked for in use.

## Threats

- **An agent steered by something it read builds or delivers without the owner seeing it.** A chat run reads pages, search results and memory, any of which can carry instructions. If its session could `build` or `deliver`, a page could have it write into the operator's workspace or send on a channel. So a chat run proposes, and the owner decides: it gets no op that builds, delivers or activates on its own say.
- **A run picks a builder the operator didn't mean for that kind.** Any skill declaring `artifact.build` passes the skill gate. A run proposes only with the builder the operator named for the kind.
- **A run records its own words as the owner's.** An idea's origin says where it came from, and `chat` and `command` say the owner. A run claiming either is refused: what a run captures is its own.
- **An unattended run asks the owner.** A scheduled run has no owner there to decide, and its proposals would only queue and lapse. Scheduled runs keep artifacts off, as they wait for no site (WO-028).

## Scope

**In scope:**
- An operator setting naming one builder per build kind, `HATCHLING_ARTIFACT_BUILDERS` (`kind=name@version`, comma-separated). Each kind must be registered and each builder a skill id; a kind named twice is refused.
- A chat run's session gets the broker's build store with ideas only:
  - capture, list, refine and note ideas, and propose an activation;
  - `build`, `deliver`, `describe` and `build-idea` are `denied`. `activate` stays as it is: it needs the confirmation only the owner's decision mints.
  - a proposal must name the operator's builder for its kind, or it's refused as an unknown builder;
  - an idea's `chat` or `command` origin is refused.
- The chat run is told the operator's builders (`HATCHLING_RUN_BUILDERS`, the same form), and only when it's a chat run.
- Agent tools, registered when the run was told builders:
  - `idea_capture`, `idea_list`, `idea_refine`, `idea_note`;
  - `build_propose`: an idea, a kind from the run's builders, and what it's for. The tool fills in the builder. It answers that the owner decides in the app, and with the broker's reason when the owner wasn't asked.
- Tests at each layer, and the browser e2e driven by the model's tools rather than a hand-sent op.

**Not in scope:**
- scheduled runs and subagents, which keep artifacts off;
- the long-lived agent session's ops, which are unchanged;
- telling the run what the owner decided. The broker tells the owner (WO-027); a later chat turn sees the idea's state through `idea_list`;
- changes to the session protocol.

## Acceptance criteria

- [ ] `HATCHLING_ARTIFACT_BUILDERS` is parsed strictly: an unknown kind, a bad skill id, a kind named twice, and any of them set without a build store are refused at startup. Tested.
- [ ] A chat run's session can capture, list, refine and note ideas and propose an activation, and is denied `build`, `deliver`, `describe` and `build-idea`. A scheduled run's session is denied every artifact op. Tested on `scoped_config` and on a served session.
- [ ] A chat run's proposal naming any builder but the operator's for its kind is refused, and one naming the operator's reaches the owner's board. Tested.
- [ ] A chat run's `capture-idea` with a `chat` or `command` origin is refused. Tested.
- [ ] Only a chat run is told the builders: a scheduled run's and a subagent's environments carry no `HATCHLING_RUN_BUILDERS`. The agent refuses a malformed value. Tested.
- [ ] The agent registers the five tools only when it was told builders. `build_propose`'s kind is one of them, and the op it sends names the operator's builder. Tested against a scripted broker.
- [ ] In CI's browser job, a chat run whose scripted model calls `idea_capture` and `build_propose` puts a card in the app, naming the operator's builder. The owner allows it, and the broker's outcome is told in the app as Hatchling's notice. (That an approved activation builds exactly what was proposed, with real builder cells, is WO-027's test.)
- [ ] A hand mutant for each rule above fails a test at its intended check.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.
