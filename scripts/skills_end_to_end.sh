#!/bin/bash
# One of Hatchling's skills enabled in the app, and used (WO-046): a root
# broker with the skills an install ships (packaging/skills) and the trust
# roots that sign them, the scripted skills model, the app's host, and
# scripts/skills_end_to_end.py, which enables write-a-report as the owner
# would and asks the agent for a report.
#
# Usage, as root, in a disposable PID namespace, from the clone:
#   scripts/skills_end_to_end.sh <app-python> <logs>
# <app-python> is a Python with Playwright. The binaries are in /usr/local/bin
# and the app is built in frontend/dist, as the README's quickstart leaves
# them, and its users exist.
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run as root"; exit 64; }
[ $# = 2 ] || { echo "usage: $0 <app-python> <logs>"; exit 64; }
app_python=$1 logs=$2
mkdir -p "$logs"

home=$(mktemp -d /var/tmp/hatchling-skills-e2e.XXXXXX)
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
# Searchable, not readable: the broker's and the keeper's users reach
# what's inside.
chmod 711 "$home"
mkdir -p -m 700 "$home"/{sock,vault,schedules,skills-enabled}
# The skills and their signer, as scripts/install.sh lays them out: root's,
# and nobody else's to write.
cp -R packaging/skills "$home/skills"
chown -R root:root "$home/skills"
chmod -R u=rwX,go=rX "$home/skills"
install -m 644 -o root -g root packaging/skill-trust-roots "$home/skill-trust-roots"

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
export HATCHLING_SKILLS_DIR=$home/skills
export HATCHLING_SKILL_TRUST_ROOTS=$home/skill-trust-roots
export HATCHLING_SKILL_ENABLEMENTS_DIR=$home/skills-enabled
export HATCHLING_CHANNEL_INGRESS=$home/ingress.sock
export HATCHLING_API_SOCKET=$home/api.sock
export HATCHLING_UI_SOCKET=$home/ui.sock
export HATCHLING_SCHEDULER_MODEL_ENDPOINT=http://localhost:18092/v1
export HATCHLING_SCHEDULER_MODEL_NAME=skills
export HATCHLING_EGRESS_ALLOWLIST=local-keyless:localhost:18092
export HATCHLING_EGRESS_GRANTS=localhost:18092
export HATCHLING_API_GRANTS=egress:localhost:18092,channel-send:local

python3 scripts/skills_model.py 18092 2> "$logs/model.log" &
model=$!
hatchling-broker migrate-vault > "$logs/migrate.log" 2>&1
hatchling-broker > "$logs/broker.log" 2>&1 &
broker=$!
for _ in $(seq 100); do [ -S "$HATCHLING_UI_SOCKET" ] && break; sleep 0.1; done
[ -S "$HATCHLING_UI_SOCKET" ] || { echo "the broker didn't start"; cat "$logs/broker.log"; exit 1; }
# Each found, verified under the shipped signer, and waiting for the owner.
for skill in write-a-report tidy-a-folder research-a-question; do
  for _ in $(seq 100); do
    grep 'skill found, waiting for the owner' "$logs/broker.log" | grep -q "\"$skill\"" && break
    sleep 0.1
  done
  grep 'skill found, waiting for the owner' "$logs/broker.log" | grep -q "\"$skill\"" \
    || { echo "the broker didn't find $skill waiting"; cat "$logs/broker.log"; exit 1; }
done
HATCHLING_HOST_UI=$PWD/frontend/dist hatchling-host > "$logs/host.log" 2>&1 &
host=$!
address=
for _ in $(seq 100); do
  address=$(grep -oE "http://127\.0\.0\.1:[0-9]+/#hatchling-ui=[0-9a-f]+" "$logs/host.log" | head -1 || true)
  [ -n "$address" ] && break
  sleep 0.1
done
[ -n "$address" ] || { echo "the host printed no address"; cat "$logs/host.log"; exit 1; }
HATCHLING_E2E_SCREENSHOTS=$logs "$app_python" scripts/skills_end_to_end.py "$address"
