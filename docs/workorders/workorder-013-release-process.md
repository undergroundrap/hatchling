# WO-013: A release process: signed, reproducible binaries and an SBOM

**Status:** DONE (2026-11-02). Every acceptance criterion is met, on the first release through the workflow, 0.5.0 (see Evidence).
**Area:** `.github/workflows/`, `Cargo.toml` (release profile), `docs/`, `README.md`, `CHANGELOG.md`
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
- **crates.io from the release workflow:** the publishing WO-016 sets up by hand moves into the release workflow, through crates.io trusted publishing, so no API token is stored anywhere and crates are published only from a tagged commit.
- **Release notes from the changelog:** the release workflow takes the version's section of `CHANGELOG.md` as the release notes, and refuses to release if that section is missing or still says Unreleased.
- **Docs:** `docs/releasing.md`: how to cut a release, and how to verify one.

**Not in scope:** packages for Linux distributions (deb, rpm), container images, macOS or Windows binaries, and automatic version bumping from commit messages.

## Acceptance criteria

- [x] Pushing a `v*` tag produces a GitHub release with binaries for both targets, `SHA256SUMS`, and a CycloneDX SBOM, with the version's changelog section as its notes.
- [x] `gh attestation verify <binary> --repo undergroundrap/hatchling` succeeds for each released binary, and fails for a binary built anywhere else.
- [x] Two independent builds of the same tag are byte-identical; a CI job compares them and fails on any difference.
- [x] Pushing a `v*` tag publishes the crates to crates.io through trusted publishing, and no crates.io token exists in the repository's secrets.
- [x] `docs/releasing.md` lets a maintainer who has never cut a release do it, and lets a user verify a binary, each step tested on a real release.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

**The release: 0.5.0.** Tag `v0.5.0` on 76003bcf (#273), pushed by the maintainer's account. Release run 41491340635 passed every job: notes, both builds of both targets, the comparison, SBOMs, the GitHub release, and crates.io.

- **The release:**
  - 23 files: 10 programs, 10 SBOMs, 2 build infos and `SHA256SUMS`;
  - its notes are 0.5.0's changelog section;
  - every program needs glibc 2.34 (`build-info-*.txt`).
- **Verified as a user would**, following [Verifying a release](../releasing.md#verifying-a-release), from a fresh download:
  - `sha256sum --check --ignore-missing SHA256SUMS`: OK.
  - `gh attestation verify` with `--repo`, `--signer-workflow undergroundrap/hatchling/.github/workflows/release.yml` and `--source-ref refs/tags/v0.5.0`: exit 0 for `hatchling-broker-x86_64-unknown-linux-gnu`, its SBOM and `SHA256SUMS`.
  - What the attestation shows:
    - signer `release.yml@refs/tags/v0.5.0`, source commit 76003bcf, a GitHub-hosted runner;
    - 23 subjects, whose digests are exactly `SHA256SUMS`'s plus `SHA256SUMS`'s own, so every released file is covered.
- **Refused:**
  - the broker with one byte changed: exit 1, no attestation for its digest;
  - the right file with `--source-ref refs/tags/v0.4.0`: exit 1, "expected SourceRepositoryRef to be refs/tags/v0.4.0, got refs/tags/v0.5.0".
- **The embedded dependency list:** the released broker's `.dep-v0` section holds 113 packages, root `hatchling-broker 0.5.0`, which `cargo audit bin` reads.
- **crates.io:** all 22 publishable crates are at 0.5.0, published by the workflow through trusted publishing. Neither the repository nor its `release` environment has any secret.
- **Protections, set at the release** (the maintainer's call):
  - the rule set "Protect release tags": only an admin creates, moves or deletes a `v*` tag;
  - the `release` environment takes only `v*` tag runs.
- **Reproducing:** each release builds each target twice, on separate runners, from different paths and `CARGO_HOME`s, and compares them. The release went out only because they matched. No one has yet rebuilt one outside CI on an Ubuntu 22.04 machine.

Before the first release:

- **What the comparison needs.** Two release builds of 3f2edc78 in a sandbox, from different checkout paths and `CARGO_HOME`s:
  - with only `CARGO_HOME` remapped, all five programs were byte-identical;
  - with nothing remapped, every program differed, by `CARGO_HOME`'s path in dependencies' panic messages (147 in `hatchling-broker`).

  The checkout's path never appears, since cargo names a workspace's files relative to it. So the build remaps `CARGO_HOME` alone. `SOURCE_DATE_EPOCH` and `codegen-units = 1` weren't needed: nothing in the build reads the time, and the default codegen units gave the same bytes every time.
- **The dry run on GitHub's runners** (pull request #270). Each target was built twice, by `cargo auditable`, on separate runners from different paths and `CARGO_HOME`s, and all ten programs were byte-identical. The SBOMs came out for all ten program/target pairs, and the notes from 0.4.0's section.
- **glibc.** Built on Ubuntu 24.04, `hatchling-broker` and `hatchling` needed glibc 2.39, for the standard library's weak `pidfd_spawnp` and `pidfd_getpid`: the version is required even though the symbols are weak. Built on 22.04, every program needs 2.34.
- **On Ubuntu 22.04** (run 41475110112), both targets' ten programs were byte-identical again, and every one needs glibc 2.34.
- **A broken build is caught.** A hand-break that dropped the `CARGO_HOME` remap (run 41475733338) made all ten programs DIFFERENT. "reproducible builds" failed, and nothing went on to release.
- **`scripts/release-notes.sh`.** Its tests cover:
  - a section alone;
  - the last section ending at the compare links;
  - `--latest`;
  - refusals for a missing, undated, empty or doubled section;
  - usage errors.

  Breaking the dot-escaping in its heading match fails the "dot is a dot" case.

## Notes

- Publishing to crates.io itself (the `hatchling-runtime` front door, the license-check change, the first publish) is WO-016, which comes first. This WO automates it.
- Trusted publishing ties a crate to this repository and workflow, and is set up per crate on crates.io; WO-016's first, manual publish creates the crates it's configured on.
- Reproducibility will likely need `--remap-path-prefix` for the cargo registry and the checkout path, `SOURCE_DATE_EPOCH`, and `codegen-units = 1` in the release profile. Let the byte comparison decide; don't add settings it doesn't need.
