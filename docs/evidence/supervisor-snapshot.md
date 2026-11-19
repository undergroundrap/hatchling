# Evidence: the Room's supervisor built from the image's own snapshot (#517)

What was tested, how, and what came back, for building the static Room
supervisor inside a build root made from the Room image's Debian snapshot.

| | |
|---|---|
| Base commit | `11a8f8c0` (`fix(broker): keep giving off after a secret even when that can't be written (#536)`) |
| Date | 2026-11-19 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox; `mmdebstrap` and `debian-archive-keyring` from Ubuntu |
| Toolchain | Rust 1.98.1 (rustup); the C toolchain from Debian snapshot `20261107T000000Z` |

This record was added after the runs, in its own commit. It changes no code.

## The threat

The release says its Room image can be rebuilt byte for byte and checked
against the published one. The static supervisor in it was linked with the
CI runner's own C library. On 2026-11-18 the release's two copies landed on
runner images built 11-03 and 11-11, with `libc6-dev` 2.43-2ubuntu2.4 and 2.7:
their `libc.a`, supervisors and images differed, and the release's check
failed. Once a runner image moves on, nobody can rebuild a released
supervisor, so nobody can check the image's root hash against its source.

## What changed

- **`room-image.sh build-root`** makes a minimal root from the same snapshot,
  with the same pinned, signed indexes, including only `gcc`, `libc6-dev` and
  `binutils`. It checks the packages against pins in the script (binutils
  2.44-3, gcc-14 14.2.0-19, libc6-dev 2.41-12+deb13u4, libgcc-14-dev
  14.2.0-19) and the SHA-256 of every C file a static link takes (`libc.a`,
  `rcrt1.o`, `crti.o`, `crtn.o`, `crtbeginS.o`, `crtendS.o`, `libgcc.a`,
  `libgcc_eh.a`). A build root that differs is refused.
- **`room-supervisor-build.sh`** builds the supervisor in that root:
  - the crates are fetched on the host as the build user, the only step with
    a network;
  - the build runs inside the root as that user (never Cargo as root), with
    no network, in its own mount and PID namespaces, with no privileges to
    gain, and the source and the pinned Rust toolchain read-only;
  - the compiler driver is set to the root's gcc, so the root's GNU linker
    links it, not Rust's bundled `rust-lld`;
  - the link line is kept, and the build fails if it names a file outside the
    build root and the Rust toolchain, or another linker.
- **The release workflow** builds its two copies this way on Ubuntu 26.04 and
  Ubuntu 24.04, deliberately different, and runs the supervisor's proofs
  under its filter against the binary each ships (`HATCHLING_ROOM_SUPERVISOR_BIN`,
  new in the proof test).
- **The image's contents list and SBOM** say what the supervisor was built
  with, from `room-image.sh`'s pins, never from the build job. The SBOM nests
  those packages in the supervisor's component (glibc and libgcc as required,
  since the binary holds them; gcc and binutils as excluded).
- `room-image-supervisor.sh` stays for development builds (CI's tests,
  `root-pass.sh`), marked as such. `docs/releasing.md` and the changelog say
  what changed.

## Results

| Check | Result |
|---|---|
| The build root, made twice from the snapshot | the same packages and the same SHA-256 for every pinned file |
| The supervisor, built twice from different checkouts into different target folders | byte-identical, SHA-256 `ccde66334383b7862c1841e9569f2795418b9669f08941a507957965965abdec`, 2,134,176 bytes |
| The real script's build | the same SHA-256 again |
| Its link line | every file under `/rust` (the Rust toolchain), `/target` (the build's own) or `/usr` (the build root); no `-fuse-ld`: the root's GNU linker |
| `readelf` | static (no `INTERP`, no `NEEDED`), a static PIE; `.comment` says `GCC: (Debian 14.2.0-19)` and `rustc 1.98.1` |
| The supervisor's proofs, against this binary | "three supervisor proofs passed under the room filter" |
| The same proofs, pointed at no binary | fail: the override reaches the test |
| The pin check | refused a build root whose package list didn't match exactly (found while writing it: the list's order) |
| `cargo clippy -p hatchling-room --all-targets --all-features -D warnings` | 0 on 1.98.1 and on 1.85.0 |
| `rustfmt --check` on the changed test | 0 |
| The SBOM script | the supervisor's component nests the four packages; without the record, nothing changes |

The runner-built supervisor of 2026-11-18 was 2,241,392 bytes against Ubuntu's
glibc 2.43; this one is built against Debian's glibc 2.41. The cross-Ubuntu
proof (the two release copies on 26.04 and 24.04 byte-identical, the images
too) is this PR's own release workflow run.

## Residual

- **Rust itself:** rustc and its standard library come from rustup, pinned by
  version and checked by its hashes. That is unchanged, and was never what
  broke.
- **The other released programs** are dynamically linked and still take their
  C startup objects and linker from the runner: #537.
