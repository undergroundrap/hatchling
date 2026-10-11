#!/usr/bin/env bash
# Makes every published crate trust the release workflow on crates.io
# (trusted publishing), through crates.io's API. Run by the maintainer, from
# the repository root; see docs/releasing.md.
#
#   scripts/setup-trusted-publishing.sh
#
# It asks for a crates.io API token with only the `trusted-publishing` scope
# (crates.io → Account Settings → API Tokens → New Token, expiring in a day).
# The token is read without echoing and is never written anywhere. Crates
# that already trust this workflow are skipped, so it's safe to re-run after
# adding a crate. Revoke the token afterwards.
set -u
OWNER=undergroundrap
REPO=hatchling
WORKFLOW=release.yml
ENVIRONMENT=release
API=https://crates.io/api/v1/trusted_publishing/github_configs
UA="hatchling trusted-publishing setup (github.com/$OWNER/$REPO)"

CRATES=$(cargo metadata --format-version 1 --no-deps | python3 -c '
import json, sys
meta = json.load(sys.stdin)
print(" ".join(sorted(p["name"] for p in meta["packages"] if p.get("publish") != [])))
') || exit 1
echo "$(echo "$CRATES" | wc -w) crates will trust $OWNER/$REPO, workflow $WORKFLOW, environment $ENVIRONMENT."

# A crates.io token is "cio" and 32 letters and digits. Check the shape of
# what arrived (a paste into a hidden prompt can fail silently), and say
# what came through without ever showing the token.
for attempt in 1 2 3; do
  read -rsp "crates.io token (trusted-publishing scope; hidden, paste then Enter): " TOKEN
  echo
  TOKEN=$(printf '%s' "$TOKEN" | tr -d '[:space:]')
  if [[ "$TOKEN" =~ ^cio[A-Za-z0-9]{32}$ ]]; then
    echo "got a crates.io token (35 characters)"
    break
  fi
  if [ -z "$TOKEN" ]; then
    echo "nothing came through. Paste with right-click or Ctrl+Shift+V, then press Enter."
  else
    echo "that doesn't look like a crates.io token: ${#TOKEN} characters, $( [[ "$TOKEN" == cio* ]] && echo 'starting with cio' || echo 'not starting with cio')."
  fi
  TOKEN=""
done
[ -n "$TOKEN" ] || { echo "no token; nothing was changed"; exit 1; }

# curl reads the token from a header file on a pipe, never from its arguments,
# so it doesn't show in the process list.
api() { curl -sS -A "$UA" -H @<(printf 'Authorization: %s\n' "$TOKEN") "$@"; }

# One call first, so a token crates.io rejects stops here, not 21 times.
first=$(echo "$CRATES" | cut -d' ' -f1)
check=$(api "$API?crate=$first")
if echo "$check" | grep -q '"errors"'; then
  echo "crates.io rejected the token: $(echo "$check" | head -c 200)"
  echo "Check it has the trusted-publishing scope and hasn't been revoked or expired, then run this again."
  exit 1
fi

failed=0
for crate in $CRATES; do
  existing=$(api "$API?crate=$crate") || { echo "! $crate: crates.io didn't answer"; failed=1; continue; }
  if echo "$existing" | python3 -c '
import json, sys
configs = json.load(sys.stdin).get("github_configs", [])
want = sys.argv[1:]
sys.exit(0 if any([c["repository_owner"].lower(), c["repository_name"], c["workflow_filename"], c.get("environment") or ""] == want for c in configs) else 1)
' "$OWNER" "$REPO" "$WORKFLOW" "$ENVIRONMENT" 2>/dev/null; then
    echo "= $crate already trusts the release workflow"
    continue
  fi
  body=$(python3 -c '
import json, sys
crate, owner, repo, workflow, env = sys.argv[1:]
print(json.dumps({"github_config": {"crate": crate, "repository_owner": owner, "repository_name": repo,
                                    "workflow_filename": workflow, "environment": env}}))
' "$crate" "$OWNER" "$REPO" "$WORKFLOW" "$ENVIRONMENT")
  answer=$(api -X POST -H "Content-Type: application/json" --data "$body" "$API")
  if echo "$answer" | grep -q '"github_config"'; then
    echo "+ $crate now trusts the release workflow"
  else
    echo "! $crate: $(echo "$answer" | head -c 300)"
    failed=1
  fi
done
unset TOKEN
echo
if [ "$failed" = 0 ]; then
  echo "Done. Now revoke the token on crates.io (Account Settings → API Tokens)."
else
  echo "Some crates failed (above). Fix the cause and run this again; configured crates are skipped."
  exit 1
fi
