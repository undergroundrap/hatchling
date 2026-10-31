# WO-032: Ideas and builds from the chat

**Status:** DONE (2026-10-31)
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

- [x] `HATCHLING_ARTIFACT_BUILDERS` is parsed strictly: an unknown kind, a bad skill id, a kind named twice, and any of them set without a build store are refused at startup. Tested.
- [x] A chat run's session can capture, list, refine and note ideas and propose an activation, and is denied `build`, `deliver`, `describe` and `build-idea`. A scheduled run's session is denied every artifact op. Tested on `scoped_config` and on a served session.
- [x] A chat run's proposal naming any builder but the operator's for its kind is refused, and one naming the operator's reaches the owner's board. Tested.
- [x] A chat run's `capture-idea` with a `chat` or `command` origin is refused. Tested.
- [x] Only a chat run is told the builders: a scheduled run's and a subagent's environments carry no `HATCHLING_RUN_BUILDERS`. The agent refuses a malformed value. Tested.
- [x] The agent registers the five tools only when it was told builders. `build_propose`'s kind is one of them, and the op it sends names the operator's builder. Tested against a scripted broker.
- [x] In CI's browser job, a chat run whose scripted model calls `idea_capture` and `build_propose` puts a card in the app, naming the operator's builder. The owner allows it, and the broker's outcome is told in the app as Hatchling's notice. (That an approved activation builds exactly what was proposed, with real builder cells, is WO-027's test.)
- [x] A hand mutant for each rule above fails a test at its intended check.

**Then (later slices):**

- Check at startup that each named builder is installed and declares `artifact.build`. Today a builder that isn't installed is found only when an approved build runs, and the owner is told it wasn't built.
- Tell the run what the owner decided (WO-026's typed outcome events), beyond `idea_list`.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Built in #257. It was gated in the sandbox on real cells, with a delegated cgroup:
- fmt, and clippy on the workspace (`--all-targets --all-features`) with no findings on 1.98.1 and 1.85.0;
- with `REQUIRE_CELL=1`, on both toolchains: `hatchling-agent --lib`, `hatchling-broker --lib`, `--test artifacts`, `--test schedule_run`, the run fixture, `hatchling-dev` and core's `builders::`;
- preflight;
- a hand mutant for each rule, each read for failing at its intended check;
- then CI, browser job included.

CodeQL flagged the first push: a config test's assert messages could print a parsed `Config`, which holds the operator's secrets. They now print only the refusal.

- **The operator's builders are read strictly** (`the_builders_are_read_strictly_and_only_with_a_store`, `crates/hatchling-broker/src/config.rs`): an unregistered kind, a builder that isn't `name@version`, a kind named twice, and a pair that isn't `kind=…`, each refused naming the setting, and any builders without `HATCHLING_ARTIFACT_DIR`. The form's own rules are core's (`crates/hatchling-core/src/builders.rs`): a list reads back as written, and each malformed pair is refused naming itself.
- **A chat run's session has ideas alone:**
  - `scoped_config` gives a chat run the store with `ideas_only`, no workspace and no review policy; a scheduled run, and a chat run with no builders or no store, get it off (`only_a_chat_run_gets_ideas_and_is_told_the_builders`, `crates/hatchling-broker/src/run_launcher.rs`);
  - the gate, op by op, in a session with ideas alone and one with the whole store (`a_session_with_ideas_alone_proposes_and_nothing_more`, `crates/hatchling-broker/src/artifact_ops.rs`);
  - with real builder cells (`a_chat_runs_proposal_reaches_the_owner_and_builds_nothing_on_its_own`, `crates/hatchling-broker/tests/artifacts.rs`): a chat run's session is denied `build`, `deliver`, `describe`, `build-idea` and an idea said to be the owner's, and a builder that isn't the operator's is `unknown-builder`. Its proposal with the operator's builder reaches the owner's board, and the owner's approval builds it. A scheduled run's session is denied a capture;
  - in real run cells (`only_a_chat_run_is_told_the_builders_and_keeps_ideas`, `crates/hatchling-broker/tests/schedule_run.rs`): a chat run captures an idea through its own session; a scheduled run is denied.
- **Only a chat run is told the builders:** the run's environment carries `HATCHLING_RUN_BUILDERS` only for a chat turn (`only_a_chat_turn_is_told_the_builders`, `crates/hatchling-broker/src/cell.rs`), and the real cells above report it. A subagent's run is never a chat run, and is built with no builders. The agent refuses the variable outside a chat turn, or malformed (`only_a_chat_turn_reads_the_builders`, `crates/hatchling-agent/src/config.rs`).
- **The agent's tools** (`crates/hatchling-agent/src/tools.rs`, against a scripted broker): offered only with builders (`the_idea_tools_are_offered_only_with_builders`); each sends its op, a capture claims no source, and a proposal names the operator's builder (`each_idea_tool_sends_its_op_and_a_proposal_names_the_operators_builder`); a kind with no builder sends nothing (`a_proposal_of_a_kind_with_no_builder_sends_nothing`); each refusal is told to the model as text (`each_idea_refusal_tells_the_model_why`).
- **From the chat to the owner, in CI's browser job** (`scripts/chat_build_end_to_end.{sh,py}`, `scripts/idea_model.py`). The owner asks in the app; a scripted model calls `idea_capture`, then `build_propose`. The card shows the build kind and the operator's builder, the idea by its length and hash, and the agent's words as the agent's. Allowed once, the card goes, and the broker's outcome is Hatchling's notice, never the agent's text. No builder is installed there, so the notice says it wasn't built.
- **15 hand mutants**, each failing at its intended check:
  - the gate removed: a `chat` claim is saved;
  - `chat` and `command` allowed: the gate test;
  - any builder accepted for a named kind: `unknown-builder` expected;
  - `describe` allowed: `unknown-build` instead of `denied`;
  - every run given ideas: the scheduled real-cell run;
  - the workspace kept: `scoped_config`;
  - builders told with no store: `scheduled_run`;
  - the variable sent to every run: the environment test;
  - an unregistered kind accepted, and builders accepted without a store: the config test;
  - the agent reading builders outside a chat: the agent's config test;
  - the tools offered with no builders: the tools test;
  - a capture claiming `chat`, and a builder not the operator's: the op the scripted broker read;
  - a builder with no version: core.
