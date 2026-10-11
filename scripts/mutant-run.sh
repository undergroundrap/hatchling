#!/usr/bin/env bash
# A supervised mutation run: pre-flight,
# launch, watchdog, and a ledger entry however the run ends.
#
# Usage: scripts/mutant-run.sh --base COMMIT [--expected N] [--] ARG...
#
# The ARGs go to the mutation command, scripts/mutate-targeted.sh, except
# that a bare `*.rs` path becomes `--file PATH` (a bare path is
# not a file selection). Mutation is per file: cargo-mutants mutates
# every function in a selected file, and has no per-mutant filter
#, so a file's untouched functions are mutated too.
#
# Steps:
#   1. Take .mutation-state/run.lock: one run at a time against a tree.
# 2. Pre-flight: `hatchling-dev preflight --base COMMIT`. A
#      finding refuses the launch: the refusal is recorded in the
#      ledger and nothing starts.
#   3. Launch the run in its own session. Its PID goes in
#      .mutation-state/runs/<run-id>.pid, its output in <run-id>.log.
#   4. Start scripts/mutant-watchdog.sh on that PID file (stall and
#      death alerts, in <run-id>.watchdog.log), and later stop it by its
#      own PID file, <run-id>.watchdog.pid. Nothing here finds a
#      process by name: no `pkill -f`.
#   5. However the run ends (finished, died, killed, or this wrapper
#      interrupted), record it with scripts/mutant-ledger.sh: `complete`
#      only when scripts/mutant-report.sh says every discovered mutant
#      was tested, `partial` otherwise.
#
# Exit status: 0 complete, nothing missed or timed out; 1 complete, but
# the floor failed; 2 partial; 4 refused by pre-flight; 64 usage; 75
# another run holds the lock.
#
# Environment:
#   HATCHLING_DEV_BIN         a built hatchling-dev (default: cargo run)
#   HATCHLING_MUTANTS_CMD     the mutation command (default:
#                             scripts/mutate-targeted.sh)
#   MUTANT_WATCHDOG_INTERVAL  seconds between watchdog checks (default 60)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPTS="$ROOT/scripts"
STATE="$ROOT/.mutation-state"
RUNS="$STATE/runs"

usage() {
  echo "usage: $0 --base COMMIT [--expected N] [--] ARG..." >&2
  exit 64
}

# cargo-mutants options that take a value (cargo-mutants 27).
takes_value() {
  case "$1" in
    -f | --file | -e | --exclude | -F | --re | -E | --exclude-re | -j | --jobs | \
      -t | --timeout | --timeout-multiplier | --build-timeout | \
      --build-timeout-multiplier | --minimum-test-timeout | -d | --dir | \
      -o | --output | -p | --package | --test-package | --test-workspace | \
      --features | --shard | --sharding | -D | --in-diff | -C | --cargo-arg | \
      --cargo-test-arg | --config | --cap-lints | --profile | --copy-target | \
      --copy-vcs | --gitignore | -L | --level | --baseline | --jobserver | \
      --jobserver-tasks | --test-tool | --skip-calls | --skip-calls-defaults | \
      --error | --manifest-path | --annotations | --colors | --line-col | \
      --completions | --emit-schema) return 0 ;;
    *) return 1 ;;
  esac
}

BASE=""
EXPECTED=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --base) [ "$#" -ge 2 ] || usage; BASE="$2"; shift 2 ;;
    --expected) [ "$#" -ge 2 ] || usage; EXPECTED="$2"; shift 2 ;;
    -h | --help) sed -n '2,/^set -euo/{/^#/s/^# \{0,1\}//p}' "$0"; exit 0 ;;
    --) shift; break ;;
    *) break ;;
  esac
done
[ -n "$BASE" ] || usage
[[ "$EXPECTED" =~ ^[0-9]+$ ]] || usage

ARGS=()
FILES=()
prev=""
for arg in "$@"; do
  if [ -n "$prev" ] && takes_value "$prev"; then
    ARGS+=("$arg")
    case "$prev" in -f | --file) FILES+=("$arg") ;; esac
  elif [[ "$arg" == *.rs && "$arg" != -* ]]; then
    ARGS+=(--file "$arg")
    FILES+=("$arg")
  else
    ARGS+=("$arg")
    case "$arg" in --file=*) FILES+=("${arg#--file=}") ;; esac
  fi
  prev="$arg"
done

mkdir -p "$RUNS" "$STATE/ledger"
exec 9>"$STATE/run.lock"
if ! flock -n 9; then
  echo "mutant-run: another run holds $STATE/run.lock; not starting a second" >&2
  exit 75
fi

TREE="$("$SCRIPTS/mutant-ledger.sh" tree-hash)"
while :; do
  NOW="$(date -u +%s)"
  RUN_ID="$(date -u -d "@$NOW" +%Y%m%dT%H%M%SZ)-${TREE:0:8}"
  [ -e "$STATE/ledger/$RUN_ID.json" ] || [ -e "$RUNS/$RUN_ID.log" ] || break
  sleep 1
done
STARTED="$(date -u -d "@$NOW" +%Y-%m-%dT%H:%M:%SZ)"
LOG="$RUNS/$RUN_ID.log"
PIDFILE="$RUNS/$RUN_ID.pid"
WATCHDOG_PIDFILE="$RUNS/$RUN_ID.watchdog.pid"
WATCHDOG_LOG="$RUNS/$RUN_ID.watchdog.log"
PREFLIGHT_LOG="$RUNS/$RUN_ID.preflight.log"

record() { # STATUS EXIT LOG [OUT_DIR]
  "$SCRIPTS/mutant-ledger.sh" record --id "$RUN_ID" --status "$1" --base "$BASE" \
    --started "$STARTED" --exit "$2" --log "$3" ${4:+--out "$4"} -- "${FILES[@]}"
}

echo "mutant-run: $RUN_ID: pre-flight against $BASE"
if [ -n "${HATCHLING_DEV_BIN:-}" ]; then
  PREFLIGHT=("$HATCHLING_DEV_BIN")
else
  PREFLIGHT=(cargo run --quiet --locked -p hatchling-dev --)
fi
if ! (cd "$ROOT" && "${PREFLIGHT[@]}" preflight --base "$BASE" --root "$ROOT") >"$PREFLIGHT_LOG" 2>&1; then
  cat "$PREFLIGHT_LOG"
  record refused "" "$PREFLIGHT_LOG" >/dev/null
  echo "mutant-run: $RUN_ID: REFUSED by pre-flight; nothing was launched (ledger: .mutation-state/ledger/$RUN_ID.json)" >&2
  exit 4
fi

MUTANTS_CMD="${HATCHLING_MUTANTS_CMD:-$SCRIPTS/mutate-targeted.sh}"
# A run is judged only by its own report, never a stale one.
rm -f "$ROOT/mutants.out/outcomes.json" "$ROOT/mutants.out/mutants.json"
cd "$ROOT"
setsid "$MUTANTS_CMD" "${ARGS[@]}" >"$LOG" 2>&1 </dev/null &
RUN_PID=$!
echo "$RUN_PID" >"$PIDFILE"
MUTANT_ROOT="$ROOT" MUTANT_PIDFILE="$PIDFILE" MUTANT_LOG="$LOG" \
  setsid "$SCRIPTS/mutant-watchdog.sh" "$EXPECTED" >"$WATCHDOG_LOG" 2>&1 </dev/null &
echo "$!" >"$WATCHDOG_PIDFILE"
echo "mutant-run: $RUN_ID: run $RUN_PID launched: ${ARGS[*]} (log: $LOG)"

# Signal a process group by the leader's PID file.
# The PID file is written right after the fork, before the child has run
# setsid: until it does, there is no group with that id, and a signal sent
# to the group is lost (the run would then outlive an interrupt until the
# KILL below). So fall back to the process itself, which has not started
# anything yet.
signal_group() { # SIGNAL PIDFILE
  local pid
  pid="$(cat "$2" 2>/dev/null)" || return 0
  [ -n "$pid" ] || return 0
  kill "-$1" -- "-$pid" 2>/dev/null || kill "-$1" "$pid" 2>/dev/null || true
}

INTERRUPTED=""
trap 'INTERRUPTED=1; signal_group TERM "$PIDFILE"' INT TERM HUP
set +e
wait "$RUN_PID"
EXIT=$?
if [ -n "$INTERRUPTED" ] && kill -0 "$RUN_PID" 2>/dev/null; then
  # Interrupted while the run went on: it has had TERM; give it 10
  # seconds, then KILL its group, and take its real exit status.
  (sleep 10 && signal_group KILL "$PIDFILE") &
  KILLER=$!
  wait "$RUN_PID"
  EXIT=$?
  kill "$KILLER" 2>/dev/null
fi
set -e
trap - INT TERM HUP
signal_group TERM "$WATCHDOG_PIDFILE"

REPORT_STATUS=0
"$SCRIPTS/mutant-report.sh" "$ROOT/mutants.out" || REPORT_STATUS=$?
if [ "$REPORT_STATUS" -le 1 ]; then STATUS=complete; else STATUS=partial; fi
record "$STATUS" "$EXIT" "$LOG" "$ROOT/mutants.out" >/dev/null
if [ -s "$WATCHDOG_LOG" ]; then
  echo "watchdog:"
  sed 's/^/  /' "$WATCHDOG_LOG"
fi
echo "mutant-run: $RUN_ID: ledger .mutation-state/ledger/$RUN_ID.json"
if [ "$STATUS" = partial ]; then
  echo "mutant-run: $RUN_ID: PARTIAL: the run ended (exit $EXIT${INTERRUPTED:+, wrapper interrupted}) before every discovered mutant was tested; recorded as partial" >&2
  exit 2
fi
exit "$REPORT_STATUS"
