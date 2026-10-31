#!/bin/bash
# A new conversation, from the app (WO-026): a root broker, the scripted
# turns model, the app's host, and scripts/conversation_end_to_end.py, which
# talks to the agent in the app, starts a new conversation as the owner
# would, and reads what the agent was given each turn.
#
# Usage, as root, in a disposable PID namespace, from the clone:
#   scripts/conversation_end_to_end.sh <app-python> <logs>
# <app-python> is a Python with Playwright. The binaries are in /usr/local/bin
# and the app is built in frontend/dist, as the README's quickstart leaves
# them, and its users exist.
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run as root"; exit 64; }
[ $# = 2 ] || { echo "usage: $0 <app-python> <logs>"; exit 64; }
app_python=$1 logs=$2
mkdir -p "$logs"

home=$(mktemp -d /var/tmp/hatchling-conversation-e2e.XXXXXX)
broker=
host=
model=
cleanup() {
  for pid in "$host" "$broker" "$model"; do
    [ -n "$pid" ] && kill "$pid" 2> /dev/null || true
  done
  wait 2> /dev/null || true
  rm -rf -- "$home"
}
trap cleanup EXIT
# Searchable, not readable: the vault keeper's user reaches the vault inside.
chmod 711 "$home"
mkdir -p -m 700 "$home"/{sock,vault,schedules}
export HATCHLING_BROKER_SOCKET=$home/sock/broker.sock
export HATCHLING_AGENT_UID
HATCHLING_AGENT_UID=$(id -u hatchling-cell)
export HATCHLING_CELL_UID_BASE=$HATCHLING_AGENT_UID
export HATCHLING_AGENT_BIN=/usr/local/bin/hatchling-agent
# The app's host and its browser run as root here: the broker's owner is
# root, not the user who ran sudo.
export HATCHLING_OWNER_UID=0
export HATCHLING_CELL_ID=cell-01
export HATCHLING_VAULT_DIR=$home/vault
export HATCHLING_SCHEDULER_DIR=$home/schedules
export HATCHLING_CHANNEL_INGRESS=$home/ingress.sock
export HATCHLING_API_SOCKET=$home/api.sock
export HATCHLING_UI_SOCKET=$home/ui.sock
export HATCHLING_SCHEDULER_MODEL_ENDPOINT=http://localhost:18092/v1
export HATCHLING_SCHEDULER_MODEL_NAME=turns
export HATCHLING_EGRESS_ALLOWLIST=local-keyless:localhost:18092
export HATCHLING_EGRESS_GRANTS=localhost:18092
export HATCHLING_API_GRANTS=egress:localhost:18092,channel-send:local

python3 scripts/turns_model.py 18092 2> "$logs/model.log" &
model=$!
hatchling-broker migrate-vault > "$logs/migrate.log" 2>&1
hatchling-broker > "$logs/broker.log" 2>&1 &
broker=$!
for _ in $(seq 100); do [ -S "$HATCHLING_UI_SOCKET" ] && break; sleep 0.1; done
[ -S "$HATCHLING_UI_SOCKET" ] || { echo "the broker didn't start"; cat "$logs/broker.log"; exit 1; }
HATCHLING_HOST_UI=$PWD/frontend/dist hatchling-host > "$logs/host.log" 2>&1 &
host=$!
address=
for _ in $(seq 100); do
  address=$(grep -oE "http://127\.0\.0\.1:[0-9]+/#hatchling-ui=[0-9a-f]+" "$logs/host.log" | head -1 || true)
  [ -n "$address" ] && break
  sleep 0.1
done
[ -n "$address" ] || { echo "the host printed no address"; cat "$logs/host.log"; exit 1; }
HATCHLING_E2E_SCREENSHOTS=$logs "$app_python" scripts/conversation_end_to_end.py "$address"
