# WO-013: A release process: signed, reproducible binaries and an SBOM

**Status:** QUEUED
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

Filled in at close.

## Notes

- Publishing to crates.io itself (the `hatchling-runtime` front door, the license-check change, the first publish) is WO-016, which comes first. This WO automates it.
- Trusted publishing ties a crate to this repository and workflow, and is set up per crate on crates.io; WO-016's first, manual publish creates the crates it's configured on.
- Reproducibility will likely need `--remap-path-prefix` for the cargo registry and the checkout path, `SOURCE_DATE_EPOCH`, and `codegen-units = 1` in the release profile. Let the byte comparison decide; don't add settings it doesn't need.
