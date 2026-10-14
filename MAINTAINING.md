# Maintaining Hatchling

Maintainer decisions should be reproducible: explain the scope, record the
evidence, and preserve contributor credit. Contributor requirements are in
[CONTRIBUTING.md](CONTRIBUTING.md); release steps are in
[docs/releasing.md](docs/releasing.md).

## Triage and labels

Aim to acknowledge new public issues and PRs within one or two days.
Ask for missing reproduction details and say what happens next. Private
security reports follow [SECURITY.md](SECURITY.md), including its response
expectations. Labels describe the work; several may apply together.

| Label | Apply when |
| --- | --- |
| `accessibility` | A barrier affects people using assistive technology or other accessibility needs. |
| `bug` | Reported behavior is wrong; request a reproduction when needed. |
| `documentation` | The change improves or corrects documentation. |
| `duplicate` | An existing issue or PR covers the same work; link it and give a one-line reason when closing. |
| `enhancement` | A proposal adds a capability or improves existing behavior. |
| `good first issue` | The task is small and fully specified, with clear acceptance criteria and no unresolved design decision. |
| `help wanted` | Outside help is welcome and the task is available; update it when someone takes ownership. |
| `invalid` | After clarification, the report is not an actionable issue for this project; explain why. |
| `question` | Someone needs an answer or more information is needed to triage the report. |
| `wontfix` | The proposal is declined or outside the accepted scope; close with a one-line reason. |
| `trust boundary` | Work touches isolation, secrets, egress, grants, authentication or permissions; it needs the extra reviews below. |
| `work order` | Core work has a numbered spec in `docs/workorders/`. |

## Issues, work orders and milestones

1. Reuse an existing WO when its fixed scope fits. Otherwise take the next
   unused number from [the queue](docs/workorders/README.md) and write a
   spec from [the template](docs/workorders/TEMPLATE.md). Never reuse a number.
   Focused fixes and docs may use a normal PR without a WO.
2. Give every WO a tracking issue titled `WO-NNN: <title>`. Link the full
   spec from the issue and the issue from the spec; GitHub issue numbers
   and WO numbers are separate sequences.
3. Apply `work order`, `trust boundary` when relevant, and other useful
   labels. Choose an existing milestone according to
   [Order of work](docs/workorders/README.md#order-of-work); only assign a
   release once its scope is agreed. Keep the PR's relevant labels and
   milestone consistent with its tracking issue.
4. Record ownership on the issue before implementation. Move the WO and
   its queue row from `QUEUED` to `IN-PROGRESS`. Keep scope fixed; a new
   problem gets separate work, not an unannounced addition.
5. Partial PRs reference the issue without closing it. The completing PR
   says `Closes #N` and names the WO. Its WO has checked acceptance
   criteria, inline evidence (commands, environment and actual counts),
   `CLOSED` status and a matching queue row. A dropped WO keeps its reason.

Close a milestone when all of its included work is complete. A completed
security subset alone does not finish a release's other commitments.

## Review and merge

Read the whole diff, including workflow and dependency changes. Before
approving a first-time contributor's workflow run, inspect what the run
will execute; approval lets that code run on the project's CI resources.

Trust-boundary changes need two independent review passes before merge.
A reviewer who authored or edited the change cannot supply an independent
verdict. Record the reviewed commit and findings; review corrections again.
Use [Security fixes](CONTRIBUTING.md#security-fixes) and
[testing patterns](docs/testing-patterns.md) to assess the proof.

The `Protect main` rules currently require these six checks:

- `gate (1.98.1)` and `gate (1.85.0)`;
- `protocol drift`;
- `validate`;
- `crates.io dry run`;
- `cla`.

Inspect semver checks and CodeQL too, and resolve failing checks before
merge. Keep this list aligned with the live branch rules. Maintainer
access is not a reason to bypass checks, the CLA or review.

Squash-merge with subject `type(scope): summary (#N)` and delete the topic
branch. Merge the reviewed head, not a later unreviewed push. Confirm the
merged commit's tree equals the tested tree with `git rev-parse <commit>^{tree}`.
If the base changed, reconcile it and verify the resulting tree before merge.
Preserve outside contributors' authorship and coauthor credit; a squash
must not replace their author email with the maintainer's.

## Outside contributors and the CLA

Acknowledge the PR within a day or two. The CLA bot asks each contributor
to sign [CLA.md](CLA.md) once and records signatures on `cla-signatures`.
The `cla` check must pass before merging, alongside review and other checks.

For an unsigned PR, send one friendly reminder after about a week. After
about two weeks, check for a signature or an ongoing conversation; if
neither exists, close with thanks and explain that signing can reopen the
path to merge. These are response targets, not automatic closure timers.
Once the contributors sign and review/checks are complete, merge promptly.

Keep available work with its contributor while waiting. If a declined or
unsigned contribution's problem still needs fixing, write a fresh,
independent implementation from the problem description. Do not copy or
cherry-pick the unsigned patch, or rewrite it to evade the CLA. Apply the
normal review and proof requirements to the replacement too.

## Changelog

Review the changelog for every PR. Add one concise entry for its
user-visible change under `## [Unreleased]`; when there is no user-visible
change, state that reason in the PR as the template permits. Name the WO
when one exists, and describe the user's before/after behavior.

- **Fixed:** behavior that was wrong now works correctly.
- **Added:** a new capability.
- **Changed:** existing behavior, configuration or an API changes.

Mark breaking changes under **Changed** with `**Breaking, <crate>:**` and
say what callers or operators must change. Several affected crates may be
named together. Avoid entries that only list functions or files edited.
At release, Unreleased becomes the version entry and a new empty Unreleased
is added; follow [the release procedure](docs/releasing.md#cutting-a-release).

## Security alerts and private reports

Triage CodeQL and Dependabot alerts. Fix a confirmed problem through the
normal reviewed PR process, or dismiss it with a written technical reason
in the alert; retain the audit trail. A red check is investigated, not
silently retried or bypassed. CodeQL currently uses GitHub's default setup.

For example, [CodeQL alert 83](https://github.com/undergroundrap/hatchling/security/code-scanning/83)
was dismissed because the logged value is a vault entry's filename, not its
sealed contents. Its dismissal records that distinction. For a test-key
alert, verify the value is a fixture with no operational authority; a file
being under `tests/` alone is not a dismissal reason. Keep actual secrets
out of public issues and dismissal explanations.

Review Dependabot update PRs, manifests and lockfiles with the same gates;
a bot author does not make a dependency update safe by itself. Coordinate
private vulnerability reports and disclosure through
[SECURITY.md](SECURITY.md) and GitHub security advisories, rather than a
public tracking issue.

## Branch hygiene

`main` and the CLA bot's `cla-signatures` are the long-lived branches.
Use topic branches for work and delete them after a verified merge.
Remove a local topic branch only after checking its work is preserved.
Leave open contributor branches with their authors. Do not delete, rewrite
or merge `cla-signatures` into `main`: it stores the signature record.
