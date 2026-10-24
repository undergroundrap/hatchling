#!/bin/bash
# The agent's browser, driven from the app (WO-029): a root broker with a
# browser configured, the scripted browsing model, the app's host, and
# scripts/browser_end_to_end.py using the app as the owner would.
#
# Usage, as root, in a disposable PID namespace, from the clone:
#   scripts/browser_end_to_end.sh <browser> <app-python> <logs>
# <browser> is the pinned headless Chromium the cell runs; <app-python> a
# Python with Playwright for the app's own browser. The binaries are in
# /usr/local/bin and the app is built in frontend/dist, as the README's
# quickstart leaves them, and its users (hatchling-cell, hatchling-keeper,
# hatchling-egress and hatchling-broker) exist.
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run as root"; exit 64; }
[ $# = 3 ] || { echo "usage: $0 <browser> <app-python> <logs>"; exit 64; }
browser=$1 app_python=$2 logs=$3
mkdir -p "$logs"

export HATCHLING_HOME
HATCHLING_HOME=$(mktemp -d /var/tmp/hatchling-browser-e2e.XXXXXX)
# Searchable, not readable: the vault keeper's user reaches the vault inside.
chmod 711 "$HATCHLING_HOME"
mkdir -p -m 700 "$HATCHLING_HOME"/{sock,vault,schedules}
export HATCHLING_BROKER_SOCKET=$HATCHLING_HOME/sock/broker.sock
export HATCHLING_AGENT_UID
HATCHLING_AGENT_UID=$(id -u hatchling-cell)
export HATCHLING_CELL_UID_BASE=$HATCHLING_AGENT_UID
export HATCHLING_AGENT_BIN=/usr/local/bin/hatchling-agent
# The app's host and its browser run as root here, in this script's own
# namespaces: the broker's owner is root, not the user who ran sudo.
export HATCHLING_OWNER_UID=0
export HATCHLING_CELL_ID=cell-01
export HATCHLING_VAULT_DIR=$HATCHLING_HOME/vault
export HATCHLING_SCHEDULER_DIR=$HATCHLING_HOME/schedules
export HATCHLING_CHANNEL_INGRESS=$HATCHLING_HOME/ingress.sock
export HATCHLING_API_SOCKET=$HATCHLING_HOME/api.sock
export HATCHLING_UI_SOCKET=$HATCHLING_HOME/ui.sock
export HATCHLING_SCHEDULER_MODEL_ENDPOINT=http://localhost:18090/v1
export HATCHLING_SCHEDULER_MODEL_NAME=browsing
export HATCHLING_EGRESS_ALLOWLIST=local-keyless:localhost:18090
export HATCHLING_EGRESS_GRANTS=localhost:18090
export HATCHLING_API_GRANTS=egress:localhost:18090,channel-send:local
export HATCHLING_WEB=sites
# The browser: pinned by its bundle's digest, run from a verified snapshot.
export HATCHLING_BROWSER=$browser
export HATCHLING_BROWSER_SHA256
HATCHLING_BROWSER_SHA256=$(hatchling-broker browser-digest "$browser")
export HATCHLING_BROWSER_CONTROLLER=/usr/local/bin/hatchling-browser
export HATCHLING_BROWSER_FONTS=/usr/share/fonts
# A browser cell has a resource group or doesn't start: an empty, delegated
# directory, the broker in a leaf of its own.
cg=/sys/fs/cgroup/hatchling-browser-e2e
mkdir "$cg"
echo '+pids +memory +cpu' > "$cg/cgroup.subtree_control"
mkdir "$cg/broker"
echo $$ > "$cg/broker/cgroup.procs"
export HATCHLING_CELL_CGROUP=$cg

python3 scripts/browsing_model.py 18090 2> "$logs/model.log" &
hatchling-broker migrate-vault > "$logs/migrate.log" 2>&1
hatchling-broker > "$logs/broker.log" 2>&1 &
for _ in $(seq 100); do [ -S "$HATCHLING_UI_SOCKET" ] && break; sleep 0.1; done
HATCHLING_HOST_UI=$PWD/frontend/dist hatchling-host > "$logs/host.log" 2>&1 &
address=
for _ in $(seq 100); do
  address=$(grep -oE "http://127\.0\.0\.1:[0-9]+/#hatchling-ui=[0-9a-f]+" "$logs/host.log" | head -1 || true)
  [ -n "$address" ] && break
  sleep 0.1
done
[ -n "$address" ] || { echo "the host printed no address"; cat "$logs/host.log"; exit 1; }
HATCHLING_E2E_SCREENSHOTS=$logs "$app_python" scripts/browser_end_to_end.py "$address"
