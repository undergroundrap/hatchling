#!/usr/bin/env bash
# The release's files, as the release workflow makes them. See
# docs/releasing.md.
#
#   scripts/release-artifacts.sh build <out dir>  # this machine's target
#   scripts/release-artifacts.sh sbom <out dir>   # every released target
#
# build: the released programs, each `<program>-<target>`, and
# `build-info-<target>.txt`, the toolchain that built them. The same commit
# built this way on another machine of the same target, from any directory,
# gives the same bytes. The one path a build leaves in a binary is
# CARGO_HOME's, in the dependencies' panic messages, and it's remapped; the
# checkout's files are named relative to it, so its path never shows. The
# release workflow builds each target twice, on separate runners from
# different paths, and refuses to release unless the two agree. It needs the pinned toolchain (rust-toolchain.toml) and
# cargo-auditable, which embeds each binary's dependency list in a section
# of its own (`cargo audit bin` reads it).
#
# sbom: a CycloneDX SBOM per program and target, `<program>-<target>.cdx.json`:
# the crates it's built from, as Cargo.lock pins them. It needs
# cargo-cyclonedx.
set -euo pipefail
[ $# = 2 ] && { [ "$1" = build ] || [ "$1" = sbom ]; } \
  || { echo "usage: scripts/release-artifacts.sh build|sbom <out dir>" >&2; exit 64; }
mode=$1
mkdir -p "$2"
out=$(cd "$2" && pwd -P)
cd "$(dirname "$0")/.."

# The released programs, as package:program, and the released targets.
PROGRAMS="hatchling-broker:hatchling-broker hatchling-agent:hatchling-agent
hatchling-host:hatchling-host hatchling-browser:hatchling-browser hatchling-runtime:hatchling"
TARGETS="x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu"

if [ "$mode" = sbom ]; then
  for target in $TARGETS; do
    cargo cyclonedx --format json --spec-version 1.5 --describe binaries \
      --target "$target" --target-in-filename
    for entry in $PROGRAMS; do
      program=${entry##*:}
      mv "crates/${entry%%:*}/${program}_bin_${target}.cdx.json" "$out/$program-$target.cdx.json"
    done
    find crates -name '*.cdx.json' -delete
  done
  # Every SBOM is of the lockfile's crates: one that resolved others is wrong.
  git diff --exit-code Cargo.lock
  ls -l "$out"
  exit 0
fi

cargo_home=$(cd "${CARGO_HOME:-$HOME/.cargo}" && pwd -P)
target=$(rustc -vV | sed -n 's/^host: //p')
case " $TARGETS " in *" $target "*) ;; *) echo "$target isn't a released target" >&2; exit 1 ;; esac
packages=()
for entry in $PROGRAMS; do packages+=(-p "${entry%%:*}"); done

export RUSTFLAGS="--remap-path-prefix=$cargo_home=/cargo"
cargo auditable build --release --locked "${packages[@]}" --bins

for entry in $PROGRAMS; do
  program=${entry##*:}
  install -m 755 "target/release/$program" "$out/$program-$target"
done

# What built them, so a mismatch between two builds can be traced, and a
# rebuild can match them.
{
  echo "commit: $(git rev-parse HEAD)"
  echo "target: $target"
  echo "runner image: ${ImageOS:-unknown} ${ImageVersion:-}"
  rustc -vV
  cargo -V
  cc --version | head -1
  ld --version | head -1
  # The newest glibc symbol version each program needs: the oldest glibc
  # it runs on.
  for entry in $PROGRAMS; do
    program=${entry##*:}
    echo "$program needs glibc $(objdump -T "$out/$program-$target" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1 | cut -d_ -f2)"
  done
} > "$out/build-info-$target.txt"
cat "$out/build-info-$target.txt"
