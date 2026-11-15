# Evidence: an idea names the goal it comes from and what it needs

What was tested, how, and what came back, for a change to what the
broker keeps on the owner's Ideas page: an idea must say which tools it
needs and which open goal it comes from, the broker checks both, and a
run adds at most three. Every number here is read from a file in the
results bundle, which is attached to the release that carries this
change, unless it says otherwise.

| | |
|---|---|
| Base commit | `2534fe51` (`test(agent): look for all of the leaked secret's number, not four digits of it (#462)`) |
| Tested tree | `d564dce63b841aadc0cc73277fc86f9302f2dc38` |
| Date | 2026-11-15 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox; the app's check on Windows 11 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-ideas-cards-wire-results.tar.gz`, 1780105 bytes, sha256 `a1844bdc4a3eb806cd2ac02bba2ba4f3c2ebb94253bda2835d69b3c620e64d6a` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

On an install of main `19ab6890`, with two local models, six ideas were
seen on the Ideas page or put there:

- **Two the install couldn't do.** "Vegetarian recipe search" on an
  install with no web search, and "Write small Rust/Go/Python binaries"
  for a computer with no compilers.
- **Four at once.** Sent the text of one idea in a chat, the agent
  answered by putting four more on the page.
- **None with anything behind it the broker could find.** Each leaned
  on the same two remembered facts, in the agent's own words.

The page's run was told "up to three" and "don't suggest what Hatchling
can't do here". Nothing held it to either, and a chat's run wasn't told
a number.

## What changed

- **The session protocol is v27** (`hatchling-core`): `SuggestOp::Suggest`
  gains `needs`, the tool groups doing the idea would use, and `from`
  (`SuggestedFrom`), where it comes from: one of the owner's open goals,
  by its id. Both are required.
- **The kept idea** (`Suggestion`, in the app's protocol) gains `needs`,
  `source` (`SuggestionSource`: the goal's id and its title as the
  broker read it) and `run`, the suggesting run as the owner's Activity
  names it. Each is left out when empty, so a page kept before is read,
  and written, as it was.
- **The broker** (`suggestions.rs`):
  - `here` reads what this install has, group by group, from its
    settings, and `unmet` refuses an idea that needs a group it lacks,
    or names one twice, saying which.
  - `goal_found` looks the goal up in the owner's goals, only under the
    asking run's own grant to read them, and `sourced` refuses a goal
    that isn't there, is closed, may not be read or couldn't be read.
  - A run that has put three ideas on the page is refused a fourth.
  - `serve.rs` hands the check the session's reader and its id.
- **The agent** (`hatchling-agent`): `idea_suggest` takes `needs` and
  `from`, and its description, the note about what the agent can do,
  and the task given to a run for the page all say that an idea comes
  from an open goal and that none is a good answer.
- `docs/session-protocol.md`, the changelog, and the app's generated
  protocol types.

## What the checks are, and aren't

- **Where an idea comes from is the broker's finding, not the agent's
  word.** The card keeps the goal's id and the title the broker read.
  The agent's `why` is still its own words, shown as such.
- **A run reads no goal it couldn't read already.** The lookup is made
  only for a run that holds the grant to read the owner's goals, and a
  run without it is told so, whether the goal is there or not.
- **What the agent remembers is no source.** The broker has nothing to
  look a remembered thing up by, so there is no way to say it. With no
  open goal there are no ideas.
- **What an idea needs is what it says it needs.** The broker checks
  the groups an idea names against the install. An idea that leaves a
  need out is kept. This stops the mistake that was seen, an honest
  "search" on an install with none, and not a model that says less
  than it will use.
- **The install is the measure, not the run.** An idea is started by
  the owner in a chat of their own, so its needs are held to what the
  install has, and to no run's grants.
- **Three is per run.** It stops one run filling the page. It is not
  what keeps the page short.

## Tests

Eleven new, and others widened.

- **The wire** (`hatchling-core`, `messages::room_tests`, one): the op
  with its needs and its goal, in its written form; an op with no
  needs, no source, a need or a source that isn't one, or a field it
  doesn't name never decodes; and the protocol is 27.
- **The kept idea** (`ui::tests`, one): an idea kept before is read
  with none of the three and written as it was; a new one carries all
  three, in their written form.
- **The broker** (`suggestions::tests`, six): an idea is kept only if
  the install has each thing it needs, named once, with each lack said
  in its own words; only if its goal is found open, with the card
  keeping the broker's finding; three a run and no more, whatever
  became of them, with another run's three its own; the page's order;
  what the install has, each group from its own setting; and, against
  a real goals store, a goal found open, closed, absent, hidden from a
  run without the grant, another person's, and unreadable, and an idea
  asked through the broker's own door.
- **The agent's tool** (`tools::tests`, three): its name, description
  and schema; what it asks the broker and what it tells the model; and
  that it asks nothing for an idea with no needs or no goal in the
  tool's terms.
- **Widened**: the page's older tests ask as runs of their own, on an
  install with everything; the page run's task test; the note's test;
  the Activity's test and the agent's own ask of the broker carry the
  new fields; the two older wire tests and the app's protocol test
  expect version 27.

## Found on the way

- **A sort that sorted nothing.** The first design listed ideas with a
  source ahead of those without, by sorting the page when it was read.
  Broken by hand, on tree `b187795d…`, the sort's absence changed no
  test: every idea kept now has a source and goes on top, so the page
  as it is kept is already in that order. The sort was taken out, the
  test now says what holds and checks each new idea's source, and the
  protocol document and the changelog say the page is listed as kept.
- **A check that ran against a moving tree.** The app's check failed
  once because the branch was switched under it while it ran. Run
  again on the tested tree, it passed. Nothing in the bundle is from
  the failed run.
- Two earlier runs of the chain. The first, on tree `b187795d…`,
  stopped at the breaks, with the sort's surviving. The second, on
  tree `3f3d40ad…`, before two other changes were merged under this
  one, had the results below. Every run in the bundle is of the
  tested tree.

## Breaks by hand

Forty-two, each one part of the change taken out or bent, the tests of
the three crates run on each, and each failing test named
(`unit-breaks.log`, `unit-*.log`, `ic_break.py`). All forty-two were
caught, and the tests passed before and after.

| Break | Caught by |
|---|---|
| An idea need not say what it needs | the wire's test |
| A goal source has another name on the wire | the wire's test; the tool's asking and asks-nothing tests |
| The protocol is still 26 | the wire's test; the two older wire tests |
| A kept idea always writes its needs | the kept idea's test; the app protocol's pinned test |
| A page kept before runs were can't be read | the kept idea's test; the pinned test; the order test |
| A kept source says its kind another way | the kept idea's test |
| What an idea needs isn't checked | the needs test; the store test |
| A need may be named twice | the needs test |
| An idea is refused for what the install has | the needs test; the store test |
| No search is said as no browser | the needs test |
| What it said it needs isn't kept | the needs test; the store test |
| An idea is kept whatever goal it names | the goal test; the store test |
| The card shows the agent's word for the goal | the goal test; the store test |
| Where it came from isn't kept | the goal test; the store test; the order test |
| A run reads goals it may not read | the store test |
| A closed goal is found open | the store test |
| Another goal is found for the one named | the store test |
| A closed goal is said not to be there | the store test |
| A run may add four | the cap test |
| The fourth is kept | the cap test |
| Three is for every run together | the cap test; the order test; the page's bound test |
| The run isn't kept on the card | the cap test; the store test; the order test |
| Reading pages is read from search | the install test |
| Every install has a Room | the install test; the store test |
| No install has a browser | the install test |
| Every install has goals | the install test |
| A run with no store has scheduled work | the install test |
| The door says there is always a Room | the store test |
| The door drops the run | the store test |
| The door finds every goal open | the store test |
| The page's run is sent to memory for ideas | the task's test |
| The page's run isn't told none is an answer | the task's test |
| The tool need not be given needs | the tool's description test |
| The tool need not be given a goal | the tool's description test |
| The tool can't say it needs the Room | the tool's description test; its asks-nothing test |
| An idea with no needs given is asked for | the tool's asks-nothing test |
| The goal is read from another field | the tool's asking test; its asks-nothing test |
| The tool doesn't say an idea comes from a goal | the tool's description test |
| The tool doesn't say three a reply | the tool's description test |
| The tool doesn't say none is an answer | the tool's description test |
| The tool still points at what it remembers | the tool's description test |
| The note doesn't say an idea names a goal | the note's test |

## Mutation

`cargo mutants --in-place --in-diff` on the change to each crate's
source (`mut.log`, `mut-*-outcomes.json`):

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 0 | 0 | 0 | 0 | 0 |
| `hatchling-agent` | 6 | 5 | 0 | 0 | 1 |
| `hatchling-broker` | 28 | 23 | 0 | 0 | 5 |

Core's change is types and a constant, with nothing in it to mutate;
`mut-core.log` says so.

Each crate's `outcomes.json`, by sha256:

- agent: `daabf7284f00cfe296cf8e9d43d0aafee119e4215c3bc6fd732c0c01110b1fe1`
- broker: `3fbf29cf695ead3c4b8ad3f32e48ff19673e402eccd01f8116be43ffa955b958`

The unviable ones:

- `tools.rs:558:9: replace <impl Tool for IdeaSuggestTool<'_>>::execute -> Result<ToolOutput, LoopError> with Ok(Default::default())`: unviable, build failure
- `suggestions.rs:92:5: replace here -> BTreeSet<ToolGroup> with BTreeSet::from_iter([Default::default()])`: unviable, build failure
- `suggestions.rs:149:5: replace goal_found -> GoalFound with Default::default()`: unviable, build failure
- `suggestions.rs:201:5: replace suggest -> SuggestResult with Default::default()`: unviable, build failure
- `suggestions.rs:244:5: replace sourced -> Result<SuggestionSource, &'static str> with Ok(Default::default())`: unviable, build failure
- `suggestions.rs:264:5: replace suggest_from -> SuggestResult with Default::default()`: unviable, build failure

## The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3926 passed, 0 failed | exit 0: 135 binaries, 3926 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

The tests of what the change touches were run as root too, as CI runs
them: 199 in core, 184 in the agent and 10 in its loop, 25 in the broker
(`root/as-root.log`).

The app's own check (`npm run check`: types, lint, 494 unit tests, and
coverage of the live adapter at 100%) passed on the same tree, on
Windows (`frontend-check.log`). The app's code didn't change: only its
generated protocol types and the version its test expects.

## What wasn't tried

- **The session's own door.** `serve.rs` hands the check the session's
  reader and its id. That is read from the code: no test at the
  session's level shows the id a card keeps is the one the owner's
  Activity shows for that run.
- **Any model against the tool.** Whether a small model reads the
  owner's goals first, copies a goal's id as it is shown, says what an
  idea needs, and suggests nothing when there is no goal, isn't known.
- **An install.** No idea has been suggested on one since this change.
  The six seen before it would each be refused now: none named a goal.
- **The app.** It shows an idea's source and run nowhere yet, and still
  groups ideas by area: the broker's order is the newest first within
  each area.
- **An install with no goals.** There the page stays empty, by design.
  What the app says of an empty page wasn't looked at.
- **A goal closed after its idea was kept.** The idea stays on the
  page, with the goal as it was found then. Nothing takes it off yet.
