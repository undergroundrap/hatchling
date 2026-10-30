#!/bin/bash
# The agent's browser, driven from the app (WO-029): a root broker with a
# browser configured, the scripted browsing model, the app's host, and
# scripts/browser_end_to_end.py using the app as the owner would. The site
# the agent's browser visits is scripts/sign_in_site.py, served over HTTPS
# with a CA of the test's own (WO-031): the agent signs in to it with the
# sign-in the owner kept.
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
# The site is on an address of a network namespace of the proof's own, and
# its name in a hosts file of its own mount namespace: it takes both,
# whatever it was run in, and is run again in them. Its process is the
# same, so its own PID marks that it was.
if [ "${HATCHLING_E2E_NAMESPACES:-}" != "$$" ]; then
  exec unshare --mount --propagation private --net -- env "HATCHLING_E2E_NAMESPACES=$$" "$BASH" "$0" "$@"
fi
browser=$1 app_python=$2 logs=$3
mkdir -p "$logs"

export HATCHLING_HOME
HATCHLING_HOME=$(mktemp -d /var/tmp/hatchling-browser-e2e.XXXXXX)
# Searchable, not readable: the vault keeper's user reaches the vault inside.
chmod 711 "$HATCHLING_HOME"
mkdir -p -m 700 "$HATCHLING_HOME"/{sock,vault,schedules,profiles,sign-ins}
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
# The site: a name for a documentation address (RFC 5737), which isn't
# internal, so the browser's relay reaches it as it would a public site, in
# this network namespace.
site_name=$(python3 -c 'import sys; sys.path.insert(0, "scripts"); from browsing_model import BROWSER_URL; print(BROWSER_URL.split("/")[2])')
site_address=198.51.100.7
ip link set lo up
# On an interface of its own, not loopback: with loopback alone, the app's
# browser says it's offline, and holds the owner's messages back.
ip link add site0 type dummy
ip addr add "$site_address/24" dev site0
ip link set site0 up
cp /etc/hosts "$HATCHLING_HOME/hosts"
echo "$site_address $site_name" >> "$HATCHLING_HOME/hosts"
chmod 644 "$HATCHLING_HOME/hosts"
mount --bind "$HATCHLING_HOME/hosts" /etc/hosts
# Its certificate, from a CA the browser is given and nothing else trusts.
tls=$HATCHLING_HOME/tls
mkdir -m 700 "$tls"
# Both carry the extensions a strict verifier asks for, as a real CA's do.
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj "/CN=Hatchling end-to-end CA" \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -keyout "$tls/ca.key" -out "$tls/ca.crt" 2> "$logs/tls.log"
openssl req -newkey rsa:2048 -nodes -subj "/CN=$site_name" -keyout "$tls/site.key" -out "$tls/site.csr" 2>> "$logs/tls.log"
printf '%s\n' "subjectAltName=DNS:$site_name" "basicConstraints=critical,CA:FALSE" \
  "keyUsage=critical,digitalSignature,keyEncipherment" "extendedKeyUsage=serverAuth" \
  "subjectKeyIdentifier=hash" "authorityKeyIdentifier=keyid,issuer" > "$tls/site.ext"
openssl x509 -req -in "$tls/site.csr" -CA "$tls/ca.crt" -CAkey "$tls/ca.key" -CAcreateserial -days 1 \
  -extfile "$tls/site.ext" -out "$tls/site.crt" 2>> "$logs/tls.log"
cat "$tls/site.crt" "$tls/ca.crt" > "$tls/chain.pem"
cp "$tls/ca.crt" "$HATCHLING_HOME/browser-ca.pem"
chmod 644 "$HATCHLING_HOME/browser-ca.pem"
export HATCHLING_BROWSER_CA=$HATCHLING_HOME/browser-ca.pem
# The browser: pinned by its bundle's digest, run from a verified snapshot.
export HATCHLING_BROWSER=$browser
export HATCHLING_BROWSER_SHA256
HATCHLING_BROWSER_SHA256=$(hatchling-broker browser-digest "$browser")
export HATCHLING_BROWSER_CONTROLLER=/usr/local/bin/hatchling-browser
export HATCHLING_BROWSER_FONTS=/usr/share/fonts
# The owner's signed-in sites (WO-030), sealed in a store of their own.
export HATCHLING_BROWSER_PROFILES_DIR=$HATCHLING_HOME/profiles
# The sign-ins the broker fills for them, sealed in a store of their own.
export HATCHLING_BROWSER_CREDENTIALS_DIR=$HATCHLING_HOME/sign-ins
# A browser cell has a resource group or doesn't start: an empty, delegated
# directory, the broker in a leaf of its own.
cg=/sys/fs/cgroup/hatchling-browser-e2e
mkdir "$cg"
echo '+pids +memory +cpu' > "$cg/cgroup.subtree_control"
mkdir "$cg/broker"
echo $$ > "$cg/broker/cgroup.procs"
export HATCHLING_CELL_CGROUP=$cg

# What the site was sent, and every request the model was: read for the
# canary sign-in, and kept out of the logs.
export HATCHLING_E2E_SITE_RECORD=$HATCHLING_HOME/site-sign-ins.jsonl
export HATCHLING_E2E_MODEL_RECORD=$HATCHLING_HOME/model-requests.jsonl
# The session cookie the site sets on a sign-in, new each run: given to the
# site and the check alone, never the broker.
session=$(python3 -c 'import secrets; print("session-" + secrets.token_urlsafe(24))')
python3 scripts/sign_in_site.py "$site_address" 443 "$tls/chain.pem" "$tls/site.key" "$HATCHLING_E2E_SITE_RECORD" \
  "$session" 2> "$logs/site.log" &
python3 scripts/browsing_model.py 18090 "$HATCHLING_E2E_MODEL_RECORD" 2> "$logs/model.log" &
hatchling-broker migrate-vault > "$logs/migrate.log" 2>&1
hatchling-broker > "$logs/broker.log" 2>&1 &
export HATCHLING_E2E_BROKER_PID=$!
export HATCHLING_E2E_BROKER_LOG=$logs/broker.log
export HATCHLING_E2E_VAULT_LOG=$HATCHLING_VAULT_DIR/.access.log
for _ in $(seq 100); do [ -S "$HATCHLING_UI_SOCKET" ] && break; sleep 0.1; done
HATCHLING_HOST_UI=$PWD/frontend/dist hatchling-host > "$logs/host.log" 2>&1 &
address=
for _ in $(seq 100); do
  address=$(grep -oE "http://127\.0\.0\.1:[0-9]+/#hatchling-ui=[0-9a-f]+" "$logs/host.log" | head -1 || true)
  [ -n "$address" ] && break
  sleep 0.1
done
[ -n "$address" ] || { echo "the host printed no address"; cat "$logs/host.log"; exit 1; }
HATCHLING_E2E_SCREENSHOTS=$logs HATCHLING_E2E_SESSION=$session "$app_python" scripts/browser_end_to_end.py "$address"
