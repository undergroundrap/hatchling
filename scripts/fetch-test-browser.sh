#!/usr/bin/env bash
# Fetches the headless Chromium the browser cell's tests were measured
# with, Chrome for Testing 143.0.7499.4, into <dir>/chrome-headless-shell-linux64,
# and prints the browser's path.
#
# usage: scripts/fetch-test-browser.sh <dir>
#
# It comes from Chrome for Testing's own release, not through Playwright:
# a newer Playwright brings a newer Chromium, which the browser cell has
# never run. The archive and the browser are each checked against their
# SHA-256 before the path is printed; a mismatch prints nothing and fails.
# Moving the cell to a newer Chromium means measuring it again, and new
# pins here.
#
# The browser's system libraries come from `playwright install-deps
# chromium-headless-shell`, run before this.
set -euo pipefail

VERSION=143.0.7499.4
URL="https://storage.googleapis.com/chrome-for-testing-public/$VERSION/linux64/chrome-headless-shell-linux64.zip"
ARCHIVE_SHA256=a9a525cb3832d59a810f78f8ba6c5ed3592a6a488984627f5d827c2a365c8a5a
BROWSER_SHA256=20ffc88be1c6f7dc8351bebfaca965ff9a07617bddf696f0b67ef85e5868b29a

[ "$#" -eq 1 ] || { echo "usage: $0 <dir>" >&2; exit 2; }
for tool in curl unzip sha256sum; do
  command -v "$tool" > /dev/null || { echo "$0 needs $tool" >&2; exit 2; }
done
dir=$1
mkdir -p "$dir"
archive=$(mktemp "$dir/chrome-headless-shell.XXXXXX.zip")
trap 'rm -f "$archive"' EXIT

# Its download has hung before: each try is bounded, and a failed one is
# tried again.
for attempt in 1 2 3; do
  if curl --fail --silent --show-error --location --max-time 300 --output "$archive" "$URL"; then
    break
  fi
  [ "$attempt" = 3 ] && { echo "couldn't fetch $URL" >&2; exit 1; }
  sleep 15
done
echo "$ARCHIVE_SHA256  $archive" | sha256sum --check --quiet >&2

rm -rf "$dir/chrome-headless-shell-linux64"
unzip -q "$archive" -d "$dir"
browser="$dir/chrome-headless-shell-linux64/chrome-headless-shell"
echo "$BROWSER_SHA256  $browser" | sha256sum --check --quiet >&2
echo "$browser"
