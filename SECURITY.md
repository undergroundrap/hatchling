# Security

Hatchling runs code it doesn't trust (skills, coding harnesses, model-generated code) next to secrets it must protect. Security reports are the most valuable contribution you can make. Thank you for taking the time.

Reviewing Hatchling for your organization? [Hatchling for security reviewers](docs/security/README.md) covers what leaves the machine, what's kept and how, the logs, a hardening checklist and what to do after a compromise.

## Reporting a vulnerability

**Please don't open a public issue, discussion or pull request for a vulnerability.**

Report it privately through GitHub: on this repository's **Security** tab, choose **Report a vulnerability**. Only the maintainers can see the report. If you can't use GitHub, open an issue that says only that you have a security report and need another way to send it, and we'll arrange one.

A useful report has:

- what an attacker controls (a skill, a tool's output, a network response, a local process…) and what they gain;
- the commit you tested, your kernel version (`uname -r`) and distribution;
- steps or a proof of concept, ideally as a failing test;
- whether you've told anyone else.

**What to expect:**

| | Target |
| --- | --- |
| Acknowledgement | within 7 days |
| Our assessment (whether it's a vulnerability, and how severe) | within 14 days |
| A fix released, for a critical or high severity issue | within 30 days of the assessment |
| A fix released, for anything else | in the next release |
| Disclosure | when the fix is released, or 90 days after your report, whichever comes first, unless we agree otherwise |

We'll keep you updated as we fix it, and credit you in the advisory unless you'd rather not be named. Every confirmed vulnerability in a released version gets a GitHub security advisory and a CVE, requested through GitHub, and its fix is named in the changelog.

## Supported versions

| Version | Security fixes |
| --- | --- |
| The latest release (0.7.x today) | Yes: a critical or high severity fix comes as a patch release |
| Earlier 0.x releases | No: upgrade to the latest |
| `main` | Yes: fixes land here first |

Before 1.0, only the latest minor version is supported. Please test against the latest release, or `main`'s latest commit. Releases are signed and reproducible: [Verifying a release](docs/releasing.md#verifying-a-release).

## The threat model

The full model is in [docs/arch/trust-boundary.md](docs/arch/trust-boundary.md). In short: **the broker and the vault are trusted, and everything inside a cell is assumed hostile.** Its [known limitations](docs/arch/trust-boundary.md#known-limitations) are already tracked as work orders; a report that goes beyond one, or shows one is worse than described, is welcome. We consider these in scope, and most interesting:

### Malicious skills and cell code

A skill, a harness CLI or generated code is fully attacker-controlled. It's a vulnerability if such code can:

- leave its cell: read or write host files outside the view it was granted, see other processes, gain a capability, or run a syscall the cell's seccomp filter should block;
- reach the network other than through the egress proxy, or reach a destination its grants don't allow;
- act with grants it wasn't given, or with another cell's or schedule's grants;
- affect the broker beyond its own requests: crash it, stall it for other callers, or corrupt its state.

### Credentials

Real egress and channel credentials must never enter a cell. It's a vulnerability if cell code can:

- obtain a real credential, the vault's contents or its key material;
- forge, replay or redirect a surrogate token, or use it after it expires or for another destination;
- obtain a secret through a `vault:fetch` grant other than the one named.

It's also in scope if the vault at rest can be read or tampered with undetected without its unlock factors, or if a fetched secret reaches the model or a log unredacted.

### The broker and the local API

It's a vulnerability if a local process that isn't the owner can connect to the broker or the local API, or if a caller can do more than its grants allow.

### Untrusted input

Tool output, network responses and documents are data, never instructions. A way to make them bypass the output gate or a confirmation rail, so that untrusted text triggers a state-changing action, is in scope.

## Out of scope

- **Host kernel bugs.** A cell is only as strong as the kernel under it. A kernel exploit is the kernel's to fix; tell its maintainers. (A Hatchling setting that needlessly exposes kernel attack surface *is* in scope.)
- **The owner.** You can read everything Hatchling holds. There is no protection against the account that runs the broker, or root.
- **Physical access**, and a machine that is already compromised.
- **What the model decides to do within its grants.** Grants and confirmation rails bound what an agent *can* do; a model making a poor choice inside them is not a vulnerability.
- **Unsupported setups**: kernels below 5.11, or a build with isolation tests skipped.
- Denial of service that needs the owner's own credentials.

## For contributors

Changes to anything on the trust boundary (isolation, the vault, egress, grants, authentication, the API) need extra review: say so in the PR description. See [CONTRIBUTING.md](CONTRIBUTING.md#security).
