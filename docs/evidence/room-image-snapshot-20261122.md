# Evidence: the Room image is built from Debian's 2026-11-22 snapshot (#581)

What was found, how the new pins were derived, and what proves them, for
[#581](https://github.com/undergroundrap/hatchling/issues/581), found by
the v0.9.0 release security checklist (item 8: the advisories for the Room
image's SBOM).

| | |
|---|---|
| Base | `main` at `b8ab7ca3` |
| Release candidate read | the `release` workflow's run on `b8ab7ca3`: image SHA-256 `6b11c802f31a57281b6b2042fcb0b989f3c035d174bb5f437eee24d2a4b384cf`, snapshot `20261107T000000Z` |
| Date | 2026-11-22 (UTC) |
| Advisory sources | OSV (`Debian:13` by source package, `PyPI`), Node's security-wg index |

This record was added after the change, in its own commit. It changes no code.

## The problem

The image's SBOM lists 206 components: 199 Debian packages from 149
source versions (read from the image's own dpkg status, since Debian's
advisories are filed by source package), 5 Python wheels, Node 24.22.2 and
the supervisor. Four Debian packages had a Debian 13 security update newer
than the snapshot:

| Source | In the image | Fixed in Debian 13 | What it is |
|---|---|---|---|
| zlib | `1:1.3.dfsg+really1.3.1-1+b1` | `+deb13u1` | DSA-6613-1, published on the snapshot's own day: a `gz_write` heap overflow after a failed write, and CPU in `crc32_combine` |
| glibc | `2.41-12+deb13u4` | `+deb13u5` | 13 CVEs: iconv hangs on crafted JIS input, tilde expansion, the resolver, ld.so for setuid programs |
| freetype | `2.13.3+dfsg-1+deb13u1` | `+deb13u2` | a crafted CID font exhausts CPU and memory |
| nghttp2 | `1.64.0-1.1+deb13u1` | `+deb13u2` | nghttpx's proxy, which the Room doesn't use |

The rest, recorded in the release pull request: the wheels have no
advisories; Node 24.22.2 has every one patched; 229 Debian entries across
22 sources are fixed only in Debian's testing (pypdf 5.4.0's crafted-PDF
loops among them; Debian's Pillow 11.1.0 is shadowed by the 13.0.0 wheel,
which Python finds first in `/usr/local`); 175 have no fix anywhere.

## The new pins

Snapshot `20261122T000000Z`. Its `trixie` index is dated 2026-11-21
09:15:44 UTC (the point release) and its `trixie-security` index 2026-11-21
00:53:03 UTC. Each `main/binary-amd64/Packages.xz` was checked against
its `InRelease`'s SHA-256, and holds `libc6` `2.41-12+deb13u5`, `zlib1g`
`+deb13u1`, `libfreetype6` and `libnghttp2-14` `+deb13u2`.

| Pin in `scripts/room-image.sh` | Was | Now |
|---|---|---|
| `snapshot` | `20261107T000000Z` | `20261122T000000Z` |
| `epoch` (the snapshot's date) | `1794009600` | `1795305600` |
| `trixie` `InRelease` | `0584fba3…` | `bfacad4ca61899ce20d4a0d439f29339f66541561b67506364efb88e27dee870` |
| `trixie-security` `InRelease` | `478e896e…` | `3d31c58caa2397eda1af459a0db092bb3b8f5abb31c67e50e1dd491958c2ddb9` |
| `libc6-dev` | `2.41-12+deb13u4` | `2.41-12+deb13u5` |
| `libc.a` | `679612af…` | `c2cf5fc8524706421ab6d6af3d997e9f898dcb7c0a43e676bcaea3d8cf4e0a14` |

`rcrt1.o`, `crti.o` and `crtn.o` hash the same in `+deb13u5`; `gcc-14`,
`libgcc-14-dev` and `binutils-x86-64-linux-gnu` are the same `.deb` in
both snapshots, so their files' pins stand.

**The method reproduces the old pins.** The same fetch at
`20261107T000000Z` gives `0584fba3…` and `478e896e…`, and the same
extraction of `libc6-dev` `+deb13u4` (its `.deb` checked against its
`Packages` SHA-256) gives every old toolchain file hash, `libc.a`'s
`679612af…` among them.

The hashes pin what was fetched; they don't vouch for it. The build's APT
still checks each index's signature against Debian's keyring, so an index
that was tampered with on the way to this machine fails CI rather than
passing it.

## What proves it

This record can't show the image builds. The sandbox has no network and
isn't root. The proof is CI on the pull request, which runs the `release`
workflow because it changes `scripts/room-image.sh`:

- the static supervisor built twice from the new snapshot's toolchain,
  which refuses a build root whose packages or files differ from the pins;
- the image built twice, compared byte for byte, and mounted read-only
  through dm-verity with tampered and missing images refused;
- the Windows setup built with it.

After the merge, the item 8 read is made again on `main`'s new release
candidate, and recorded in the release pull request.
