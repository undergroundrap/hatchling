# WO-013: A release process: signed, reproducible binaries and an SBOM

**Status:** IN-PROGRESS. The workflow, the scripts and the docs are in; the criteria that need a real release are checked at the first one (0.5.0).
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

- [ ] Pushing a `v*` tag produces a GitHub release with binaries for both targets, `SHA256SUMS`, and a CycloneDX SBOM, with the version's changelog section as its notes.
- [ ] `gh attestation verify <binary> --repo undergroundrap/hatchling` succeeds for each released binary, and fails for a binary built anywhere else.
- [ ] Two independent builds of the same tag are byte-identical; a CI job compares them and fails on any difference.
- [ ] Pushing a `v*` tag publishes the crates to crates.io through trusted publishing, and no crates.io token exists in the repository's secrets.
- [ ] `docs/releasing.md` lets a maintainer who has never cut a release do it, and lets a user verify a binary, each step tested on a real release.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

So far, before the first release:

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
