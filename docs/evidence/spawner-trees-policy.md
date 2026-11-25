# Evidence: a Trees request is held to the spawner's policy on both paths (#690)

| | |
|---|---|
| Base | `main` at `3b2b82e2` (#678) |
| Date | 2026-11-25 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |
| Found by | an outside model's review of `spawner.rs`, confirmed in the code |

## What was wrong

The spawner serves `Request::Trees`, which gives a harness's work trees to a cell's identity or takes them back. It checked the request on only one of its two paths:
- with `direct: true`, `own_trees` held the ids to the configured cell's, and each directory to a harness's tree;
- with `direct: false`, nothing was checked. The broker's directories and ids went straight to the tree helper.

The spawner exists so that a compromised broker can't change anything its policy doesn't allow (WO-017, step 3). On a root install the spawner is root and cells have direct ids, so `direct: false` never arises legitimately. A compromised `hatchling-broker` could still send it, and have root run the tree helper on paths it named, with ids it chose.

## What it does now

Both paths go through `trees_for`, which checks everything before anything is changed:
- the ids must be the configured cell's;
- the mapping must be the configured one, so a spawner configured for direct ids refuses the subordinate path, and the other way round;
- every directory must be a harness's tree beneath the spawner's temp directory.

The subordinate path then hands the helper the checked trees, not the broker's paths.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| clippy `-D warnings`, 1.98.1 and 1.85.0 | pass on both |
| the whole workspace's tests | 4491 passed, 0 failed |
| `hatchling-dev audit` | all checks passed |

New test, `a_trees_request_is_held_to_the_policy_whichever_path_it_asks_for`, drives a real connection through `serve_connection`. The helper program is one that doesn't exist, so a request that got as far as starting it would fail with a different reason from any refusal it expects. It checks four refusals:
- the subordinate path asked of a direct spawner, with the configured ids: refused for its mapping;
- on a subordinate spawner, ids that aren't the configured cell's: refused;
- on a subordinate spawner, a host directory (`/etc`): refused as not a harness's tree;
- the direct path asked of a subordinate spawner: refused.

**Hand-breaks**, each failing the test and passing again when restored:

| Break | Caught by |
|---|---|
| The subordinate path passed the broker's directories unchecked, as before | `a_trees_request_is_held_to_the_policy_whichever_path_it_asks_for` |
| The mapping check removed | `a_trees_request_is_held_to_the_policy_whichever_path_it_asks_for` |
| The harness-tree check removed | `a_trees_request_is_held_to_the_policy_whichever_path_it_asks_for` |

**Mutation**, on the lines changed against `3b2b82e2` (`cargo mutants --in-diff`, `hatchling-broker`): 8 tested, 8 caught, 0 missed, 0 timeouts.

An earlier run timed out on one: with `serve_connection` replaced by `Ok(())`, nothing replied, and the test waited on a read while it still held the spawner's end of the socket. The test now closes that end before it reads, so a reply that never came is the end of the stream, and the mutant is caught.

All of the above ran on tree `ff5a4a3c`.
