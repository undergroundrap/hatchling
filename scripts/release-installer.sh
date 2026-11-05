#!/bin/sh
# Makes the release's installer files in <out-dir>, from this checkout:
#   hatchling-installer.tar.gz  scripts/install.sh and packaging/, the same
#                               bytes from the same commit (sorted, owned by
#                               root, dated 0, gzip without a name or time)
#   get-hatchling.sh            scripts/get.sh, naming <tag> as its version,
#                               so it downloads the files released with it
#
#   scripts/release-installer.sh <out-dir> <tag>
set -eu
[ $# = 2 ] || { echo "usage: scripts/release-installer.sh <out-dir> <tag>" >&2; exit 64; }
OUT=$1 TAG=$2
case "$TAG" in
  v[0-9]*) ;;
  *) echo "release-installer.sh: the tag is a version, such as v0.8.0: $TAG" >&2; exit 64 ;;
esac
case "$TAG" in
  *[!A-Za-z0-9._-]*) echo "release-installer.sh: the tag is a version, such as v0.8.0: $TAG" >&2; exit 64 ;;
esac
cd "$(dirname "$0")/.."
mkdir -p "$OUT"
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
    --mode='u+rwX,go+rX,go-w' --format=gnu -cf - scripts/install.sh packaging \
  | gzip -9n > "$OUT/hatchling-installer.tar.gz"
grep -q '^VERSION=latest$' scripts/get.sh || { echo "release-installer.sh: scripts/get.sh has no VERSION=latest line" >&2; exit 1; }
sed "s/^VERSION=latest\$/VERSION=$TAG/" scripts/get.sh > "$OUT/get-hatchling.sh"
grep -q "^VERSION=$TAG\$" "$OUT/get-hatchling.sh"
