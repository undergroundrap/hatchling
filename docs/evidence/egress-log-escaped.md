# Evidence: a cell's egress destination is logged escaped (#571)

What was tested, how, and what came back, for
[#571](https://github.com/undergroundrap/hatchling/issues/571), found by
the v0.9.0 release security checklist (item 4: input is checked at the
boundary).

| | |
|---|---|
| Base | `main` at `40a004cf` |
| Tested tree | `63383cbd` |
| Date | 2026-11-22 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |

This record was added after the runs, in its own commit. It changes no code.

## The problem

The broker logged a cell's egress request with `destination = %destination`
and `reason = %refusal.reason`: written as they are, so a newline in
either starts a line of the log's own. The destination is the string the
cell sent. Once its grant is checked it must be one the operator granted,
so reaching these lines with a newline in it takes a grant that has one;
the reason, though, can carry what an upstream or a proxy said. Ten
fields in five places (a body that doesn't match its stage, a request
that got no answer, failed or was denied, and a surrogate minted or not)
were written this way since before 0.8.0, and no test covered them.

The program the agent runs in the Room, logged by `room_ops`, is its
own text too. It was a plain field, with no test saying how it's written.

## What changed

All ten are logged with `?`, escaped, as the item says untrusted text
is. The program's field says `?` too, so nothing rests on how a plain
field happens to be written.

## Results

| Check | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.85.0 and 1.98.1 | clean |
| The broker's `serve::` and `room_ops::` lib tests, 1.85.0 and 1.98.1 | 55 passed on each |
| `hatchling-broker`'s tests, lib and integration, with the workspace built, 1.98.1 | 2,078 passed, 0 failed |
| Mutation of the lines changed (`--in-diff`, the broker's lib tests) | 4 mutants, 4 unviable: a field's `%` or `?` is no mutant |

`an_egress_refusal_forges_no_line_of_the_log` sends a destination and a
reason, each with a newline before a marker, through each kind of
refusal, a surrogate refused and one minted, and a body that doesn't
match its stage to a destination granted as it was asked for, and finds
each marker logged but never at the start of a line.
`a_programs_name_forges_no_line_of_the_log` does the same for a
program's name.

### Hand-breaks

Each field was put back to `%`, the `serve::` and `room_ops::` tests run,
then undone. 11 broken, 11 caught:

| Break | Caught by |
|---|---|
| Each of the ten egress fields (10 breaks) | `an_egress_refusal_forges_no_line_of_the_log` |
| The program's field | `a_programs_name_forges_no_line_of_the_log` |
