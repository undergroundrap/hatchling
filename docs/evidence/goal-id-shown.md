# Evidence: the goals tool shows each goal's id

What was tested, how, and what came back, for a fix to the agent's
goals tool. Every number here is read from a file in the results
bundle, which is attached to the release that carries this change,
unless it says otherwise.

| | |
|---|---|
| Base commit | `4a46c0d0` (`feat(ideas): a started idea says when it was answered (#466)`) |
| Tested tree | `9c4671a8ff809850b6d76bf1ea578868e7e994f7` |
| Date | 2026-11-15 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-goal-id-shown-results.tar.gz`, 258345 bytes, sha256 `b7c26d1af1ea3d53ca815f2e17fd57a67050b92648a869ed7a15eabcde0f4e02` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it. No file of
the app is changed, so the app's own check was not run.

## What was wrong

An idea names the goal it comes from by the goal's id, and the broker
keeps the idea only when that id is one of the owner's open goals. The
agent's goals tool showed each goal's title, detail and the date it was
set, and not its id.

It was found on an install, with a local model, on the build of the
change before this one. Asked in a chat for two ideas from the owner's
goals, the agent read the goals, called the idea tool seven times, and
was refused each time: "The owner has no goal with that id." It then
told the owner the goal's id wasn't accepted, and asked them for it.
No idea could be kept on any install.

No test caught it. The agent's tests gave the idea tool an id they made
up, and the broker's tests looked a goal up by the id its own store
returned. Nothing checked that the id the broker looks up is one the
agent is ever shown.

## What changed

- `crates/hatchling-agent/src/tools.rs`: what the goals tool shows of
  a goal now starts with its `id`, and the tool's description says the
  id is how a goal is named to another tool.
- Two tests that join the two sides, below.
- The changelog.

No protocol change: the id was already in what the broker serves.

## Tests

One new, two widened.

- **The agent's tool against a real broker** (`tests/goals.rs`,
  widened): a goal is made through the broker, and what the goals tool
  then shows for it carries the id the broker gave it, with its title.
- **The broker's two doors** (`suggestions::tests`, new): against a
  real goals store, the id the broker serves a run that reads the
  owner's open goals is the open goal's id, a closed goal isn't served,
  the id is found as that open goal, and an idea that gives it back
  through the broker's own door is kept.
- **The tool's own test** (`tools::tests`, widened): each goal is
  listed with its id, across pages, and two goals have two ids.

## Breaks by hand

Three, each run against the agent's library tests and its goals test,
and the broker's page tests (`unit-breaks.log`, `unit-*.log`,
`gi_break.py`). All three were caught, and the tests passed before and
after.

| Break | Caught by |
|---|---|
| A goal is shown without its id | the tool's test; the real-broker test |
| A goal's title is shown as its id | the tool's test; the real-broker test |
| Every goal is shown with the same id | the tool's test; the real-broker test |

## Mutation

Every mutant cargo-mutants makes on the lines changed against the base
(`mut.log`, `mut-agent-*`). The broker's change is a test alone: it has
nothing to mutate, and its log says so.

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-agent` | 4 | 4 | 0 | 0 | 0 |
| `hatchling-broker` | 0 | 0 | 0 | 0 | 0 |

Command, from `mut.log`: `cargo mutants --in-place --in-diff
gi-slice-agent.diff --timeout 120 -o <out> -- --lib --test goals`.

No mutant was unviable, and no equivalence is claimed.

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
| Tests passed | 3946 | 3946 |
| Tests failed | 0 | 0 |
| Lines saying skipped | 28 | 28 |

As root, outside the sandbox's jail (`root/as-root.log`): 186 tests in
the agent with 4 in its goals test, and 22 in the broker's page tests.
All passed.

## Found on the way

- **A test over its length.** On tree `08aad5bb…` everything passed but
  clippy, on both toolchains: the broker's check had been added to an
  existing test, which then ran past 100 lines. It is a test of its own
  now, with a store of its own, and every run was made again on the
  tree above.

## What wasn't tried

- **An install.** This fix has not run on one. That a model, shown the
  id, gives it back as written, and that an idea then appears on the
  page, are still to be seen there.
- **A goal with no id.** What the broker serves always has one. A goal
  item without one would not be listed by the tool; no test makes one.
