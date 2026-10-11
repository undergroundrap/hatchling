# Work orders

A work order (WO) is how core work on Hatchling is planned and closed: a short spec that says what changes, what doesn't, and the evidence that proves it done. It's a contract, so a reviewer can check the result against it, and a future reader can re-run the gates and get the same verdict.

**Who uses them:** the maintainers use WOs to drive the core: the isolation layer, the broker, the vault, egress, the protocol. You don't need one to contribute. A normal pull request is welcome for a bug fix, a test, docs, or a focused feature. It still meets [CONTRIBUTING.md](../../CONTRIBUTING.md)'s bar.

**Want to take one on?** Comment on its issue (or open one) before you start, so two people don't build the same thing. A WO you complete closes with your evidence and your name.

**Want to propose one?** Open an issue describing the problem. If it's core work, a maintainer turns it into a WO from [TEMPLATE.md](TEMPLATE.md).

## Rules

- **Numbered from WO-001, sequentially, forever.** A number is never reused or renumbered.
- **One file each:** `docs/workorders/workorder-NNN-<short-slug>.md`, from [TEMPLATE.md](TEMPLATE.md).
- **Status:** QUEUED → IN-PROGRESS → CLOSED (or DROPPED, with the reason).
- **Scope is fixed once work starts.** If the scope turns out wrong, close or drop this WO and open a new one. Don't silently widen it.
- **A closed WO keeps its evidence inline:** the exact commands, the environment, and the counts.
- **Trivial changes** (a typo, a one-line fix) don't need a WO. They still need the gates.

## Order of work

The goal is a runtime a security team can approve, for one person or a whole company. Work is ordered by what it protects:

1. **What a hostile cell can do.** The cell is the adversary the design exists for, so these come first: WO-009 (a key an upstream echoes back reaches the cell; numeric IP forms pass the allowlist), WO-008 (data leaves through a vendor's URL fetching), WO-010 (a cell can cancel the owner's schedules; an API grant check passes vacuously), WO-005 (`socketpair` families and inherited groups), WO-004 (resource limits).
2. **What another local user, or someone with disk access, can do:** WO-006 (a root broker follows symlinks), WO-007 (any process with the agent's uid can open a session), WO-002 (swapped vault files), WO-003 (key rotation at the keyring tiers), WO-011 (secret copies in memory and core dumps), WO-012 (crash leftovers and safe cleanup).
3. **In parallel, the first five minutes:** WO-001 (`--help`, and a quickstart CI runs). It isn't trust-boundary work, so it doesn't wait on security review.
4. **Then what a security review asks for next:** running without root, the release process (signed binaries, an SBOM, reproducible builds), and root cell tests in CI.
5. **Then integrations** for larger deployments: audit export, external secret backends, fleet configuration, and the macOS backend.

## Queue

WO-002 to WO-011 come from the pre-launch review of the trust boundary: see [Known limitations](../arch/trust-boundary.md#known-limitations). The trust-boundary ones get extra review.

| WO | Title | Trust boundary | Status |
| --- | --- | --- | --- |
| [001](workorder-001-help-and-tested-quickstart.md) | `--help` on every binary, a quickstart CI runs, and a hosted-model guide |  | QUEUED |
| [002](workorder-002-bind-secrets-to-names.md) | Bind sealed secrets to their names | yes | QUEUED |
| [003](workorder-003-keyring-key-rotation.md) | Complete key rotation for keyring vaults | yes | QUEUED |
| [004](workorder-004-cell-resource-limits.md) | Per-cell resource limits | yes | QUEUED |
| [005](workorder-005-seccomp-and-identity-tightening.md) | Tighten the seccomp filter and the cell's groups | yes | QUEUED |
| [006](workorder-006-symlink-safe-ownership.md) | Symlink-safe ownership and staging | yes | QUEUED |
| [007](workorder-007-bind-sessions-to-cells.md) | Bind agent sessions to the cells the broker spawned | yes | QUEUED |
| [008](workorder-008-model-request-policy.md) | A request-body policy for model vendors | yes | QUEUED |
| [009](workorder-009-egress-correctness.md) | Egress correctness fixes | yes | QUEUED |
| [010](workorder-010-schedule-ownership.md) | Schedule ownership and API grant edges | yes | QUEUED |
| [011](workorder-011-secret-hygiene.md) | Secret hygiene in memory and logs | yes | QUEUED |
| [012](workorder-012-disposable-state-and-cleanup.md) | Separate disposable files from your data, and clean them up safely | yes | QUEUED |

## Candidates

Not yet WOs. Each gets a full file, with a scope and acceptance criteria, before work starts:

- **Root cell tests in CI.** CI runs the cell suites as an ordinary user, where a cell's uid is the runner's own. A root broker's cells get another uid, and a bug only that path hits went unseen until the quickstart ran as root. Run the cell suites under `sudo` in CI too.
- **Running without root.** An unprivileged broker can map only its own uid, so its cells run as the owner, and the local API then refuses to start. Give an unprivileged broker a distinct uid for its cells (for example from a subordinate uid range through `newuidmap`), so it can serve both.
- **Explain failed runs.** When a scheduled run fails, the broker logs only "the run failed": the reason is sealed in the parked result, which nothing reads yet. Log the reason (with redaction), and let the owner list and read parked results through the API.
- **Live frontend.** Wire the frontend's live adapter (already built and tested) to the local API, so the UI drives a real broker.
- **macOS backend.** An implementation of the isolation trait for macOS.
- **FIDO2 on real hardware.** The FIDO2 hmac-secret vault factor is tested against a software authenticator. Validate it on physical security keys, and record which ones.
- **Release process.** Version numbers, a changelog, signed and reproducible binaries, and a software bill of materials (SBOM) for each release.
- **Audit export.** Stream the broker's audit and vault-access records to an operator's log pipeline (syslog, or JSON lines for a SIEM) through an audit-sink interface. Exporting adds no read path for a cell or an API caller.
- **External secret backends.** A secret-backend interface, so an operator's secret manager (HashiCorp Vault, a cloud KMS) can hold the real credentials. The egress swap is unchanged: a cell still only ever holds a surrogate.
- **Fleet configuration.** A configuration file, alongside the environment variables, that provisioning tools (Terraform, Kubernetes, Ansible) can manage across many machines; and identity for local API callers beyond a socket's uid.
- **Issue triage.** How outside reports and proposals are labeled, accepted or declined.
