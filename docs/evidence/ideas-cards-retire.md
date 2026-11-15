# Evidence: an idea that waits goes stale, or loses its source

What was tested, how, and what came back, for a change that takes
ideas out of the owner's For you when they have waited too long or the
goal they came from is gone. Every number here is read from a file in
the results bundle, which is attached to the release that carries this
change, unless it says otherwise.

| | |
|---|---|
| Base commit | `41c248a4` (`feat(ideas): an idea names the goal it comes from and what it needs (#463)`) |
| Tested tree | `dba16785cb020bc2aac52e0421a76ae55a3de068` |
| Date | 2026-11-15 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox; the app's check and the app's breaks on Windows 11 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-ideas-cards-retire-results.tar.gz`, 1591489 bytes, sha256 `a4f619f1825e1425c23effb6c785759172be2b13f367012691a950cd309d039c` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it. In the
bundle's `app-breaks/`, the Windows machine's paths are replaced with
`<repo>` and `<scratchpad>`; nothing else in those logs was changed.

## What was wrong

An idea waited in For you until the owner started it or set it aside.
Nothing else took one out, so the page only grew, and an idea stayed
after the goal it came from was closed. Twenty could wait: a busy
fortnight filled the page, and later ideas were refused.

## What changed

- **Two new states** (`hatchling-core`, `SuggestionState`): `stale` and
  `source_gone`. The session protocol is v28 for the app's sake; no
  session op changes.
- **The broker** (`suggestions.rs`):
  - `retire` finds, among ideas still waiting, the ones that have
    waited more than fourteen days (`STALE_SECS`) and the ones whose
    goal isn't among the owner's open goals.
  - It runs when the owner's app lists the page, with the owner's open
    goals (`open_goals`, read as their goals page reads them), and what
    it finds is written back. It also runs when a run suggests, with no
    goals: a run may not ask after all of them, so only staleness is
    found there.
  - `Restore` stamps the idea with the time it was brought back. An
    idea whose source is gone is answered as it stands while its goal
    is closed.
  - Sixty may wait (`MAX_WAITING`), where twenty could.
- **The app** (`frontend/src/adapters/live/ideas.ts`): a retired idea is
  shown among those set aside, with a line saying why; bringing back
  one whose goal is closed says it stays; and an idea says the goal it
  came from, ahead of the note that the rest is the agent's words.
- `docs/session-protocol.md`, the changelog, and the app's generated
  protocol types.

## What retirement is, and isn't

- **It is the broker's finding.** The agent has no op that retires,
  sets aside or removes an idea, and none was added.
- **Only waiting ideas are judged.** One the owner started or set aside
  stays as they left it.
- **A source is judged only for the owner.** A run's session may read
  one goal by its id, under its own grant; it isn't given the owner's
  whole list. So a lost source is found when the owner's app lists the
  page, and not when a run suggests.
- **Unreadable goals retire nothing.** With no goals store, or one that
  can't be read, no idea loses its source.
- **A date that can't be read retires nothing.**
- **The owner still decides.** A stale idea can be brought back, and
  any idea can still be started or set aside.

## Tests

Nine new, and others widened.

- **The wire** (`ui::tests`, one): each state's written word, the two
  new ones among them; a word that isn't a state never decodes; and
  the protocol is 28.
- **The broker** (`suggestions::tests`, six): an idea goes stale after
  fourteen days and no sooner, a started or set-aside one isn't
  judged, what is found is written back, and an unreadable date
  retires nothing; an idea whose goal is closed loses its source at
  once, one with no source has none to lose, and unreadable goals
  judge nothing; an idea both stale and without its goal has lost its
  source; a stale idea brought back waits from then, with its goal
  never asked after, and one whose goal is closed stays until the goal
  is open again; a stale idea makes room for a run that suggests, and
  sixty may wait; and, against a real goals store, the owner's open
  goals are read, a closed goal's idea loses its source through the
  broker's own door, and a store that can't be read gives none.
- **The app** (`live-ideas.test.mjs`, two): an idea says its goal, and
  a retired one is shown as set aside with why; one whose goal is
  still closed stays when brought back, the owner is told, and setting
  one aside isn't taken for a restore.
- **Widened**: the three older wire tests and the app's protocol test
  expect version 28; the page's tests ask at a time of their choosing.

## Found on the way

- **A frozen file.** The first version gave retired ideas a state of
  their own in the app's model (`frontend/src/protocol/models.ts`). The
  app's check refused it: that file is frozen, and a change to it is
  proposed and reviewed, not made in passing. The change was taken
  out. A retired idea is shown with the app's existing "dismissed"
  state and a line saying why.
- **Two tests in one directory.** A new test failed in the first run:
  an idea whose goal was open came back as having lost its source. Run
  alone it passed. Looped with the others, it failed ten times in
  twelve, with another test's ideas on its page: it had been given a
  scratch directory name an older test already used. With a name of
  its own it passes, and the code it tests was never changed. Those
  runs were on trees `e5024cf6…` and `387badbe…`.
- **Two breaks that didn't build.** Two of the app's seven breaks
  failed to compile at first, so no test ran for them. They were
  written again to compile, and all seven are caught.

## Breaks by hand

Twenty-five in the broker and core, each one part of the change taken
out or bent, the tests run on each, and each failing test named
(`unit-breaks.log`, `unit-*.log`, `ir_break.py`). All twenty-five were
caught, and the tests passed before and after.

| Break | Caught by |
|---|---|
| Stale has another name on the wire | the wire's test |
| A lost source has another name | the wire's test |
| The protocol is still 27 | the wire's test; the three older wire tests |
| An idea is stale at fourteen days to the second | the stale test; the room test; the bring-back test |
| Stale is thirteen days | the stale test |
| Nothing goes stale | the stale test; the room test; the bring-back test; the source test |
| An idea the owner started goes stale | the stale test |
| A date that can't be read is stale | the stale test |
| What is found isn't written back | the stale test; the source test; the bring-back test |
| Nothing is retired when the page is listed | the stale test; the source test; the both test; the bring-back test; the store test |
| No idea loses its source | the source test; the both test; the bring-back test; the store test |
| An idea whose goal is open loses its source | the source test; the both test; the stale test; the bring-back test; the store test |
| Goals that can't be read retire every idea with a source | the source test; the store, stale and room tests; four of the page's older tests |
| An idea kept before sources were has lost one | the source test |
| Stale is said before a lost source | the both test |
| A closed goal counts as open | the store test |
| Goals that can't be read are read as none open | the store test; the stale and room tests; three of the page's older tests |
| The door asks after no goals | the store test |
| Brought back, it keeps its old date | the bring-back test |
| An idea comes back while its goal is closed | the bring-back test |
| Goals that can't be read bring an idea back | the bring-back test |
| A stale idea's goal is asked after | the bring-back test |
| A run that suggests retires nothing | the room test |
| A run that suggests judges sources | the room test; the stale test; the store test; three of the page's older tests |
| Twenty may wait | the room test |

Seven in the app, each built and run against the app's two test files
for the page, which passed before and after (`app-breaks/breaks.log`,
`app-breaks/*.log`, `ir_app_break.py`). All seven were caught.

| Break | Caught by |
|---|---|
| A stale idea is shown as waiting | the says-why test |
| An idea with no source left is shown as started | the says-why test; the stays test |
| A retired idea doesn't say why | the says-why test |
| The two reasons are the wrong way round | the says-why test |
| An idea doesn't say its goal | the says-why test |
| An idea that stayed gone is said to be back | the stays test |
| Setting aside a gone idea is taken for a restore | the stays test |

## Mutation

`cargo mutants --in-place --in-diff` on the change to each crate's
source (`mut.log`, `mut-broker-outcomes.json`, sha256
`bdf31f17828b673d5e2cb0bac610a53188c1d5c389ff4d27f3c6c791c2f6a42b`):

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 0 | 0 | 0 | 0 | 0 |
| `hatchling-broker` | 28 | 24 | 0 | 0 | 4 |

Core's change is two enum variants and a constant, with nothing in it
to mutate; `mut-core.log` says so. The app's TypeScript has no
mutation run: its seven breaks by hand are what test it.

The unviable ones:

- `suggestions.rs:274:5: replace suggest -> SuggestResult with Default::default()`: unviable, build failure
- `suggestions.rs:338:5: replace suggest_from -> SuggestResult with Default::default()`: unviable, build failure
- `suggestions.rs:451:5: replace handle -> SuggestionResult with Default::default()`: unviable, build failure
- `suggestions.rs:465:5: replace handle_at -> SuggestionResult with Default::default()`: unviable, build failure

## The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3933 passed, 0 failed | exit 0: 135 binaries, 3933 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

The tests of what the change touches were run as root too, as CI runs
them: 200 in core, 31 in the broker, and the agent's 184 and its loop's
10, which this change doesn't touch (`root/as-root.log`).

The app's own check (`npm run check`: types, lint, the frozen-file
check, 496 unit tests, and coverage of the live adapter at 100%) passed
on the same tree, on Windows (`frontend-check.log`).

## What wasn't tried

- **An install.** Nothing has been retired on one, and no page was
  opened: the app's side was tested through its adapter.
- **Done.** An idea the owner started isn't marked done when its chat
  finishes. The broker can't yet tie a started idea to the run that
  answered it; that comes with the change that opens a conversation
  for a started idea.
- **A view of their own.** Retired ideas are shown under Dismissed,
  with why. A state of their own in the app needs a change to its
  frozen model.
- **An idea from an unfinished run.** There is no such source yet, so
  none to lose.
- **A goal closed and opened again between two readings.** The idea is
  judged by the goals as they are when the page is read.
- **Two clocks.** Fourteen days is counted on the broker's clock from a
  time the broker wrote. A clock set back makes an idea wait longer;
  one set forward retires it sooner.
