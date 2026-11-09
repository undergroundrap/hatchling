# Releasing

How a Hatchling version is cut and published, and how a new crate gets onto crates.io. Most of it is for the maintainer; contributors only need [the rule for new crates](#adding-a-crate).

## How versions work

- **One version for every crate** (lockstep): the root `Cargo.toml`'s `[workspace.package] version`, which every crate inherits, and the matching `[workspace.dependencies]` entries. A release bumps them together, and publishes every crate, changed or not, so every `hatchling-*` crate at one version is known to work together.
- **Before 1.0, a breaking change bumps the minor version** (0.2.0 → 0.3.0); anything else, the patch. The `semver checks` CI job compares each crate's public API with crates.io and says when a change needs the minor bump. A crate not yet on crates.io has nothing to compare with, and is checked from its first release on.
- **`CHANGELOG.md`** has an Unreleased section every pull request adds its line to. At a release it becomes the version's section, and that text is the release notes.
- **Milestones** on GitHub plan each version; [Order of work](workorders/README.md#order-of-work) sets their order.

## Cutting a release

1. **A release pull request:**
   - bump the version in the root `Cargo.toml`: `[workspace.package] version` and every `[workspace.dependencies]` entry (all the same string), then `cargo update --workspace` so `Cargo.lock` follows;
   - rename `## [Unreleased]` in `CHANGELOG.md` to `## [X.Y.Z] - YYYY-MM-DD`, add an empty Unreleased above it, and update the compare links at the bottom;
   - name the new version in [Install](install.md#install)'s clone and download commands, and the supported release in `SECURITY.md` (`git grep -n 'vA.B.C'` for the old version finds them), so the guide on `main` installs the release this pull request makes;
   - all required checks green, including "crates.io dry run", and the `release` workflow's dry run (it runs on a pull request that changes `Cargo.toml` or `Cargo.lock`): both builds of each target the same, the SBOMs made, and the notes taken from the version's section;
   - the [release security checklist](release-security-checklist.md) run, each item's result recorded in the pull request's description.
   - [the scorecard](scorecard.md) updated: every row this release changes done again on a fresh install, with ✅ only for what a person did.
2. **Merge it**, and wait for `main`'s CI to pass.
3. **Tag the merge commit and push the tag.** That's the release. Only an admin can create, move or delete a `v*` tag (the "Protect release tags" rule set), and only a `v*` tag's run may use the `release` environment, the one crates.io trusts:

   ```bash
   git fetch origin && git tag -a vX.Y.Z origin/main -m "Hatchling X.Y.Z"
   ```

   ```bash
   git push origin vX.Y.Z
   ```

4. **CI releases it.** The `release` workflow (`.github/workflows/release.yml`) starts on the tag:
   - **It refuses a bad tag:** one that isn't `vX.Y.Z`, whose commit isn't on `main`, that disagrees with the workspace version, or whose version has no dated, non-empty section in `CHANGELOG.md` ([`scripts/release-notes.sh`](../scripts/release-notes.sh) takes the section).
   - **It builds:**
     - the released programs (`hatchling-broker`, `hatchling-agent`, `hatchling-host`, `hatchling-browser` and `hatchling`) for `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`, with [`scripts/release-artifacts.sh`](../scripts/release-artifacts.sh): the pinned toolchain, `--locked`, and `cargo auditable`;
     - each target twice, on separate runners, from different checkout paths and `CARGO_HOME`s; the release goes out only if the two builds are byte-identical.
   - **It builds the app** (`scripts/release-artifacts.sh ui`) twice, from different paths, into `hatchling-ui.tar.gz`, and refuses to release unless the two are byte-identical.
   - **It builds the Windows setup** from the first x86_64 build and the app, and scans it with Microsoft Defender: a build it flags goes no further.
   - **It builds the x86_64 room system image** twice on separate runners, compares the outputs byte for byte, and exercises a read-only dm-verity mount.
   - **It describes them:** a CycloneDX SBOM per program and target, `SHA256SUMS` of every file, and a build-provenance attestation of each file, SBOMs and `SHA256SUMS` included.
   - **It publishes:**
     - the GitHub release, with those files and the version's changelog section as its notes;
     - every crate on crates.io: each is packaged and built from its package in a job with no token, then a short-lived token through trusted publishing, and [`scripts/publish-crates.sh --existing-only --no-verify`](../scripts/publish-crates.sh), which publishes each crate in dependency order without building it, and skips any already there at this version.

   No crates.io token is stored anywhere, and no job that builds, or runs a dependency's build script or macro, holds a token that can write, sign or publish. Watch the run under Actions.
5. **Check** that the release has its 32 files, including the five room image assets and `SHA256SUMS`. Verify one binary as [below](#verifying-a-release) and check that each crate shows the new version on crates.io and builds on docs.rs. A warning on the crates.io job means a crate new to crates.io was skipped. See [Adding a crate](#adding-a-crate).

If the workflow fails before the GitHub release, nothing was published: fix the cause on `main`, delete the tag (`git push origin :vX.Y.Z`, then `git tag -d vX.Y.Z`) and tag the fixed commit. If it fails in the crates.io job, the crates it already published stay published (crates.io never deletes a version): fix the cause, then re-run the job; the script skips what's already there and publishes the rest. A bad version can be **yanked** on crates.io (`cargo yank --version X.Y.Z <crate>`), which stops new projects from picking it up without breaking existing lockfiles; then release a fixed version.

## Verifying a release

Each release from 0.5.0 on has, for each target (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`):
- the programs, each `<program>-<target>`;
- an SBOM of each, `<program>-<target>.cdx.json`;
- `build-info-<target>.txt`, the toolchain and runner image that built them, and the oldest glibc each runs on (2.34: Debian 12, Ubuntu 22.04, RHEL 9 and later);
- `hatchling-ui.tar.gz`, the app `hatchling-host` serves, built twice like the programs: extract it and point `HATCHLING_HOST_UI` at its `ui` directory;
- from 0.8.0, `hatchling-installer.tar.gz` (`scripts/install.sh` and `packaging/`, the same bytes from the same commit) and `get-hatchling.sh` (`scripts/get.sh`, naming its own tag), which install it with no clone: [Quick install](install.md#quick-install). `scripts/release-installer.sh` makes both;
- and `SHA256SUMS`, of all of them.

**That it's the file the release published:**

```bash
sha256sum --check --ignore-missing SHA256SUMS
```

**That this repository's release workflow built it, from the tag:** with the [GitHub CLI](https://cli.github.com/), for each file:

```bash
gh attestation verify hatchling-broker-x86_64-unknown-linux-gnu --repo undergroundrap/hatchling --signer-workflow undergroundrap/hatchling/.github/workflows/release.yml --source-ref refs/tags/vX.Y.Z
```

It checks the file's SHA-256 against an attestation signed for this repository's workflow, on GitHub's runners, at that tag. A file that any of those differs for fails, including one built anywhere else, unless it's byte-identical to the release's (see the next section). Verifying works on the file under any name, so it can check an installed program too (`/usr/local/bin/hatchling-broker`).

**What's in it:** the SBOM lists every crate the program is built from, with its version and license. The same list is embedded in the binary itself; [`cargo audit`](https://crates.io/crates/cargo-audit) checks it against the RustSec advisories:

```bash
cargo audit bin hatchling-broker-x86_64-unknown-linux-gnu
```

### Room system image

From 0.9, the x86_64 room image is released as `room-system-x86_64.squashfs` with its `.verity` hash tree, `.root-hash`, `.contents.txt` package inventory, and CycloneDX `.sbom.json`. The image uses SquashFS gzip compression for WSL compatibility. `SHA256SUMS` and the release attestations cover all five. Verify the image and its root hash against the release's attestation before using that root to authenticate data. The root hash is the trust anchor for dm-verity. The format uses SHA-256, 4096-byte data and hash blocks, verity format 1 without a superblock, and the image SHA-256 as salt.

The builder uses signed Debian 13 archives from a dated snapshot, the hash-pinned upstream Node LTS binary, and the hash-pinned Python wheels in `scripts/room-image-wheels.lock`. It checks the exact signed indexes used by APT before running package scripts. APT checks the package digests through those indexes. Debian supplies the remaining libraries and retains their package and copyright records. The package inventory records both Debian's Pillow package and the newer wheel used by the Python interpreter for `pdfplumber`. Refresh the snapshot and upstream hashes with release updates. The build requires x86_64 Ubuntu 26.04 with `mmdebstrap`, `debian-archive-keyring`, `squashfs-tools`, and `cryptsetup-bin`. CI logs their versions.

Two independent CI jobs compile the unpublished room supervisor as a static x86-64 GNU binary from the locked workspace. A fresh image job for each build receives only the ELF and copies it as data. The image contains it at the fixed root-owned path `/usr/local/libexec/hatchling-room`. The contents inventory and SBOM both record its SHA-256, and the image's verity root covers the executable. The Rust toolchain and Cargo inputs are pinned. CI records the Ubuntu native compiler, linker and library versions because those affect the static binary. For the room artifacts, the byte-for-byte check covers the two CI builds with the logged runner inputs, and a local replay can produce different digests.

To replay the image jobs in a disposable Linux environment, give a dedicated unprivileged `room-builder` user two independent checkouts of the same commit, the pinned Rust toolchain, a native linker and binutils. Use separate, trusted checkouts of the same commit for the root image commands. The build user must have no sudo access and no write access to those trusted checkouts. Transfer only the two built executables into the image stage. Run the kernel probe in a private mount and network namespace.

```bash
sudo -H -u room-builder bash -lc 'cd /path/to/build-a && bash scripts/room-image-supervisor.sh /tmp/supervisor-a'
sudo -H -u room-builder bash -lc 'cd /path/to/build-b && bash scripts/room-image-supervisor.sh /tmp/supervisor-b'
staging=$(mktemp -d)
cp -P -- /tmp/supervisor-a/hatchling-room "$staging/room-a"
cp -P -- /tmp/supervisor-b/hatchling-room "$staging/room-b"
(cd /path/to/trusted-image-a && sudo bash scripts/room-image.sh build /tmp/room-a "$staging/room-a")
(cd /path/to/trusted-image-b && sudo bash scripts/room-image.sh build /tmp/room-b "$staging/room-b")
(cd /path/to/trusted-image-a &&
    bash scripts/room-image.sh compare /tmp/room-a /tmp/room-b &&
    sudo unshare --mount --net --propagation private bash scripts/room-image-probe.sh /tmp/room-a)
```

The probe mounts the image read-only with `nodev` and `nosuid`, runs tools and Python libraries from it, and verifies refusal of tampered and missing images. It checks the installed supervisor's ownership, permissions and digest, then exercises the startup handshake as an unprivileged user in a fresh PID namespace. The installer and room launcher will consume these artifacts in their integration slices. In the room, libraries that use `sendmsg` terminate with `SIGSYS` under its syscall filter. The filter refuses `setsockopt`.

## Reproducing a build

The release's programs can be rebuilt byte for byte. On the target's machine, with the runner image `build-info-<target>.txt` names (Ubuntu 22.04: its linker and C library are part of the build), from a clone of the tag:

```bash
git clone --branch vX.Y.Z https://github.com/undergroundrap/hatchling hatchling-vX.Y.Z
```

```bash
cd hatchling-vX.Y.Z && cargo install cargo-auditable --version 0.7.7 --locked
```

```bash
bash scripts/release-artifacts.sh build out
```

Then compare `out/`'s programs with `SHA256SUMS`. The script prints the toolchain it used; if a program differs, compare that with `build-info-<target>.txt` first. The release workflow does this for every release, twice per target.

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

   `cargo login` shows the token on the screen as you paste it, so paste it where nothing records the screen (no screen sharing, recording or screenshot). Run all of this as the user whose Rust toolchain it is, in `bash`; a root shell has neither.

   ```bash
   bash scripts/publish-crates.sh
   ```

   It skips every crate already on crates.io at that version, and publishes the rest in dependency order: the new crate, then whatever was waiting on it. crates.io limits how fast an account can create new crates (on 2026-10-11: a burst of 5, then about one per 10 minutes); the script waits that out and is safe to re-run.
3. **Revoke the token** on crates.io, then run `cargo logout`. Logging out only deletes the local copy; revoking kills it.
4. **Trusted publishing** for the new crate: run `scripts/setup-trusted-publishing.sh` again, as above (it skips the crates already set up). From then on, releases publish it with the rest.

The first publish of all 21 crates (0.1.1, 2026-10-11) went this way, and took about two and a half hours because of that rate limit.
