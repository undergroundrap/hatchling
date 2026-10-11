# WO-013: A release process: signed binaries, an SBOM, and crates.io

**Status:** QUEUED
**Area:** `Cargo.toml` (workspace), `crates/*/Cargo.toml`, `deny.toml`, `.github/workflows/`, `docs/`, `README.md`, `CHANGELOG.md`
**Issue:** [#22](https://github.com/undergroundrap/hatchling/issues/22)
**Trust boundary:** yes

## Problem

Hatchling ships as source only. To run it you clone, build and install two binaries by hand, and nothing lets you check that a binary you were handed was built from this repository. That's the first thing a security team asks about a privileged daemon: where did this binary come from, what's in it, and can I rebuild it myself and get the same bytes?

A release today is also a manual job: someone tags a commit and writes notes. As the project grows, each manual step is a chance to ship the wrong commit or forget a changelog entry.

## Scope

**In scope:**

- **A release workflow**, triggered by a version tag, that builds the release binaries on GitHub's runners for `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` from the tagged commit, with `--locked` and the pinned toolchain.
- **Provenance and signatures:** each binary gets a build-provenance attestation (`actions/attest-build-provenance`), so anyone can run `gh attestation verify` against this repository. A `SHA256SUMS` file is attached to the release and attested the same way.
- **An SBOM per release:** a CycloneDX SBOM of the shipped binaries' dependencies, attached to the release. The binaries are built with `cargo auditable`, so the dependency list is also embedded in each binary.
- **Reproducible builds:** two builds of the same tag on separate runners produce byte-identical binaries, and CI checks that they do. That covers path remapping, `SOURCE_DATE_EPOCH` and anything else the comparison turns up.
- **crates.io:**
  - The installable front door is a new crate, `hatchling-runtime`, which installs a single `hatchling` command. (`hatchling` itself is taken on crates.io by an unrelated project.)
  - The crates it depends on are published under their `hatchling-*` names.
  - Development and test crates (`hatchling-dev`, `hatchling-adversarial`) stay `publish = false`.
  - Publishing runs through crates.io trusted publishing from the release workflow, with no API token stored anywhere.
  - Every published crate shares the workspace version, and each internal crate's docs say it carries no stability promise on its own.
- **Semver checks:** `cargo semver-checks` in CI on the published crates, so an API break without a version bump fails the pull request.
- **Release notes from the changelog:** the release workflow takes the version's section of `CHANGELOG.md` as the release notes, and refuses to release if that section is missing or still says Unreleased.
- **Docs:** `docs/releasing.md` (how to cut a release, how to verify one), and the crates.io and docs.rs badges in the README.

**Not in scope:** packages for Linux distributions (deb, rpm), container images, macOS or Windows binaries, and automatic version bumping from commit messages.

## Acceptance criteria

- [ ] Pushing a `v*` tag produces a GitHub release with binaries for both targets, `SHA256SUMS`, and a CycloneDX SBOM, with the version's changelog section as its notes.
- [ ] `gh attestation verify <binary> --repo undergroundrap/hatchling` succeeds for each released binary, and fails for a binary built anywhere else.
- [ ] Two independent builds of the same tag are byte-identical; a CI job compares them and fails on any difference.
- [ ] On a fresh machine, `cargo install hatchling-runtime` installs a `hatchling` command, and the README's quickstart works with it.
- [ ] No crates.io token exists in the repository's secrets; publishing uses trusted publishing.
- [ ] `cargo deny check` still fails on a third-party AGPL dependency once the workspace crates are publishable (see the notes).
- [ ] A pull request that breaks a published crate's public API without a version bump fails CI; shown by a test PR.
- [ ] `docs/releasing.md` lets a maintainer who has never cut a release do it, and lets a user verify a binary, each step tested on a real release.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- **The license check depends on `publish = false` today.** `deny.toml`'s `[licenses.private] ignore = true` is what exempts our own AGPL crates from the license allowlist, and it applies only to unpublished crates. Once the crates are publishable, exempt them by name instead (`[[licenses.exceptions]]` per `hatchling-*` crate), so a third-party AGPL dependency still fails the gate.
- The workspace comment on `publish = false` says why it exists: an accidental publish would muddy the dual license. This WO makes publishing deliberate, from one workflow, so that reason is met another way.
- Trusted publishing ties a crate to this repository and workflow. Set it up per crate on crates.io before the first release; the first publish of a brand-new crate may need a one-time manual step.
- The front-door `hatchling` command and the existing `hatchling-broker` and `hatchling-agent` binaries need a decision: whether `hatchling` dispatches to them as subcommands, or ships alongside them. Make it here, in the design, before work starts.
