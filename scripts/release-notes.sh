#!/usr/bin/env bash
# Prints a version's release notes: its section of CHANGELOG.md, without the
# heading. See docs/releasing.md.
#
#   scripts/release-notes.sh X.Y.Z    # the version's notes
#   scripts/release-notes.sh --latest # the newest released version's
#
# Refuses, exit 1, when the section is missing, isn't headed
# `## [X.Y.Z] - YYYY-MM-DD` (still Unreleased, say), or is empty: a release
# without its notes would ship.
set -euo pipefail
cd "$(dirname "$0")/.."

case "${1:-}" in
  --latest)
    version=$(sed -n 's/^## \[\([0-9][^]]*\)\] - .*/\1/p' CHANGELOG.md | head -1)
    [ -n "$version" ] || { echo "CHANGELOG.md has no released version" >&2; exit 1; } ;;
  [0-9]*) version=$1 ;;
  *) echo "usage: scripts/release-notes.sh X.Y.Z | --latest" >&2; exit 64 ;;
esac

# The heading, exactly once, with the version's date.
headings=$(grep -c "^## \[$version\]" CHANGELOG.md || true)
[ "$headings" = 1 ] || { echo "CHANGELOG.md has $headings sections for $version, not 1" >&2; exit 1; }
grep -qE "^## \[${version//./\\.}\] - [0-9]{4}-[0-9]{2}-[0-9]{2}\$" CHANGELOG.md \
  || { echo "CHANGELOG.md's $version heading isn't \`## [$version] - YYYY-MM-DD\`" >&2; exit 1; }

# Its body: up to the next section, or the compare links at the bottom,
# with the blank lines at either end trimmed.
notes=$(awk -v heading="## [$version]" '
  index($0, heading) == 1 { inside = 1; next }
  inside && (/^## \[/ || /^\[[^]]+\]: /) { exit }
  inside { print }
' CHANGELOG.md | sed -e '/./,$!d' | tac | sed -e '/./,$!d' | tac)
[ -n "$notes" ] || { echo "CHANGELOG.md's $version section is empty" >&2; exit 1; }
printf '%s\n' "$notes"
