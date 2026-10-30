#!/bin/bash
# A build approved in the app (WO-027): a broker with a build store and no
# cell of its own, the app's host, and scripts/approval_end_to_end.py, which
# proposes a build on the broker's socket as an agent would, then approves
# it in the app as the owner would.
#
# Usage, as root, in a disposable PID namespace, from the clone:
#   scripts/approval_end_to_end.sh <app-python> <logs>
# <app-python> is a Python with Playwright. The binaries are in /usr/local/bin
# and the app is built in frontend/dist, as the README's quickstart leaves
# them, and its users exist. Without a cell, the broker admits agents on its
# socket by their uid, hatchling-cell's: the proof connects as that uid, and
# that connection is the agent. The owner, and the app, are root: never the
# agent's uid, or the agent could send the owner's messages.
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run as root"; exit 64; }
[ $# = 2 ] || { echo "usage: $0 <app-python> <logs>"; exit 64; }
app_python=$1 logs=$2
mkdir -p "$logs"

home=$(mktemp -d /var/tmp/hatchling-approval-e2e.XXXXXX)
broker=
host=
cleanup() {
  [ -n "$host" ] && kill "$host" 2> /dev/null || true
  [ -n "$broker" ] && kill "$broker" 2> /dev/null || true
  wait 2> /dev/null || true
  rm -rf -- "$home"
}
trap cleanup EXIT
# Searchable, not readable: the vault keeper's user reaches the vault
# inside, and the agent's uid the broker's socket.
chmod 711 "$home"
mkdir -p -m 700 "$home"/{vault,artifacts}
mkdir -m 711 "$home/sock"
export HATCHLING_BROKER_SOCKET=$home/sock/broker.sock
# Anyone may connect; the broker serves the agent's uid alone.
export HATCHLING_BROKER_SOCKET_MODE=666
export HATCHLING_AGENT_UID
HATCHLING_AGENT_UID=$(id -u hatchling-cell)
export HATCHLING_CELL_UID_BASE=$HATCHLING_AGENT_UID
export HATCHLING_OWNER_UID=0
unset HATCHLING_AGENT_BIN
export HATCHLING_VAULT_DIR=$home/vault
export HATCHLING_ARTIFACT_DIR=$home/artifacts
export HATCHLING_CHANNEL_INGRESS=$home/ingress.sock
export HATCHLING_API_SOCKET=$home/api.sock
export HATCHLING_UI_SOCKET=$home/ui.sock
export HATCHLING_API_GRANTS=channel-send:local

hatchling-broker migrate-vault > "$logs/migrate.log" 2>&1
hatchling-broker > "$logs/broker.log" 2>&1 &
broker=$!
for _ in $(seq 100); do [ -S "$HATCHLING_UI_SOCKET" ] && [ -S "$HATCHLING_BROKER_SOCKET" ] && break; sleep 0.1; done
[ -S "$HATCHLING_BROKER_SOCKET" ] || { echo "the broker didn't start"; cat "$logs/broker.log"; exit 1; }
HATCHLING_HOST_UI=$PWD/frontend/dist hatchling-host > "$logs/host.log" 2>&1 &
host=$!
address=
for _ in $(seq 100); do
  address=$(grep -oE "http://127\.0\.0\.1:[0-9]+/#hatchling-ui=[0-9a-f]+" "$logs/host.log" | head -1 || true)
  [ -n "$address" ] && break
  sleep 0.1
done
[ -n "$address" ] || { echo "the host printed no address"; cat "$logs/host.log"; exit 1; }
HATCHLING_E2E_SCREENSHOTS=$logs "$app_python" scripts/approval_end_to_end.py "$address"
