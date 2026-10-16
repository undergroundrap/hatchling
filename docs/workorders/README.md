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

The goal is a runtime a security team can approve, for one person or a whole company. Work is ordered by what it protects, and each step is a version [milestone](https://github.com/undergroundrap/hatchling/milestones):

1. **What a hostile cell can do.** The cell is the adversary the design exists for, so these come first: WO-009 (a key an upstream echoes back reaches the cell; numeric IP forms pass the allowlist), WO-008 (data leaves through a vendor's URL fetching), WO-010 (a cell can cancel the owner's schedules; an API grant check passes vacuously), WO-005 (`socketpair` families and inherited groups), WO-004 (resource limits).
2. **What another local user, or someone with disk access, can do** (v0.3.0 shipped WO-006 and WO-007; v0.4.0 takes the rest): WO-006 (a root broker follows symlinks), WO-007 (any process with the agent's uid can open a session), WO-002 (swapped vault files), WO-003 (key rotation at the keyring tiers), WO-011 (secret copies in memory and core dumps), WO-012 (crash leftovers and safe cleanup), WO-023 (an allowlisted name that resolves to an internal address).
3. **In parallel, the first five minutes:** WO-001 (`--help`, and a quickstart CI runs) and WO-016 (`cargo install hatchling-runtime`, which also holds the crates.io names with real releases). Then seeing it work: WO-021 (the UI drives a real broker), and WO-022 (an operator API, held apart from the user's, with the command center's first screen). These come before the rest of the local-attacker work: approvals delivered to the user, and per-site prompts for the agent's web access, both need the UI.
4. **Then what a security review asks for next:** WO-014 (running without root) and WO-017 (privilege-separating the trusted side, so a bug in the broker can't reach your keys), designed together since they share the spawner; WO-013 (the release process: signed and reproducible binaries, an SBOM, publishing from CI); and WO-015 (root cell tests in CI).
5. **What runs in a cell, by the operator's choice:** WO-018 (a coding CLI added by a manifest, OpenCode first) and WO-019 (tools approved, pinned and listed). Hatchling bundles neither: it makes adding one safe. These can start once the hostile-cell work is done.
6. **Then integrations** for larger deployments: audit export, external secret backends, fleet configuration, and the macOS backend.

## Queue

WO-002 to WO-011 come from the pre-launch review of the trust boundary: see [Known limitations](../arch/trust-boundary.md#known-limitations). The trust-boundary ones get extra review.

| WO | Title | Trust boundary | Status |
| --- | --- | --- | --- |
| [001](workorder-001-help-and-tested-quickstart.md) | `--help` on every binary, a quickstart CI runs, and a hosted-model guide |  | DONE |
| [002](workorder-002-bind-secrets-to-names.md) | Bind sealed secrets to their names | yes | QUEUED |
| [003](workorder-003-keyring-key-rotation.md) | Complete key rotation for keyring vaults | yes | QUEUED |
| [004](workorder-004-cell-resource-limits.md) | Per-cell resource limits | yes | CLOSED |
| [005](workorder-005-seccomp-and-identity-tightening.md) | Tighten the seccomp filter and the cell's groups | yes | CLOSED |
| [006](workorder-006-symlink-safe-ownership.md) | Symlink-safe ownership and staging | yes | CLOSED |
| [007](workorder-007-bind-sessions-to-cells.md) | Bind agent sessions to the cells the broker spawned | yes | CLOSED |
| [008](workorder-008-model-request-policy.md) | A request-body policy for model vendors | yes | CLOSED |
| [009](workorder-009-egress-correctness.md) | Egress correctness fixes | yes | CLOSED |
| [010](workorder-010-schedule-ownership.md) | Schedule ownership and API grant edges | yes | CLOSED |
| [011](workorder-011-secret-hygiene.md) | Secret hygiene in memory and logs | yes | QUEUED |
| [012](workorder-012-disposable-state-and-cleanup.md) | Separate disposable files from your data, and clean them up safely | yes | QUEUED |
| [013](workorder-013-release-process.md) | A release process: signed, reproducible binaries and an SBOM | yes | QUEUED |
| [014](workorder-014-running-without-root.md) | Run the full broker without root | yes | QUEUED |
| [015](workorder-015-root-cell-tests-in-ci.md) | Run the cell test suites as root in CI | yes | QUEUED |
| [016](workorder-016-publish-to-crates-io.md) | Publish to crates.io, with `hatchling-runtime` as the front door | yes | CLOSED |
| [017](workorder-017-privilege-separate-the-trusted-side.md) | Privilege-separate the trusted side: a credential keeper, an egress authority, and a spawner so the broker drops root | yes | QUEUED |
| [018](workorder-018-harness-manifests.md) | Harness manifests: add a coding CLI by configuration, with OpenCode first | yes | QUEUED |
| [019](workorder-019-approved-tools.md) | Approved tools: what a cell can run, pinned and listed | yes | QUEUED |
| [020](workorder-020-model-reasoning-control.md) | Operator-selected model thinking mode | yes | DONE |
| [021](workorder-021-live-frontend.md) | The UI drives a real broker | yes | QUEUED |
| [022](workorder-022-operator-api.md) | An operator API, and the command center's Machine view | yes | QUEUED |
| [023](workorder-023-internal-destinations.md) | Refuse internal addresses for destinations that aren't declared internal | yes | QUEUED |

## Candidates

Not yet WOs. Each gets a full file, with a scope and acceptance criteria, before work starts:

- **Tell cells apart for schedules.** A cell can list and cancel any schedule an agent session made, though never the owner's. Now that every session runs under the cell id the broker gave it (WO-007), record which cell made a schedule and hold list and cancel to that cell.
- **Store directories through a handle.** The memory and goals stores still repair a directory's mode by path after checking it. Make them use `hatchling_sys::ensure_private_dir`, as the vault and scheduled runs do since WO-006.
- **Explain failed runs.** When a scheduled run fails, the broker logs only "the run failed": the reason is sealed in the parked result, which nothing reads yet. Log the reason (with redaction), and let the owner list and read parked results through the API.
- **macOS backend.** An implementation of the isolation trait for macOS.
- **FIDO2 on real hardware.** The FIDO2 hmac-secret vault factor is tested against a software authenticator. Validate it on physical security keys, and record which ones.
- **Approvals delivered to the user, with scoped grants.** Approval requests reach the user in the client, outside the conversation with the agent, so an injected agent can't argue its way to a yes; a grant can be one-time, session-, task- or time-bounded, or standing, and later requests must match its scope exactly. The broker already mints approvals through a path no agent session reaches; this adds the delivery and the scopes (and needs the live frontend, WO-021).
- **Taint-aware egress.** Track which processes in a cell have read the user's data; those lose any automatic allowance for network requests and fall back to approval. Stops quiet exfiltration through requests that would otherwise pass.
- **Inspection on the model path.** A pluggable hook, outside the cell, that inspects requests to and responses from the model (for example a prompt-injection classifier) and can block or flag them; the cell can't turn it off.
- **Audit export.** Stream the broker's audit and vault-access records to an operator's log pipeline (syslog, or JSON lines for a SIEM) through an audit-sink interface. Exporting adds no read path for a cell or an API caller.
- **External secret backends.** A secret-backend interface, so an operator's secret manager (HashiCorp Vault, a cloud KMS) can hold the real credentials. The egress swap is unchanged: a cell still only ever holds a surrogate.
- **Fleet configuration.** A configuration file, alongside the environment variables, that provisioning tools (Terraform, Kubernetes, Ansible) can manage across many machines; and identity for local API callers beyond a socket's uid.
