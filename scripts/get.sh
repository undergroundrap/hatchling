#!/bin/sh
# Gets Hatchling and installs it as a service, in one command, with nothing
# but curl: no clone, no GitHub CLI. See docs/install.md#quick-install.
#
#   curl -fsSLO https://github.com/undergroundrap/hatchling/releases/latest/download/get-hatchling.sh
#   sh get-hatchling.sh
#
# Read it first: it's short, and it asks before anything big.
#
# Run it as yourself, not as root: it asks for sudo once, to install. It
#   1. checks this machine can run Hatchling (Linux with systemd, x86_64 or
#      aarch64);
#   2. downloads the release's programs, app and installer into a directory
#      of its own, and checks every file against the release's SHA256SUMS:
#      one that differs, or isn't listed, stops it before anything is
#      installed;
#   3. makes sure a model is there to answer: Ollama, on this machine, with
#      the model (qwen3.5:9b unless --model names another), which it offers
#      to pull. Without Ollama it says how to install it, or to use a hosted
#      model, and stops: it doesn't install other people's software for you.
#      With --hosted openrouter there's no model on this machine at all:
#      the installer asks for the provider's key at its own prompt;
#   4. runs the release's scripts/install.sh with sudo, which checks the
#      files again, installs the service, and prints the app's address.
#
# Options:
#   --hosted openrouter     a hosted model, with no GPU or Ollama needed:
#                           OpenRouter's free 1,000,000-token one, unless
#                           --model names another of its models
#   --model <name>          the model, on Ollama (default qwen3.5:9b)
#   --model-endpoint <url>  a model server on this machine other than
#                           Ollama's: it must already have the model
#   --yes                   pull the model without asking
#   --version <tag>         another release than this script's
#   --from-url <url>        where the release's files are, for a mirror
#                           (https://, or file:// for a local copy)
#
# This file, as the release publishes it, names its own version below, so
# the files it downloads are the ones released with it. Installed with one
# model, the other is one command after (use-hosted or use-local), and then
# a switch in the app: docs/hosted-models.md.
set -eu

REPO=undergroundrap/hatchling
VERSION=latest
# Ollama's address: OLLAMA_HOST, as Ollama itself reads it, when it's one
# on this machine; else its default.
case "${OLLAMA_HOST:-}" in
  http://localhost:[0-9]*|http://127.0.0.1:[0-9]*) OLLAMA=${OLLAMA_HOST%/}/v1 ;;
  localhost:[0-9]*|127.0.0.1:[0-9]*) OLLAMA=http://${OLLAMA_HOST%/}/v1 ;;
  *) OLLAMA=http://localhost:11434/v1 ;;
esac
PORT=${OLLAMA#http://}
PORT=${PORT%/v1}
PORT=${PORT#*:}
case "$PORT" in
  ''|*[!0-9]*) OLLAMA=http://localhost:11434/v1 ;;
esac
DEFAULT_MODEL=qwen3.5:9b

say() { echo "== $*"; }
fail() { echo "get-hatchling: $*" >&2; exit 1; }
usage() {
  echo "usage: sh get-hatchling.sh [--hosted openrouter] [--model <name>] [--model-endpoint <url>] [--yes] [--version <tag>] [--from-url <url>]" >&2
  exit 64
}

MODEL= ENDPOINT= YES= FROM_URL= HOSTED=
while [ $# -gt 0 ]; do
  case "$1" in
    --hosted) HOSTED=${2:-}; shift 2 || usage ;;
    --model) MODEL=${2:-}; shift 2 || usage ;;
    --model-endpoint) ENDPOINT=${2:-}; shift 2 || usage ;;
    --yes) YES=1; shift ;;
    --version) VERSION=${2:-}; shift 2 || usage ;;
    --from-url) FROM_URL=${2:-}; shift 2 || usage ;;
    -h|--help) sed -n '2,/^set -eu$/{/^#/p}' "$0"; exit 0 ;;
    *) usage ;;
  esac
done
case "$HOSTED" in
  '') [ -n "$MODEL" ] || MODEL=$DEFAULT_MODEL ;;
  openrouter) [ -z "$ENDPOINT" ] || fail "--hosted and --model-endpoint are two models: install with one, then switch" ;;
  *) fail "--hosted names a provider Hatchling knows: openrouter" ;;
esac
case "$MODEL" in
  *[!A-Za-z0-9._:/-]*) fail "--model has a character a model's name doesn't: $MODEL" ;;
esac
case "$VERSION" in
  latest|v[0-9]*) ;;
  *) fail "--version is a release's tag, such as v0.8.0" ;;
esac
case "$VERSION" in
  *[!A-Za-z0-9._-]*) fail "--version is a release's tag, such as v0.8.0" ;;
esac

# 1. This machine.
[ "$(id -u)" != 0 ] || fail "run it as yourself, not as root: it asks for sudo when it installs, and you are the owner the app answers"
[ "$(uname -s)" = Linux ] || fail "Hatchling runs on Linux (on Windows, in WSL2): docs/install.md#what-you-need"
case "$(uname -m)" in
  x86_64) TARGET=x86_64-unknown-linux-gnu ;;
  aarch64|arm64) TARGET=aarch64-unknown-linux-gnu ;;
  *) fail "no release for $(uname -m): Hatchling is built for x86_64 and aarch64" ;;
esac
if [ ! -d /run/systemd/system ]; then
  if grep -qi microsoft /proc/sys/kernel/osrelease 2> /dev/null; then
    fail "systemd isn't running, and Hatchling runs as its service. In WSL, add [boot] systemd=true to /etc/wsl.conf, run \`wsl --shutdown\` in Windows, open this distro again, and run this again."
  fi
  fail "systemd isn't running, and Hatchling runs as its service: docs/install.md#what-you-need"
fi
for tool in curl sha256sum tar sudo; do
  command -v "$tool" > /dev/null || fail "it needs $tool, which isn't installed"
done

# Asks `question`; yes with --yes, or when the answer is empty or y.
ask() {
  [ -z "$YES" ] || return 0
  [ -r /dev/tty ] || fail "$1 Nobody is here to answer: run it again with --yes, or do it first."
  printf '%s [Y/n] ' "$1" > /dev/tty
  read -r answer < /dev/tty || answer=n
  case "$answer" in ''|y|Y|yes|Yes) return 0 ;; *) return 1 ;; esac
}

# 2. The release's files, checked.
if [ -z "$FROM_URL" ]; then
  if [ "$VERSION" = latest ]; then
    FROM_URL=https://github.com/$REPO/releases/latest/download
  else
    FROM_URL=https://github.com/$REPO/releases/download/$VERSION
  fi
fi
case "$FROM_URL" in
  https://*) PROTO='=https' ;;
  file:///*) PROTO='=file' ;;
  *) fail "--from-url is https://, or file:// for a copy on this machine" ;;
esac
DIR=$(mktemp -d "${TMPDIR:-/tmp}/hatchling-get.XXXXXXXX")
trap 'rm -rf "$DIR"' EXIT
trap 'exit 130' INT TERM
chmod 755 "$DIR"
FILES="SHA256SUMS hatchling-broker-$TARGET hatchling-agent-$TARGET hatchling-host-$TARGET hatchling-browser-$TARGET hatchling-ui.tar.gz hatchling-installer.tar.gz"
say "downloading Hatchling ($VERSION, $TARGET) from $FROM_URL"
for file in $FILES; do
  curl --proto "$PROTO" --tlsv1.2 -fsSL --retry 3 -o "$DIR/$file" "$FROM_URL/$file" \
    || fail "couldn't download $file: is $VERSION a release with get-hatchling.sh (0.8.0 or later)?"
done
say "checking every file against the release's SHA256SUMS"
for file in $FILES; do
  [ "$file" = SHA256SUMS ] && continue
  grep -q "  $file\$" "$DIR/SHA256SUMS" || fail "$file isn't in SHA256SUMS: nothing was installed"
done
(cd "$DIR" && sha256sum --check --ignore-missing --quiet SHA256SUMS) \
  || fail "a file differs from SHA256SUMS: nothing was installed"
if command -v gh > /dev/null; then
  echo "   To check who built them too: gh attestation verify $DIR/<file> --repo $REPO"
fi

# The installer: scripts/install.sh and packaging/, plain files and
# directories only (a link could point outside), and nothing outside them.
tar -tzf "$DIR/hatchling-installer.tar.gz" > "$DIR/installer.list" || fail "the installer doesn't read as a tar file"
tar -tvzf "$DIR/hatchling-installer.tar.gz" | cut -c1 | grep -qv '^[-d]$'   && fail "the installer holds something other than files and directories: nothing was installed"
while IFS= read -r entry; do
  case "$entry" in
    /*|*..*) fail "the installer holds $entry, outside its own directory: nothing was installed" ;;
    scripts/install.sh|packaging/*|scripts/|packaging) ;;
    *) fail "the installer holds $entry, which an installer doesn't: nothing was installed" ;;
  esac
done < "$DIR/installer.list"
mkdir "$DIR/installer"
tar -xzf "$DIR/hatchling-installer.tar.gz" -C "$DIR/installer" --no-same-owner --no-same-permissions
[ -f "$DIR/installer/scripts/install.sh" ] && [ -d "$DIR/installer/packaging" ] \
  || fail "the installer has no scripts/install.sh and packaging/"

# 3. A model to answer.
if [ -n "$HOSTED" ]; then
  say "using a hosted model, from $HOSTED: the installer asks for its key"
elif [ -n "$ENDPOINT" ]; then
  say "using the model server at $ENDPOINT: the installer checks it has $MODEL"
else
  ENDPOINT=$OLLAMA
  if command -v nvidia-smi > /dev/null; then
    MIB=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits 2> /dev/null | head -1 | tr -dc 0-9)
    if [ -n "$MIB" ] && [ "$MIB" -lt 7500 ]; then
      echo "   This GPU has $MIB MiB: $MODEL runs partly on the CPU, more slowly. A hosted model is one command after: docs/hosted-models.md"
    fi
  else
    echo "   No NVIDIA GPU found: a local model may run on the CPU, slowly. A hosted model is one command after: docs/hosted-models.md"
  fi
  if ! command -v ollama > /dev/null; then
    fail "Hatchling needs a model to answer, and Ollama isn't installed. Install it from https://ollama.com/download (on Linux, its own script: curl -fsSL https://ollama.com/install.sh | sh), then run this again. Or use a hosted model, with no GPU needed: sh get-hatchling.sh --hosted openrouter (an OpenRouter key; a free one works)."
  fi
  TAGS=$(curl -fsS --max-time 5 "${ENDPOINT%/v1}/api/tags" 2> /dev/null) \
    || fail "Ollama is installed but doesn't answer at ${ENDPOINT%/v1}: start it (sudo systemctl start ollama), then run this again."
  case "$TAGS" in
    *"\"name\":\"$MODEL\""*|*"\"name\":\"$MODEL:latest\""*) say "Ollama has $MODEL" ;;
    *)
      if [ "$MODEL" = "$DEFAULT_MODEL" ]; then size=" (about 6.6 GB)"; else size=; fi
      ask "Ollama doesn't have $MODEL yet. Download it now$size?" \
        || fail "Hatchling needs a model: \`ollama pull $MODEL\`, then run this again."
      say "ollama pull $MODEL"
      ollama pull "$MODEL" || fail "ollama pull $MODEL failed"
      ;;
  esac
fi

# 4. Install, with the release's own installer.
say "installing: sudo asks for your password"
if [ -n "$HOSTED" ]; then
  if [ -n "$MODEL" ]; then
    sudo sh "$DIR/installer/scripts/install.sh" --from "$DIR" --hosted "$HOSTED" --model "$MODEL"
  else
    sudo sh "$DIR/installer/scripts/install.sh" --from "$DIR" --hosted "$HOSTED"
  fi
  echo
  echo "A model on this machine too, for private work or offline? With Ollama and a model,"
  echo "in your own terminal:  sudo hatchling-broker use-local --model qwen3.5:9b"
else
  sudo sh "$DIR/installer/scripts/install.sh" --from "$DIR" --model-endpoint "$ENDPOINT" --model "$MODEL"
  echo
  echo "A hosted model too, with a far larger window? Switch to one in your own terminal,"
  echo "which asks for its key there:  sudo hatchling-broker use-hosted openrouter"
fi
echo "After that, switch between the two in the app, under Settings, General, Model."
