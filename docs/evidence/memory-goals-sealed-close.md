# Evidence: memory and goals across backup, reset, rekey and an upgrade (WO-047 slice 4)

What was added, and how it was tested, to finish WO-047: the sealed stores carried by backup, restore, each reset and key rotation, a 0.9.0 install's plaintext sealed after an upgrade, and the docs.

| | |
|---|---|
| Base | `main` at `cec31ef9` (slice 3, #642, after #643 and #645) |
| Date | 2026-11-24 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0; and CI's upgrade job, a real install of 0.9.0 upgraded to this commit |

## What it is

- **Backup and restore carry both stores.** Memory's and goals' sealed stores are inside their roots, which are the broker's state, so the roots carry them. #645 (closing #644) fixed the restore that cleared them, after slice 2 listed memory's twice.
- **`reset conversation` keeps them, and `reset agent` clears them**, sealed stores and all.
- **A 0.9.0 backup**, which holds memory and goals as plaintext under their roots' names, restores into the roots, and the broker's next start seals it (slices 2 and 3).
- **Key rotation** moves both stores' keys, as it does every store's: they're in the broker's store list, which `rekey-vault` rotates.
- **An upgrade, on a real install** (CI's *upgrade from the last release*):
  - 0.9.0 is given a goal through the local API, and a remembered item and a tracking log in 0.9.0's own layout, as its broker wrote them;
  - after the upgrade to this commit, the app lists the goal and the item;
  - the plaintext layout is gone, both sealed stores are there, and no file under `/var/lib/hatchling` holds their words (`scripts/sealed_drill.py`).
- **The docs** say what's true now: the trust boundary's *At rest* row, the scorecard, *Your security* and the app's copy of it, the Windows guide, and the storage reference. Memory's session records and context log aren't sealed, and they say so.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.98.1 and 1.85.0 | pass |
| the whole workspace's tests | 4433 passed, 0 failed |
| `hatchling-dev audit` | all checks passed |
| `scripts/sealed_drill.py` | compiles; run by CI's upgrade job |

New tests:
- `a_store_inside_a_state_directory_comes_back_with_it`, now with goals;
- `memory_and_goals_go_with_the_agent_and_stay_with_the_conversation`;
- `a_backup_from_before_the_seal_restores_and_is_sealed_at_the_start`;
- `memory_and_goals_follow_a_rekey`: both stores open under the new key and not the old.

**Hand-break:** the broker's state cleared with the conversation, not the agent, fails `memory_and_goals_go_with_the_agent_and_stay_with_the_conversation`.

**Mutation:** this slice changes no production code but a doc comment (`backup::assemble`'s), so it has no mutants of its own. #645's change, which it depends on, was mutated in its pull request: 4 caught, 2 unviable, 0 missed.

## Said plainly

- **Key rotation at each tier.** Memory's and goals' stores follow a rekey in a test at the file tier. Rotation moves every store in the list the same way at every tier, and the tiers are tested with the vault's other stores. Memory's and goals' aren't run at the TOTP or the security-key tier.
- **On a real install,** the upgrade job lists the goal and the remembered item through the app's API. The Feed's reads of goals, tracking and memory, and recall in a chat, are tested through the real broker pipeline in the integration suites, not on an install.
