# Evidence: an idea's start note no longer says Hatchling wrote it

What was tested, how, and what came back, for a one-line wording change in
`frontend/src/features/ideas/ideas.ts`. Every number here is read from a
file in the results bundle, which is attached to the release that carries
this change.

| | |
|---|---|
| Base commit | `50603c67` (`test(vault): the keyring tests read a key's day from its record (#470)`) |
| Tested tree | `0818ba4b8f02dbffdb413ecb0be428ffc7c27798` |
| Date | 2026-11-16 (UTC) |
| Machine | x86_64, Windows 11, Node 22.17.0, from Git Bash |
| Results bundle | `hatchling-idea-start-wording-results.tar.gz`, 30940 bytes, sha256 `18c29d049f527588168c8bf2e6955e576b708890b3da068ff351a7e05230b279` |

This record was added after the run, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What changed

The note under an idea's "Let's do this" said the idea goes into the chat
box "as Hatchling wrote it". A card kept from a chat (#469) is the owner's
own words, so the note now says only what happens: "This starts a new
conversation and puts the idea in your chat box. Read it, change it, and
send it when you're ready."

## Run (`check.log`)

`npm run check` in `frontend/`: exit 0.

- Typecheck, lint, design, actions, protocol, seam and format checks passed.
- Unit tests: 502 passed, 0 failed.
- The live adapter's tests: 184 passed, 0 failed, coverage 100% of lines,
  branches and functions.
- The operator page's tests: 16 passed, 0 failed, coverage 100%.

## What wasn't tried

- **The Rust gate and mutation.** No Rust changes; the change is to one
  string the app shows. CI runs the full gate.
