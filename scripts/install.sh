#!/bin/sh
# Installs Hatchling as a service that starts at boot. See docs/install.md.
#
#   sudo scripts/install.sh --from <dir> --model-endpoint <url> --model <name>
#                           [--ollama-context <tokens>] [--force]
#
# Run it as root, with sudo, from a clone of the version you install: the
# user who ran sudo is the owner, the one the app answers.
#
# <dir> holds the programs, as a release has them (hatchling-broker-<target>
# and so on) or as a build leaves them (target/release), and the app, as
# hatchling-ui.tar.gz or a built directory named ui. When it holds the
# release's SHA256SUMS, every file is checked against it before anything is
# installed, and one that differs stops the install.
#
# The model is an OpenAI-compatible endpoint on this machine, such as
# Ollama's (http://localhost:11434/v1). For a hosted API, install with a
# local one, then follow docs/hosted-models.md.
#
# When that's Ollama, running as its systemd service, it sizes the model's
# window to this machine: 65,536 tokens when the model fits on the GPU with
# it, else 32,768 (Ollama's default can be 4,096, too small for an agent).
# It loads the model at each size and asks how it fits. A window of 65,536
# or more is kept, and so is one of 32,768 or more set by anyone but this
# installer. It writes a drop-in of its own,
# /etc/systemd/system/ollama.service.d/zz-hatchling-context.conf, which it
# names, and which removing undoes. --ollama-context sets a size instead; 0
# leaves Ollama as it is.
#
# It installs:
#   /usr/local/bin/hatchling-{broker,agent,host,browser}
#   /usr/local/share/hatchling/ui                      the app
#   /etc/hatchling/hatchling.env                       the configuration (kept
#                                                      if it's there; --force
#                                                      writes it again)
#   /etc/systemd/system/hatchling.service              the broker, as root
#   /etc/systemd/user/hatchling-host.service           the app's host, as the owner
#   /etc/sysusers.d/hatchling.conf, /etc/tmpfiles.d/hatchling.conf
# then makes the users and directories, seals the vault, starts both
# services, and prints the app's address.
set -eu

usage() {
  echo "usage: sudo scripts/install.sh --from <dir> --model-endpoint <url> --model <name> [--ollama-context <tokens>] [--force]" >&2
  exit 64
}
fail() { echo "install.sh: $*" >&2; exit 1; }
say() { echo "== $*"; }

FROM= ENDPOINT= MODEL= FORCE= OLLAMA_CONTEXT=
while [ $# -gt 0 ]; do
  case "$1" in
    --from) FROM=${2:-}; shift 2 || usage ;;
    --model-endpoint) ENDPOINT=${2:-}; shift 2 || usage ;;
    --model) MODEL=${2:-}; shift 2 || usage ;;
    --ollama-context) OLLAMA_CONTEXT=${2:-}; shift 2 || usage ;;
    --force) FORCE=1; shift ;;
    *) usage ;;
  esac
done
[ -n "$FROM" ] && [ -n "$ENDPOINT" ] && [ -n "$MODEL" ] || usage

[ "$(id -u)" = 0 ] || fail "run it as root, with sudo"
OWNER=${SUDO_UID:-}
case "$OWNER" in
  ''|0) fail "run it with sudo as the user who owns Hatchling, not as root itself" ;;
esac
OWNER_NAME=$(id -nu "$OWNER") || fail "no user with uid $OWNER"
command -v systemctl > /dev/null || fail "Hatchling's service needs systemd"
HERE=$(cd "$(dirname "$0")/.." && pwd -P)
PACKAGING=$HERE/packaging
[ -d "$PACKAGING" ] || fail "no packaging/ beside this script: run it from a clone"

# The model: an http endpoint on this machine, as host:port.
case "$ENDPOINT" in
  http://localhost:*|http://127.0.0.1:*) ;;
  *) fail "--model-endpoint must be http://localhost:<port>/...: a hosted API is set up after, by docs/hosted-models.md" ;;
esac
HOSTPORT=${ENDPOINT#http://}
HOSTPORT=${HOSTPORT%%/*}
case "$HOSTPORT" in
  *:*[!0-9]*|*:) fail "--model-endpoint has no port: $ENDPOINT" ;;
esac
case "$MODEL" in
  *[!A-Za-z0-9._:/-]*) fail "--model has a character a model's name doesn't: $MODEL" ;;
esac
case "$OLLAMA_CONTEXT" in
  ''|0) ;;
  *[!0-9]*) fail "--ollama-context is a number of tokens, or 0: $OLLAMA_CONTEXT" ;;
  *) [ "$OLLAMA_CONTEXT" -ge 8192 ] || fail "--ollama-context is at least 8192, what the agent needs, or 0" ;;
esac

# The files: each program, plain or as a release names it, and the app.
case "$(uname -m)" in
  x86_64) TARGET=x86_64-unknown-linux-gnu ;;
  aarch64|arm64) TARGET=aarch64-unknown-linux-gnu ;;
  *) fail "no release for $(uname -m)" ;;
esac
FROM=$(cd "$FROM" && pwd -P) || fail "no directory $FROM"
found() { # program
  for name in "$1-$TARGET" "$1"; do
    if [ -f "$FROM/$name" ]; then echo "$FROM/$name"; return; fi
  done
  fail "$FROM holds no $1 (or $1-$TARGET)"
}
BROKER=$(found hatchling-broker); AGENT=$(found hatchling-agent)
HOST=$(found hatchling-host); BROWSER=$(found hatchling-browser)
if [ -f "$FROM/hatchling-ui.tar.gz" ]; then UI=$FROM/hatchling-ui.tar.gz
elif [ -d "$FROM/ui" ]; then UI=$FROM/ui
else fail "$FROM holds no app: hatchling-ui.tar.gz, or a built ui directory"
fi
if [ -f "$FROM/SHA256SUMS" ]; then
  say "checking every file against SHA256SUMS"
  (cd "$FROM" && sha256sum --check --ignore-missing --quiet SHA256SUMS) \
    || fail "a file differs from SHA256SUMS: nothing was installed"
  for file in "$BROKER" "$AGENT" "$HOST" "$BROWSER" "$UI"; do
    [ -d "$file" ] && continue
    grep -q "  $(basename "$file")\$" "$FROM/SHA256SUMS" \
      || fail "$(basename "$file") isn't in SHA256SUMS: nothing was installed"
  done
else
  say "no SHA256SUMS in $FROM: the files aren't checked (a build of your own)"
fi

say "installing the programs and the app"
for pair in "$BROKER:hatchling-broker" "$AGENT:hatchling-agent" "$HOST:hatchling-host" "$BROWSER:hatchling-browser"; do
  install -m 755 "${pair%%:*}" "/usr/local/bin/${pair##*:}"
done
mkdir -p /usr/local/share/hatchling
rm -rf /usr/local/share/hatchling/ui
if [ -d "$UI" ]; then cp -R "$UI" /usr/local/share/hatchling/ui
else tar -xzf "$UI" -C /usr/local/share/hatchling
fi
chmod -R a+rX /usr/local/share/hatchling

say "making Hatchling's users and its state directory"
install -D -m 644 "$PACKAGING/sysusers.d/hatchling.conf" /etc/sysusers.d/hatchling.conf
systemd-sysusers /etc/sysusers.d/hatchling.conf
install -D -m 644 "$PACKAGING/tmpfiles.d/hatchling.conf" /etc/tmpfiles.d/hatchling.conf
systemd-tmpfiles --create /etc/tmpfiles.d/hatchling.conf
for dir in sock vault schedules conversation goals memory; do
  [ -d "/var/lib/hatchling/$dir" ] || mkdir -m 700 "/var/lib/hatchling/$dir"
done

ENV=/etc/hatchling/hatchling.env
# What a newer Hatchling keeps by default, added to a configuration kept
# from an older one: each setting only when it isn't there at all (one set
# empty was turned off, and stays off). A store added this way comes with
# the grants its packaged configuration gives it, the chat's among them;
# one already configured is left as it is. The file is backed up first.
KEPT_BY_DEFAULT="HATCHLING_CONVERSATION_DIR HATCHLING_GOALS_DIR HATCHLING_MEMORY_DIR"
keep_up() {
  added= grants=
  for key in $KEPT_BY_DEFAULT; do
    grep -q "^$key=" "$ENV" && continue
    added="$added $key"
    case $key in
      HATCHLING_GOALS_DIR)
        grep -q '^HATCHLING_GOALS_GRANTS=' "$ENV" || added="$added HATCHLING_GOALS_GRANTS"
        grants="$grants goals:" ;;
      HATCHLING_MEMORY_DIR)
        grep -q '^HATCHLING_MEMORY_GRANTS=' "$ENV" || added="$added HATCHLING_MEMORY_GRANTS"
        grants="$grants memory:" ;;
    esac
  done
  [ -n "$added" ] || return 0
  backup="$ENV.before-$(date +%Y%m%d-%H%M%S)"
  cp -p "$ENV" "$backup"
  [ -z "$(tail -c 1 "$ENV")" ] || echo >> "$ENV"
  for key in $added; do
    grep "^$key=" "$PACKAGING/hatchling.env" >> "$ENV"
  done
  for kind in $grants; do
    extra=$(sed -n 's/^HATCHLING_API_GRANTS=//p' "$PACKAGING/hatchling.env" | tr ',' '\n' | grep "^$kind" | paste -sd, -)
    if [ -n "$extra" ] && grep -q '^HATCHLING_API_GRANTS=.' "$ENV"; then
      sed -i "s/^HATCHLING_API_GRANTS=.*/&,$extra/" "$ENV"
      added="$added ($extra for the app and its chat)"
    fi
  done
  say "added what this version keeps for you:$added (the old configuration is $backup)"
}
if [ -f "$ENV" ] && [ -z "$FORCE" ]; then
  say "keeping the configuration in $ENV (--force writes it again)"
  keep_up
else
  say "writing the configuration: $ENV"
  mkdir -p -m 755 /etc/hatchling
  sed -e "s|@OWNER_UID@|$OWNER|" -e "s|@MODEL_ENDPOINT@|$ENDPOINT|" \
      -e "s|@MODEL_NAME@|$MODEL|" -e "s|@MODEL_HOSTPORT@|$HOSTPORT|g" \
      "$PACKAGING/hatchling.env" > "$ENV.new"
  chmod 600 "$ENV.new"
  mv "$ENV.new" "$ENV"
fi

say "installing the services"
install -D -m 644 "$PACKAGING/systemd/hatchling.service" /etc/systemd/system/hatchling.service
SYSTEMD=$(systemctl --version | sed -n '1s/^systemd \([0-9]*\).*/\1/p')
if [ "${SYSTEMD:-0}" -ge 254 ]; then
  mkdir -p /etc/systemd/system/hatchling.service.d
  install -D -m 644 "$PACKAGING/systemd/hatchling-cgroup.conf" /etc/systemd/system/hatchling.service.d/cgroup.conf
else
  rm -f /etc/systemd/system/hatchling.service.d/cgroup.conf
  say "systemd $SYSTEMD has no DelegateSubgroup: cells run without a cgroup of their own (README, Cell limits)"
fi
install -D -m 644 "$PACKAGING/systemd/hatchling-host.service" /etc/systemd/user/hatchling-host.service

if [ -z "$(ls -A /var/lib/hatchling/vault)" ]; then
  say "sealing the vault"
  (set -a; . "$ENV"; set +a; /usr/local/bin/hatchling-broker migrate-vault)
fi

# The model's room: an agent's prompt, its tools and a working history
# fill a small window fast, and a thinking model's answer takes more. A
# local Ollama service gets the largest window the model fits the GPU
# with, of 65,536 and 32,768 (or OLLAMA_CONTEXT, when it's given), unless
# it has as much already; anything else is left as it is, and doctor says
# what it finds.
DROP_IN=/etc/systemd/system/ollama.service.d/zz-hatchling-context.conf
# Loads the model once (only Ollama loads one this way), so its fit can be
# asked and the first answer doesn't wait for it.
load_model() {
  command -v curl > /dev/null || return 1
  curl -fsS -m 300 -o /dev/null "http://$HOSTPORT/api/generate" -H 'Content-Type: application/json' \
    -d "{\"model\":\"$MODEL\",\"keep_alive\":\"10m\"}" 2> /dev/null
}
# Gives Ollama a window of $1 tokens, through the drop-in, and waits for it
# to listen again.
set_window() {
  mkdir -p "$(dirname "$DROP_IN")"
  printf '[Service]\nEnvironment=OLLAMA_CONTEXT_LENGTH=%s\n' "$1" > "$DROP_IN"
  systemctl daemon-reload
  systemctl restart ollama.service
  tries=0
  until ss -Htln "sport = :${HOSTPORT##*:}" | grep -q .; do
    tries=$((tries + 1))
    [ "$tries" -le 30 ] || fail "Ollama didn't come back after its window was set: see journalctl -u ollama, or remove $DROP_IN"
    sleep 1
  done
}
ollama_room() {
  [ "$OLLAMA_CONTEXT" != 0 ] || return 0
  if ! systemctl cat ollama.service > /dev/null 2>&1; then
    say "no Ollama service here: give the model's server a window of 65536 tokens if the model fits the GPU with it, else 32768 (docs/install.md, \"Give the model room\")"
    return 0
  fi
  now=$(systemctl show ollama.service -p Environment --value | tr ' ' '\n' | sed -n 's/^OLLAMA_CONTEXT_LENGTH=//p' | tail -n 1)
  case "$now" in ''|*[!0-9]*) now= ;; esac
  if [ -n "$OLLAMA_CONTEXT" ]; then
    if [ -n "$now" ] && [ "$now" -ge "$OLLAMA_CONTEXT" ]; then
      say "Ollama gives models a $now-token window: kept"
      return 0
    fi
    set_window "$OLLAMA_CONTEXT"
    say "gave Ollama a $OLLAMA_CONTEXT-token window (it had ${now:-its default}): $DROP_IN, which removing undoes"
    return 0
  fi
  # Ours is ours to resize; anyone else's of 32,768 or more is theirs.
  ours=
  grep -qs "^Environment=OLLAMA_CONTEXT_LENGTH=$now\$" "$DROP_IN" && ours=1
  if [ -n "$now" ] && { [ "$now" -ge 65536 ] || { [ -z "$ours" ] && [ "$now" -ge 32768 ]; }; }; then
    say "Ollama gives models a $now-token window: kept"
    return 0
  fi
  say "sizing $MODEL's window to this machine: loading it at 65536 tokens"
  for window in 65536 32768; do
    set_window "$window"
    fit=unknown
    load_model && fit=$(/usr/local/bin/hatchling-broker model-fit "$ENDPOINT" "$MODEL" 2> /dev/null || echo unknown)
    if [ "$fit" = gpu ]; then
      say "gave Ollama a $window-token window: $MODEL fits on the GPU with it ($DROP_IN, which removing undoes)"
      return 0
    fi
    if [ "$window" = 65536 ]; then
      case "$fit" in
        cpu) say "at 65536 tokens $MODEL doesn't fit on the GPU: trying 32768" ;;
        *) say "Ollama doesn't say how $MODEL fits at 65536 tokens: trying 32768, to be safe" ;;
      esac
    fi
  done
  say "gave Ollama a 32768-token window ($DROP_IN, which removing undoes): $MODEL runs partly on the CPU even so, or Ollama doesn't say how it fits"
}
ollama_room

# The model, loaded once now: doctor sees how it fits on this machine, and
# the first answer doesn't wait for it. Only Ollama loads a model this
# way; another server's refusal is said, and doctor checks it anyway.
if command -v curl > /dev/null; then
  say "loading $MODEL, so doctor can see how it fits"
  load_model || say "the model's server didn't load $MODEL (not Ollama, or not ready yet): doctor checks it"
fi

say "checking it can work here"
if ! /usr/local/bin/hatchling-broker doctor --env "$ENV"; then
  fail "fix what doctor found, then run this again: it keeps your configuration"
fi

say "starting the broker"
systemctl daemon-reload
systemctl enable hatchling.service > /dev/null
systemctl restart hatchling.service

say "starting the app's host as $OWNER_NAME"
loginctl enable-linger "$OWNER_NAME"
systemctl --user -M "$OWNER_NAME@" daemon-reload
systemctl --user -M "$OWNER_NAME@" enable hatchling-host.service > /dev/null
systemctl --user -M "$OWNER_NAME@" restart hatchling-host.service

TOKEN=/var/lib/hatchling/ui.sock.token
tries=0
until [ -s "$TOKEN" ]; do
  tries=$((tries + 1))
  [ "$tries" -le 30 ] || fail "the broker didn't start: see journalctl -u hatchling, and sudo hatchling-broker doctor"
  sleep 1
done
echo
echo "Hatchling is running, and starts at boot."
echo "Open the app in a browser on this machine (the address is a key to your agent: don't share it):"
echo
echo "  http://127.0.0.1:7340/#hatchling-ui=$(cat "$TOKEN")"
echo
echo "Each start of the broker makes a new one. To see it again, as $OWNER_NAME:"
echo "  hatchling-host url"
