# Evidence: keep an unfinished chat for later

What was tested, how, and what came back, for a change that lets the
owner keep what they asked in a chat whose run ran out of steps, as a card
on their Ideas page in their own words. Every number here is read from a
file in the results bundle, which is attached to the release that carries
this change, unless it says otherwise.

| | |
|---|---|
| Base commit | `16ee46a9` (`fix(model): a model server's error is asked again (#468)`) |
| Tested tree | `599252274a60dd333072d032e84b3d9f978457da` |
| Date | 2026-11-15 and 16 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox; the app's check and the app's breaks on Windows 11 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-keep-for-later-results.tar.gz`, 1798958 bytes, sha256 `08e6070529a9923c2ab71dc8433ebafd0bbbccbe881a48e42b3b1853c633450d` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it. In the
bundle's `app-breaks/` and `frontend-check.log`, the Windows machine's
paths are replaced with `<repo>` and `<scratchpad>`; nothing else in those
logs was changed.

## What was wrong

When a chat run ran out of steps, its answer was shown like any other,
and what the owner asked was left wherever it was in the chat. Starting a
new conversation clears what the chat kept, so the unfinished ask was
easy to lose. The broker knew how the run ended (the agent's exit status
80, since #466), and the owner wasn't told.

## What changed

- **The answer says so** (`hatchling-core`, `hatchling-channel`,
  `hatchling-scheduler`, `hatchling-broker`). `ChatMessage` gains
  `out_of_steps`, written only when true. The chat loop passes it to the
  delivery (`Delivery::deliver_answer` takes it; `Origin` carries it), and
  the app's chat sink puts it on the message. A scheduled run's answer
  never says it.
- **What was asked is offered, in memory** (`hatchling-channel`,
  `chat_run.rs`). When such an answer was shown to the owner in the
  conversation still open, the chat's turn offers the hub what they said
  in that exchange, trimmed and at most 400 bytes (`kept_words`). The hub
  holds the offer only for the conversation open, at most eight
  (`MAX_OFFERS`), the oldest going first. A new conversation, however it
  starts, withdraws them all; so does a restart.
- **The owner's keep** (`suggestions.rs`, `api_ops.rs`).
  `SuggestionOp::Keep { run }` makes a card from the offer: the broker's
  fixed title ("Pick up where a chat stopped") and why, the owner's words
  as what it would do, source `SuggestionSource::Chat`, and the run. It is
  put at the top of the page and the offer is withdrawn. Owner-only, as
  every `manage_suggestions` op is.
- **The app** (`frontend`). Under an answer in the owner's own chat that
  ran out of steps, the chat shows "Keep for later" as a choice. The press
  keeps the card and the choice shows it was made; the Ideas page is read
  again. A kept card says it is the owner's own words. The choice uses the
  app's existing choice message and its existing `message.choose` command,
  which the app's automation policy already marks human-only. No frozen
  file is changed.
- The session protocol is v30, for the app's sake; no session op changes.
  `docs/session-protocol.md`, `docs/local-api.md`, the changelog, the
  protocol schema and the app's generated types.

## What it is, and isn't

- **Nothing is kept unless the owner presses.** No model writes the card,
  and the agent has no op that keeps one.
- **The words are the owner's.** The card's title and why are the
  broker's fixed text; what it would do is what the owner asked.
- **The New conversation promise stands.** The offer lives in memory, for
  the conversation open, and goes when a new one starts or the broker
  restarts. What the owner keeps is on the Ideas page, by their press.
- **The agent's cap doesn't hold the owner.** Sixty ideas may wait for the
  agent's suggestions; a card the owner keeps isn't refused for it. The
  page still holds at most 100.
- **A kept card has no goal to lose.** It waits, and goes stale after
  fourteen days, as any card does.

## Tests

Nine new in Rust and two in the app, and others widened.

- **The wire** (`ui::tests`, one): an answer with `out_of_steps` and
  without, `SuggestionOp::Keep`, and `SuggestionSource::Chat`.
- **The hub** (`hub::tests`, one): an offer is held while its
  conversation is open and not for another; a run offered again holds its
  newest words once; a kept one is withdrawn alone; past eight the oldest
  goes; a new conversation withdraws all, whether or not it is for an
  idea.
- **The delivery** (`api_ops::tests`, one): through the hub's delivery
  and the chat sink, an answer said to have run out of steps reaches the
  app so, with its run; another answer, and Hatchling's notice, don't.
- **The chat** (`chat_run::tests`, two): an answer shown that ran out of
  steps is delivered saying so and offers what was said, once, with its
  run and conversation; an answer with steps to spare, a failed run, an
  answer not shown, one that reached no one, and one after the owner
  started another conversation offer nothing. And the chat's own turn
  hands the hub the words as a kept card would hold them.
- **The keep** (`suggestions::tests`, four): the words trimmed and
  bounded, cut at a character with no space before the ellipsis; a card of
  the owner's words with the broker's title, why and source at the top of
  the page, the offer withdrawn, and nothing kept that isn't offered, is
  empty, or with no chat or scheduled work off; a kept card has no goal to
  lose; and a page full of waiting ideas still takes it, stays at its
  bound, and a page that can't be read or written keeps nothing and leaves
  the offer.
- **The owner's door** (`api_ops::tests`, one): through the API, with and
  without the hub, and refused to a caller who isn't the owner.
- **The app** (`live-ideas.test.mjs`, `live-adapter.test.mjs`, one each):
  the keep and the kept card's words; and, end to end through the adapter,
  the choice under an answer in the owner's chat that names its run, not
  under any other, a press the broker no longer offers, and a press that
  keeps it.
- **Widened**: every chat message built in the broker carries the new
  field; the delivery tests in the scheduler; the four version tests and
  the app's (v30).

## Breaks by hand

Thirty-five in Rust (`unit-breaks.log`, `unit-*.log`, `kl_break.py`) and
eleven in the app (`app-breaks/breaks.log`, `app-breaks/*.log`,
`kl_app_break.py`). All were caught, and the tests passed before and
after.

| Break (Rust) | Caught by |
|---|---|
| An answer writes `out_of_steps: false` on the wire | the wire's test |
| A kept card has another word on the wire | the wire's test |
| The hub's delivery drops `out_of_steps` | the delivery test |
| A notice is said to be out of steps | the delivery test |
| The app is never told | the delivery test |
| The chat delivers every answer as out of steps | the chat's test |
| Every answer offers | both chat tests |
| No answer offers | both chat tests |
| The offer names another conversation | both chat tests |
| The offer is taken and kept | the chat's test |
| The chat's turn offers nothing | the turn test |
| The chat's turn offers the words unbounded | the turn test |
| An offer for another conversation is held | the hub's test |
| A run offered again is held twice | the hub's test |
| The offers grow past the bound | the hub's test |
| A new conversation keeps the offers | the hub's test |
| A conversation for an idea keeps the offers | the hub's test |
| A withdrawal withdraws nothing | the hub's, keep and door tests |
| Any offer answers for any run | the hub's and keep tests |
| The API keeps nothing | the door test |
| A keep with no chat is said to be off | the keep and door tests |
| An empty offer is kept | the keep test |
| A keep runs with scheduled work off | the keep test |
| The card has another title | the keep test |
| The card says no why | the keep test |
| The card has no source | the keep test |
| The card goes to the bottom | the keep and bound tests |
| The page grows past its bound | the bound test |
| The offer stays after a keep | the keep and door tests |
| A kept card loses a goal | the no-goal test |
| The page's own handling keeps what a chat offers | the no-goal test |
| The words are kept untrimmed | the words test |
| The words are kept whole | the words test |
| The cut has no room for the ellipsis | the words test |
| A space is kept before the ellipsis | the words test |

| Break (app) | Caught by |
|---|---|
| Every answer offers to keep | the adapter's keep test; the trail test |
| A connected app's chat offers to keep | the adapter's keep test |
| The offer never shows it was kept | the adapter's keep test |
| The offer shows kept before the press | the adapter's keep test |
| Any message is an offer | the adapter's keep test, and most of the adapter's tests |
| Any option keeps | the adapter's keep test |
| A refused keep is marked kept | the adapter's keep test |
| A kept card says it is the agent's words | the kept-card test |
| A goal's card is said to be kept by the owner | how an idea is shown |
| The page isn't read after a keep | the adapter's keep test; the kept-card test |
| A keep not offered is said to have failed | the adapter's keep test; the kept-card test |

## Mutation

Every mutant cargo-mutants makes on the lines changed against the base,
in each crate, with that crate's own tests (`mut.log`, `mut-*`). None was
missed and none timed out. Core's change is wire types alone: it has
nothing to mutate, and its log says so.

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 0 | 0 | 0 | 0 | 0 |
| `hatchling-channel` | 12 | 12 | 0 | 0 | 0 |
| `hatchling-scheduler` | 3 | 3 | 0 | 0 | 0 |
| `hatchling-broker` | 38 | 27 | 0 | 0 | 11 |

Command, from `mut.log`: `cargo mutants --in-place --in-diff
kl-slice-<crate>.diff --timeout <60|180> -o <out> -- --lib`.

Unviable, each a build failure:

- `crates/hatchling-broker/src/api_ops.rs:359:9: replace Ops<'a>::owned -> (UiActionResult, ApiOutcome) with (Default::default(), Default::default())`: unviable, build failure.
- `crates/hatchling-broker/src/api_ops.rs:732:9: replace Ops<'a>::send -> (UiActionResult, ApiOutcome) with (Default::default(), Default::default())`: unviable, build failure.
- `crates/hatchling-broker/src/api_ops.rs:788:9: replace Ops<'_>::new_conversation -> (UiActionResult, ApiOutcome) with (Default::default(), Default::default())`: unviable, build failure.
- `crates/hatchling-broker/src/api_ops.rs:883:5: replace chat_sink -> hatchling_channel::Sink with Default::default()`: unviable, build failure.
- `crates/hatchling-broker/src/chat_run.rs:338:9: replace Chat<R, K>::resuming -> Self with Default::default()`: unviable, build failure.
- `crates/hatchling-broker/src/chat_run.rs:378:9: replace Chat<R, K>::take_offer -> Option<Offer> with Some(Default::default())`: unviable, build failure.
- `crates/hatchling-broker/src/chat_run.rs:466:9: replace Chat<R, K>::step -> Step with Default::default()`: unviable, build failure.
- `crates/hatchling-broker/src/chat_run.rs:667:5: replace chat_turn -> Step with Default::default()`: unviable, build failure.
- `crates/hatchling-broker/src/suggestions.rs:477:5: replace handle_at -> SuggestionResult with Default::default()`: unviable, build failure.
- `crates/hatchling-broker/src/suggestions.rs:625:5: replace keep -> SuggestionResult with Default::default()`: unviable, build failure.
- `crates/hatchling-broker/src/suggestions.rs:635:5: replace keep_at -> SuggestionResult with Default::default()`: unviable, build failure.

No equivalence is claimed: there was no missed mutant to explain.

## The gate

On each toolchain, from a cleared build cache (`gate.log`,
`gate-1.*.log`): `cargo fmt --all -- --check`, `cargo clippy
--workspace --all-targets --locked -- -D warnings`, the same with
`--all-features`, `cargo build --workspace --locked`, `cargo test
--workspace --locked --no-fail-fast`, `hatchling-dev seed-check` and
`hatchling-dev protocol-schema`.

| | 1.85.0 | 1.98.1 |
|---|---|---|
| Every step | exit 0 | exit 0 |
| Test binaries with a result | 135 | 135 |
| Tests passed | 3958 | 3958 |
| Tests failed | 0 | 0 |

The first run of the gate on this tree (`first-gate/`) passed on 1.85.0;
on 1.98.1 two tests failed that this change doesn't touch,
`vault_keyring::tests::status_shows_every_way_in_and_each_key_and_never_a_code`
and `vault_keyring::tests::the_command_runs_the_key_actions_through_its_devices`.
Its tests ran across 00:00 UTC: each test stamps a key with the day it is
added and compares a day read again afterwards, and the two days
differed (2026-11-15 and 2026-11-16, in the failure's own words). The
gate was run again, whole, on the same tree, and passed; that run is the
table above.

As root, outside the sandbox's jail (`root/as-root.log`): 202 tests in
core, 42 in the scheduler, 35 in the channel, and 114 in the broker's
modules this touches. All passed.

The app's own check (`frontend-check.log`, `npm run check`: types, lint,
the frozen-file check, 502 unit tests, coverage of the live adapter at
100%) passed on the tested tree.

## Found on the way

- **Clippy's findings, fixed without exemptions.** The first build had an
  item that wasn't exported, a wildcard match, two functions past 100
  lines (the chat's step, and one test), a helper returning an `Option`
  that was always `Some`, and `panic!` in two tests. The chat's ending is
  now noted by a small method of its own, the long test is two, and the
  rest were rewritten.
- **A trim that could be made to hang.** The first mutation run on tree
  `3c4668f7…` timed out on one mutant: `end -= 1` made `end /= 1` in the
  loop that finds where to cut the owner's words, which then never ends.
  The cut is now found by a bounded search, and every run was made again
  on the tree above.
- **Breaks that proved nothing at first.** Five of the breaks (two in
  Rust, three in the app) didn't compile as first written, and one of the
  app's changed nothing a test could see. Each was written again to
  compile and to change what it names. One Rust break was listed against a
  test that compares with the same function it breaks; it was listed
  against the test that does catch it.
- **A break that wrote into the checkout.** With scheduled work off, one
  break let the keep write the page to a relative path, inside the
  sandbox's checkout. The break runner now clears untracked files after
  each break.
- **A midnight flake elsewhere.** The two security-key tests above depend
  on the day not changing between two reads of the clock. That is filed
  to be fixed apart.

## What wasn't tried

- **An install.** This change has not run on one. That a real model's
  out-of-steps answer shows the choice, and that the press keeps the card
  on a real page, are to be seen there.
- **The lines that join the parts.** The chat loop calling its turn, and
  the chat view drawing the choice, are read and not run here.
- **A restart.** That a broker restart withdraws the offer follows from
  the hub being in memory; no test restarts a broker.
- **Words that aren't text.** What the owner said is offered as the chat
  kept it; a message with only an attachment offers its text, which may be
  empty, and then nothing is kept.
