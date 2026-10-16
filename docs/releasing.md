# Releasing

How a Hatchling version is cut and published, and how a new crate gets onto crates.io. Most of it is for the maintainer; contributors only need [the rule for new crates](#adding-a-crate).

## How versions work

- **One version for every crate** (lockstep): the root `Cargo.toml`'s `[workspace.package] version`, which every crate inherits, and the matching `[workspace.dependencies]` entries. A release bumps them together, and publishes every crate, changed or not, so every `hatchling-*` crate at one version is known to work together.
- **Before 1.0, a breaking change bumps the minor version** (0.2.0 → 0.3.0); anything else, the patch. The `semver checks` CI job compares each crate's public API with crates.io and says when a change needs the minor bump.
- **`CHANGELOG.md`** has an Unreleased section every pull request adds its line to. At a release it becomes the version's section, and that text is the release notes.
- **Milestones** on GitHub plan each version; [Order of work](workorders/README.md#order-of-work) sets their order.

## Cutting a release

1. **A release pull request:**
   - bump the version in the root `Cargo.toml`: `[workspace.package] version` and every `[workspace.dependencies]` entry (all the same string), then `cargo update --workspace` so `Cargo.lock` follows;
   - rename `## [Unreleased]` in `CHANGELOG.md` to `## [X.Y.Z] - YYYY-MM-DD`, add an empty Unreleased above it, and update the compare links at the bottom;
   - all required checks green, including "crates.io dry run";
   - the [release security checklist](release-security-checklist.md) run, each item's result recorded in the pull request's description.
2. **Merge it**, and wait for `main`'s CI to pass.
3. **Publish a GitHub release** with the tag `vX.Y.Z` on that commit, and the version's changelog section as the notes:

   ```bash
   gh release create vX.Y.Z --target <commit> --title "Hatchling X.Y.Z" --notes-file notes.md
   ```

4. **CI publishes every crate.** The `release` workflow (`.github/workflows/release.yml`) starts when the release is published:
   - it checks that the tag, the workspace version and the changelog agree;
   - it gets a short-lived crates.io token through trusted publishing;
   - it runs [`scripts/publish-crates.sh --existing-only`](../scripts/publish-crates.sh), which publishes each crate in dependency order and skips any already on crates.io at this version.

   No crates.io token is stored anywhere. Watch the run under Actions.
5. **Check:** each crate shows the new version on crates.io, and docs.rs builds its docs. A warning on the run means a crate new to crates.io was skipped; see [Adding a crate](#adding-a-crate).

If the workflow fails partway, the crates it already published stay published (crates.io never deletes a version). Fix the cause, then re-run the job: the script skips what's already there and publishes the rest. A bad version can be **yanked** on crates.io (`cargo yank --version X.Y.Z <crate>`), which stops new projects from picking it up without breaking existing lockfiles; then release a fixed version.

## Trusted publishing setup

Each published crate trusts the release workflow, once, on crates.io:

| Field | Value |
| --- | --- |
| Repository owner | `undergroundrap` |
| Repository name | `hatchling` |
| Workflow filename | `release.yml` |
| Environment | `release` |

[`scripts/setup-trusted-publishing.sh`](../scripts/setup-trusted-publishing.sh) sets it for every crate through crates.io's API, in any order (the settings don't depend on each other):

1. On crates.io, make a token: Account Settings → API Tokens → New Token, expiring in a day, with **only** the `trusted-publishing` scope.
2. From the repository root, on Linux or WSL:

   ```bash
   bash scripts/setup-trusted-publishing.sh
   ```

   It asks for the token without echoing it, skips crates that already trust the workflow, and is safe to re-run.
3. Revoke the token.

By hand instead: the crate's page → **Settings → Trusted Publishing → Add**, with the values above. A crate without it is skipped by the release workflow, with a warning.

## Adding a crate

Most new code is a module in an existing crate, behind a feature if it's optional. A new crate is for code that brings heavy dependencies most users don't need (a cloud or secret-manager client, say) or that ships its own program. [CONTRIBUTING](../CONTRIBUTING.md#adding-a-crate) has the checklist for the pull request.

A development or test crate sets `publish = false` and stops there. A published one ships in the next release, and needs the maintainer once:

1. **The release goes out as usual.** The release workflow publishes everything it can, skips the new crate (and any crate that depends on it), and names them in a warning. crates.io only lets the account owner create a crate, and trusted publishing can't create one.
2. **The first publish, by hand.** On crates.io, make a token: Account Settings → API Tokens → New Token, expiring in a day, scopes `publish-new` and `publish-update`. Then, on a Linux machine or WSL, from a fresh clone of the release tag:

   ```bash
   git clone --branch vX.Y.Z https://github.com/undergroundrap/hatchling hatchling-vX.Y.Z
   ```

   ```bash
   cd hatchling-vX.Y.Z
   ```

   ```bash
   cargo login
   ```

   ```bash
   bash scripts/publish-crates.sh
   ```

   It skips every crate already on crates.io at that version, and publishes the rest in dependency order: the new crate, then whatever was waiting on it. crates.io limits how fast an account can create new crates (on 2026-10-11: a burst of 5, then about one per 10 minutes); the script waits that out and is safe to re-run.
3. **Revoke the token** on crates.io, then run `cargo logout`. Logging out only deletes the local copy; revoking kills it.
4. **Trusted publishing** for the new crate: run `scripts/setup-trusted-publishing.sh` again, as above (it skips the crates already set up). From then on, releases publish it with the rest.

The first publish of all 21 crates (0.1.1, 2026-10-11) went this way, and took about two and a half hours because of that rate limit.
