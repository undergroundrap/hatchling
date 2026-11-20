# Evidence: every protection, where it's enforced, and what's said when it's off (WO-037 slice 2 and close)

What was checked, how, and what came back, for WO-037: a pass over every
protection Hatchling has, against the code that enforces it, and what `doctor`,
the Your security page and `docs/your-security.md` say when it's on, weaker,
off or unknown. Slices 1a and 1b fixed `doctor`'s words. This slice fixes the
page's and the doc's, and closes the work order.

| | |
|---|---|
| Base | WO-037 slice 1b (#551), on `bd8dd381` (#550) |
| Date | 2026-11-20 (UTC) |
| How | each protection read in its code; `doctor` (`crates/hatchling-broker/src/doctor.rs`); the page (`frontend/src/features/settings/security.ts`, which shows `docs/your-security.md` word for word, `your-security-text.ts`, plus the notices for this install, fed by `read_setup`) |

This record was added after the runs, in its own commit. It changes no code.

## The inventory

"Said" means said plainly, with what to do, in `doctor` and, where the page has
a notice for it, on the page. Every state below that can arise is now said
somewhere the owner looks.

| Protection | Enforced at | Weaker, off or unknown when | Said by |
|---|---|---|---|
| Cells: user, mount, network, IPC and pid namespaces, their own root | `hatchling-isolation/src/linux/spawn.rs`, `child_main`; `hatchling-sys/src/mounts.rs` (`pivot_root`) | the kernel refuses one (`max_user_namespaces` 0, root only, AppArmor's userns rule, a container): no cell starts | `doctor` names the step and why (before this WO); the doc, "The cells" |
| Cells' syscall filter | `spawn.rs` (`set_no_new_privs`, `seccomp::install`) | a kernel without seccomp filtering: no cell starts | `doctor`'s probe now installs the agent's filter (1b); the doc, "The cells" (this slice) |
| Cells' cgroup | `spawn.rs`, `hold_to_limits`; `CellCgroup` | `HATCHLING_CELL_CGROUP` unset: process limits alone; the Room and the browser can't run | `doctor`'s note names what they turn off (1a); the page's notice says it (this slice); the doc, "The cells" (this slice) |
| The browser cell | `spawn.rs`, `bounded_as_a_whole`; `browser_cell.rs` (proxy); `hatchling-browser` (an empty profile each session) | set where cells have no cgroup: it can't start | `doctor`, a problem (1a) |
| The agent's computer, the Room | `room_launch.rs`, `check_installed` | not x86_64, no image admitted, the image not in place, no cgroup | `doctor` names the first missing (1b); the doc (this slice). **Offering it only where it can start is a change of behaviour: #549** |
| The room disk, sealed | `room_disk.rs` | no cryptsetup, device-mapper or loop devices; not read as root; open while the vault is locked | `doctor` and the Data page (before this WO); the doc, qualified for a vault a stolen disk opens (this slice) |
| The vault's key | `hatchling-vault` (keyring tiers); `doctor`'s `check_vault` | its key file alone, the file tier, authenticator codes: a stolen disk opens it | `doctor`, a note each, where they read as fine (1a); the page's notice for this install and the doc (before this WO) |
| The Keeper and the Broker each as their own user | `keeper.rs`, `service_user`; `broker_user.rs`, `drop_to_broker` | run by hand, not as root: both run as the owner | `doctor`, a note (1a); the doc, qualified (this slice) |
| The Broker's own file access | `confine.rs`, `confine_broker` (Landlock) | a kernel without Landlock: unconfined (it was only logged) | `doctor`, a note (1a); the doc, "A compromised Broker" (this slice) |
| The output gate on channel replies | `output_gate.rs` | `HATCHLING_OUTPUT_GATE=off`, or a built-in check off (`HATCHLING_REDACTION_BUILTINS`) | `doctor`, a note naming what isn't checked (1a); the doc (this slice) |
| The agent's uid | `config.rs`, `refuse_shared_agent_uid` | `HATCHLING_AGENT_ALLOW_NOBODY`: any process running as nobody could act as the agent | `doctor`, a note (1a) |
| The owner's decisions kept | `api.rs`; `approval_board.rs`; `decisions.rs` | the scheduler off: not kept | `doctor`, a note (1a); the doc (this slice) |
| The Windows distro's seal | `installer/windows/setup.ps1` (`/etc/wsl.conf`) | the drive mounted or interop on again, after install | `doctor` checks it under WSL (1b); the doc (this slice) |
| The whole setup unread | `api_ops.rs`, `read_setup` | the Keeper refuses or fails the read: the page's notices drop out | the page now says it couldn't read this install, and points to `doctor` (this slice) |
| Egress and the allowlist | `hatchling-egress`; cells' `unshare_net` | none found: no weaker mode | — |
| Standing grants and what they covered | `site_grants.rs`, `covered.rs` | no grants store: standing yeses aren't offered; a record that can't be kept: not covered | the card offers what the broker keeps (WO-036) |
| Test-only switches (`HATCHLING_REQUIRE_*`, `HATCHLING_TEST_CGROUP`) | read only in test modules | never in production | — |

## The work order's criteria

| Criterion | Where it's met |
|---|---|
| The evidence lists each protection checked, where it's enforced, and what `doctor` and the page say when it's on, weaker, off or unknown | the inventory above |
| Every finding is fixed in words here, or has its own linked issue | every row is said; the one change of behaviour is #549 |
| For each protection that can be off, a test makes it off and checks `doctor` says so in plain words | [doctor-says-off.md](doctor-says-off.md) and [doctor-probes.md](doctor-probes.md), each with its tests and hand-breaks; the namespaces' and the room disk's were tested before this WO (`each_cell_step_has_its_own_advice`, the room disk's tests) |
| Nothing on the Your security page claims a protection the install doesn't have | the doc's claims are qualified where an install can lack them (the Keeper's user, the cells' filter and cgroup, the Room's disk, the Windows seal, the output gate, the Broker's confinement, decisions kept), and an install that couldn't be read is said so |

## Results for this slice

| Check | Result |
|---|---|
| `npm run check` (typecheck, lint, design, protocol, the lock, tests, coverage), on Windows | 880 passed, 0 failed |
| The page shows the doc word for word (`your-security.test.mjs`) | passed |
| The page's notices: unread said, read said by tier and cgroup, nothing in the demo or while it's read | passed |
| `hatchling-dev audit` on the final tree, the close included (`e86be848`) | all checks passed |

## Residual

- **#549:** the Room is offered before anything checks its image is
  installed.
- `doctor`'s **probe can't be told apart** from one that skipped the filter
  (slice 1b's residual).
- The page's notices are fed by the Keeper's `read_setup`. The others above are
  `doctor`'s alone. Putting them on the page means `read_setup` saying them,
  which is a change of the local API, for a later work order.
