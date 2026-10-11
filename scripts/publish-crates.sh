#!/usr/bin/env bash
# Publishes the workspace's crates to crates.io at the checkout's version,
# dependencies first. See docs/releasing.md.
#
# Usage, from a checkout of the release commit:
#   scripts/publish-crates.sh                  # the maintainer, after `cargo login`
#   scripts/publish-crates.sh --existing-only  # the release workflow
#
# - A crate already on crates.io at this version is skipped, so a failed
#   or interrupted run can simply be run again.
# - --existing-only also skips crates that aren't on crates.io at all and
#   lists them: crates.io only lets its owner create a crate, so a new one
#   gets its first publish by hand (the release workflow can't create it).
# - On a crates.io rate limit (new crates: a burst, then about one per ten
#   minutes), it waits ten minutes and tries the same crate again.
#
# Exit status: 0 when everything this run was asked to publish is on
# crates.io; 1 on a publish failure.
set -u
EXISTING_ONLY=false
case "${1:-}" in
  --existing-only) EXISTING_ONLY=true ;;
  "") ;;
  *) echo "usage: scripts/publish-crates.sh [--existing-only]" >&2; exit 64 ;;
esac

# One line per publishable workspace crate, dependencies before dependents:
# "<crate> <version> <its workspace dependencies...>".
PLAN=$(cargo metadata --format-version 1 --no-deps | python3 -c '
import json, sys
meta = json.load(sys.stdin)
pkgs = {p["name"]: p for p in meta["packages"] if p.get("publish") != []}
versions = {p["version"] for p in pkgs.values()}
if len(versions) != 1:
    sys.exit(f"the publishable crates disagree on their version: {sorted(versions)}")
order, seen = [], set()
def deps(name):
    return sorted({d["name"] for d in pkgs[name]["dependencies"]
                   if d["name"] in pkgs and d["kind"] in (None, "build")})
def visit(name):
    if name in seen:
        return
    seen.add(name)
    for dep in deps(name):
        visit(dep)
    order.append(name)
for name in sorted(pkgs):
    visit(name)
for name in order:
    print(name, pkgs[name]["version"], *deps(name))
') || exit 1
VERSION=$(echo "$PLAN" | head -1 | cut -d' ' -f2)
echo "version $VERSION, $(echo "$PLAN" | wc -l) crates"

# Neither cargo call may read stdin: the loop below reads the plan from it.
on_crates_io() { cargo info "$1" --registry crates-io < /dev/null > /dev/null 2>&1; }
LOG=$(mktemp)
trap 'rm -f "$LOG"' EXIT
NEW=""
HELD=""
while read -r crate _ deps; do
  if on_crates_io "$crate@$VERSION"; then
    echo "= $crate $VERSION is already on crates.io"
    continue
  fi
  if $EXISTING_ONLY && ! on_crates_io "$crate"; then
    echo "! $crate is new to crates.io: skipped; it needs its first publish by hand"
    NEW="$NEW $crate"
    continue
  fi
  waits_on=""
  for dep in $deps; do
    case " $NEW $HELD " in *" $dep "*) waits_on="$dep" ;; esac
  done
  if [ -n "$waits_on" ]; then
    echo "! $crate skipped: it depends on $waits_on, which isn't published at $VERSION yet"
    HELD="$HELD $crate"
    continue
  fi
  while true; do
    echo "+ publishing $crate $VERSION"
    if cargo publish -p "$crate" --locked < /dev/null > "$LOG" 2>&1; then
      echo "  ok"
      break
    fi
    if grep -qiE '429|too many|rate limit' "$LOG"; then
      echo "  rate limited by crates.io; waiting 10 minutes ($(date -u +%H:%M) UTC)"
      sleep 600
      continue
    fi
    echo "  FAILED; the log follows. Fix the cause, then run this again."
    tail -30 "$LOG"
    exit 1
  done
  # Let the index catch up before a dependent crate is verified against it.
  for _ in $(seq 1 12); do on_crates_io "$crate@$VERSION" && break; sleep 10; done
done <<< "$PLAN"

if [ -n "$NEW$HELD" ]; then
  message="Not published: new to crates.io:${NEW:- none}; waiting on them:${HELD:- none}. Give the new crates their first publish by hand, then run scripts/publish-crates.sh again (docs/releasing.md, \"Adding a crate\")."
  echo
  echo "$message"
  [ -n "${GITHUB_ACTIONS:-}" ] && echo "::warning::$message"
  exit 0
fi
echo "Done: every crate is on crates.io at $VERSION."
