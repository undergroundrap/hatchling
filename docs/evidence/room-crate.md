# Evidence: the Room's supervisor moved into a crate of its own

What was tested, how, and what came back, for moving the supervisor that
runs inside the Room out of `hatchling-browser` into the new, unpublished
`hatchling-room` crate. Every number here is read from a file in the
results bundle, which is attached to the release that carries this change.

| | |
|---|---|
| Base commit | `5bc16b7e` (`feat(ideas): a kept chat card says where its run said it stopped (#473)`) |
| Tested tree | `c992ce02f02167b5801a3eaed7d4969f4b9980f5` |
| Date | 2026-11-16 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 threads, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned) |
| Results bundle | `hatchling-room-crate-results.tar.gz`, 206768 bytes, sha256 `f87ffdc5c7264b2e71cad28f44eded6f54f2e30e7389c6024a8f4d2d0f228dbf` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What moved, and what didn't change

- `crates/hatchling-browser/src/bin/hatchling-room.rs`, its three modules
  in `src/bin/hatchling-room/`, and `tests/room_supervisor.rs` are in
  `crates/hatchling-room/`, at the same paths within the crate. Git sees
  five renames with no line changed.
- The `room-supervisor` feature moved with them: the binary and its proof
  still build only with it, so a plain build or test of the workspace
  builds neither, as before. (The proof runs the static release build
  under the Room's filter; a debug build can't pass it.)
- CI's static supervisor step and `scripts/room-image-supervisor.sh` name
  the new package, with the same feature, target and flags.
- `hatchling-dev`'s audit holds `hatchling-room` to the cell side, as it
  holds the agent and the browser's controller, with a test that it
  reports a path to a trusted crate. The module map, the dependency
  review and the release workflow's paths name the crate.

## Runs (`run.log`, `gate.log`)

| Step | Result |
|---|---|
| CI's step, on the new crate: its graph, static | 58 lines; reaches `hatchling-core` and `hatchling-sys` only, nothing on the trusted side |
| CI's step: `cargo test --release --target x86_64-unknown-linux-gnu -p hatchling-room --features room-supervisor --bin hatchling-room --test room_supervisor`, static | exit 0; 46 unit tests passed; "three supervisor proofs passed under the room filter" |
| The static supervisor by `scripts/room-image-supervisor.sh`, from the base tree (rebuilt by taking the slice's diff back) and from this one | both built; sha256 `b7dc7ff3…` before, `96a11d3b…` after |
| Their sections (`sections-*.txt`) | `.text` 1370355 bytes in both; `.rodata` 64 bytes shorter after (its own source paths); every other loaded section the same size |
| `cargo mutants --list`, the supervisor's mutants before (`--package hatchling-browser --features room-supervisor`) and after (`--package hatchling-room --features room-supervisor`) | 149 and 149; the same, line and column, once the crate's directory is named the same (`MUTANTS-SAME`) |
| Full gate, 1.85.0 | fmt, clippy (default and all features), build, test, seed-check, protocol-schema: each exit 0; 135 test binaries, 3965 passed, 0 failed |
| Full gate, 1.98.1 | the same; 3965 passed, 0 failed |

The binaries' bytes differ, as they must: the package's name is part of
every symbol's crate hash, and of the paths the binary keeps for panics.
`strings.diff` has the strings that differ; they are those symbols and
paths.

## Found on the way

- **The first try wasn't a move.** It put the supervisor at
  `src/main.rs` without its feature. The audit's thin-main rule refused a
  753-line `main.rs`, and the plain workspace test ran the proof in a debug
  build, where the Room's filter ends it. Both are why the layout and the
  feature stay as they were. That try's logs aren't in the bundle; every
  run above is of the tree above.

## What wasn't tried

- **Mutation.** Nothing in the supervisor changed; the mutant lists above
  show it is the same code. The audit's new test is in `hatchling-dev`,
  whose own mutants weren't run for one constant and its test.
- **An image built in CI** from this tree: the release workflow builds it
  from the same script.
