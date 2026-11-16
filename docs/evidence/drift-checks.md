# Evidence: the docs and the work-order queue held to the code (WO-042)

What was tested, how, and what came back, for `hatchling-dev`'s drift
check and the `tracking` workflow. Every number here is read from a file
in the results bundle, which is attached to the release that carries this
change.

| | |
|---|---|
| Base commit | `e7d9a0ea` (`docs(workorders): queue the room disk's capacity, step 4's files and the drift checks (#488)`) |
| Tested tree | `d4ea5d0cad9494cf26a449466598468819023836` |
| Date | 2026-11-16 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned) |
| Results bundle | `hatchling-drift-checks-results.tar.gz`, 22305 bytes, sha256 `399994587e33e7cabfb251eb56a8721261c1e6cc3f6f76af2625205679b551ce` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## Runs

**The check's code** (`run.log`, on tree `54d7c72d`, whose
`crates/hatchling-dev` is the tested tree's):

- `cargo fmt --check`: exit 0. The workspace built on both toolchains.
- `cargo test -p hatchling-dev`, 1.85.0 and 1.98.1: exit 0 on each.
- `cargo clippy -p hatchling-dev --all-targets --all-features -- -D warnings`, on each: exit 0.
- **The audit on the real tree:** "all checks passed".
- **The audit on eight stale copies of the real tree**, one at a time, each
  exit 1 with the finding that names it:

| Made stale | The audit said |
|---|---|
| `docs/arch/protocol.md` a version behind | `docs/arch/protocol.md says v30 where PROTOCOL_VERSION is 31` |
| `docs/session-protocol.md` a version behind | `docs/session-protocol.md says v30 where PROTOCOL_VERSION is 31` |
| `docs/install.md` counting four users | `docs/install.md counts 4 system users where packaging/sysusers.d/hatchling.conf makes 5` |
| A queue row's status changed | `WO-036 says QUEUED but its queue row says IN-PROGRESS` (and 037, 038) |
| A work order with no row | `WO-043 (workorder-043-x.md) has no row in the queue` |
| A number skipped | `WO-043 is missing: numbers run without a gap` (and 044) |
| An open work order with no issue | `WO-040 is QUEUED and names no tracking issue` |
| A closed work order with unticked items | `WO-035 is closed with 8 acceptance item(s) unticked` |

- **Mutation** on `crates/hatchling-dev`'s changed lines:
  `cargo mutants --in-place --in-diff dc-slice-dev.diff --timeout 120 -o <out> -- --lib`.
  73 mutants: 65 caught, 0 missed, 0 timed out, 8 unviable
  (`mut-outcomes.json` sha256 `d81f542cddc801f327245c0fa7eeca588228ce7c064069f2a23e4fb54c5d59de`).

**The tested tree** (`final.log`): the same code, with `scripts/tracking.py`
counting an issue a pull request closes as closed, the workflow passing it
the event, and WO-042 closed. `cargo fmt --check` exit 0; the audit "all
checks passed"; `cargo test -p hatchling-dev`: 232 passed, 0 failed;
`python3 -m unittest discover -s scripts -p 'test_tracking.py'`: 12 passed.

Unviable:

- `lib.rs:49:5: replace dispatch -> Result<Vec<checks::Finding>, Error> with Ok(vec![Default::default()])`: unviable, build failure
- `drift.rs:50:5: replace run -> Result<Vec<Finding>, crate::Error> with Ok(vec![Default::default()])`: unviable, build failure
- `drift.rs:68:5: replace missing -> Finding with Default::default()`: unviable, build failure
- `drift.rs:90:5: replace versions -> Result<Vec<Finding>, crate::Error> with Ok(vec![Default::default()])`: unviable, build failure
- `drift.rs:158:5: replace users -> Result<Vec<Finding>, crate::Error> with Ok(vec![Default::default()])`: unviable, build failure
- `drift.rs:205:5: replace rows -> Vec<Row> with vec![Default::default()]`: unviable, build failure
- `drift.rs:259:5: replace queue -> Result<Vec<Finding>, crate::Error> with Ok(vec![Default::default()])`: unviable, build failure
- `drift.rs:319:5: replace work_order -> Vec<Finding> with vec![Default::default()]`: unviable, build failure

## Found on the way

- **A status with a full stop.** Two work orders write `**Status:** DONE.`;
  the first run read it as `DONE.` and called them out of step with their
  rows. The status is now the word's letters and hyphens, with a test.
- **Two survivors in the first mutation run:** a read error other than a
  missing file was taken as missing, and any line mentioning system users
  could be taken for the table's row. Each now has a test; the second run
  caught both.
- **The pull request that closes a work order** marks it closed while its
  issue is still open, until it merges. The tracking check counts the
  issues a pull request says it closes as closed, on that pull request.

## What wasn't tried

- **The workflow on GitHub** before this pull request: its script ran
  against the repository from Windows (`tracking.py issues`: no findings),
  and its tests ran in the sandbox.
- **The full gate:** the change is `hatchling-dev`, a script, a workflow
  and docs; CI runs the gate.
