# WO-050: Fuzz every parser of what comes from outside

**Status:** QUEUED
**Area:** a new `fuzz/` workspace (not published), `.github/workflows/`, the parsers below where a target finds a bug, `docs/testing-patterns.md`
**Issue:** [#628](https://github.com/undergroundrap/hatchling/issues/628)
**Trust boundary:** yes

## Problem

Hatchling's broker runs all the time and reads bytes it didn't write: a model's replies, web pages, a cell's frames, a skill's manifest, the trust-roots file. Its tests are strong where someone thought of the case: unit tests, end-to-end runs, and mutation testing with a kill floor of 1.0. Nothing throws the cases nobody thought of at them. Hatchling has no fuzzing at all.

**The attack:** a crafted input that panics or hangs the always-on broker, or that two parts of it read differently, so one checks a request and another acts on something else. It can arrive as a model reply, a page the agent fetched, a frame from a cell running hostile code, or a skill package. A panic is a denial of service: the agent stops until a restart. A disagreement is worse: a gate passes what it should refuse.

Fuzzing runs a parser on millions of generated inputs, keeps the ones that reach new code, and reports any that crash, hang, or break a stated property.

## Scope

**In scope:**

- A `fuzz/` workspace with `cargo fuzz` (libFuzzer), not published, pinned to a nightly toolchain of its own.
- A target for each parser that reads what comes from outside, each checking properties, not only that nothing panics:
  - **The egress request policy** (`hatchling-egress/src/policy.rs`, `check` and `check_body`): no panic; a request it allows parses as the API it's meant for, the same way twice; duplicate keys and oversize bodies refused.
  - **Session frames and messages** (`hatchling-core/src/framing.rs`, and `SessionRequest`, `AgentMessage` from JSON): no panic; a frame over the bound refused before it's read whole; a message that parses serializes back to the same value.
  - **Model replies** (`hatchling-model`, the chat-completions reply parser): no panic; tool calls and content as one reading, never both.
  - **The skill manifest** (`hatchling-skill`, `SignedManifest::from_json`): no panic; what parses validates or is refused, never half-read; canonical bytes stable across a round trip.
  - **The trust-roots file** (`hatchling-broker/src/trust_roots.rs`, `TrustRoots::parse`): no panic; only the strict format parses.
  - **Web pages read as text** (`hatchling-egress/src/web.rs`): no panic, and no hang, within the time and size bounds.
  - **The output gate's held-secret matcher** (`hatchling-broker/src/output_gate.rs`): no panic; a held secret embedded anywhere in an input is found.
- A starting corpus from the tests' own fixtures, kept in the repo.
- A CI job on every pull request that changes one of these crates: each target for 60 seconds, the corpus kept between runs, a failing input uploaded as an artifact.
- Every bug a target finds fixed, with its input added as a regression test in the crate's ordinary tests, so `cargo test` keeps it without the fuzzer.

**Not in scope:** bounded proofs (Kani), sanitizers on `hatchling-sys`, fuzzing the frontend, and changing a parser beyond fixing what a target finds. Each is its own issue if wanted.

## Acceptance criteria

- [ ] `cargo fuzz list` names a target for each parser above, and each states the properties it checks.
- [ ] Each target ran for at least an hour locally with no finding left unfixed, and the evidence names the hours and the coverage each reached.
- [ ] CI runs each target for 60 seconds on every pull request that changes its crate, and a target that finds a crash fails the job with the input attached.
- [ ] Each finding is fixed and has a regression test in the crate's own tests that fails without the fix.
- [ ] A hand-break: a parser changed to panic on one input shape is caught by its target within the CI run's 60 seconds.
- [ ] `docs/testing-patterns.md` says how to add a target, run one, and turn a finding into a test.

## Gates

All must pass before the WO closes:

- [ ] `bash scripts/check.sh` green on the pinned toolchain and the 1.85.0 MSRV (the fuzz workspace is outside it and builds on its own nightly).
- [ ] **Mutation, kill floor 1.0** on any parser changed to fix a finding: `scripts/mutant-run.sh --base <base commit> -- <each changed .rs file>`.
- [ ] Docs that describe the changed behaviour are updated in the same change.

## Evidence

Filled in at close.

## Notes

Fuzzing complements mutation testing: mutation shows the tests notice when the code changes; fuzzing finds inputs the tests never tried. The targets check properties (a round trip, two readings agreeing, a bound held) because a parser that never panics can still be wrong.
