"""Gives an installed Hatchling state to keep across an upgrade, then checks it
kept it. CI's upgrade job installs the last release, seeds it, upgrades to the
commit under test, and verifies.

Usage:
  python3 scripts/upgrade_state.py seed <api socket> <record file>
  python3 scripts/upgrade_state.py verify <api socket> <record file>

`seed` schedules work through the local API, as the app does, with the
connection's own grants, and writes what it made to <record file>. `verify`
lists the schedules after the upgrade and fails unless each one seeded is
there with the same timing and grants, still due.
"""
import json
import os
import socket
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "docs", "examples"))
from hello import receive, send  # noqa: E402

# How long to wait for a broker that was just started to make its socket.
START_WAIT_SECS = 60

# Far enough ahead that no seeded schedule fires during the upgrade.
SEEDED = [
    {"when": {"every": "cron", "expr": "0 8 * * 1-5", "tz": "America/New_York"},
     "task": "Upgrade check: say the word kept."},
    {"when": {"every": "cron", "expr": "30 6 1 1 *", "tz": "Europe/Berlin"},
     "task": "Upgrade check: a yearly reminder."},
]


def connect(path, wait=START_WAIT_SECS):
    """A connection to the API socket at `path`, once there is one."""
    deadline = time.monotonic() + wait
    while True:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            sock.connect(path)
            return sock
        except (FileNotFoundError, ConnectionRefusedError):
            sock.close()
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.2)


def session(path):
    """A welcomed API connection, and the grants the welcome named (none
    from a broker too old to say)."""
    sock = connect(path)
    send(sock, {"type": "hello", "schema_version": 4, "client": "upgrade_state.py", "token": None})
    welcome = receive(sock)
    if welcome.get("type") != "welcome":
        raise SystemExit(f"handshake failed: {welcome}")
    return sock, welcome.get("grants")


def ask(sock, request_id, op):
    """The broker's answer to a schedule `op`."""
    send(sock, {"type": "action", "request_id": request_id,
                "action": {"action": "manage_schedule", "op": op}})
    sock.settimeout(30)
    while True:
        frame = receive(sock)
        if frame.get("type") == "action_result" and frame.get("request_id") == request_id:
            result = frame["result"]
            if result.get("result") != "schedule":
                raise SystemExit(f"{op['op']} wasn't answered as a schedule op: {result}")
            return result["outcome"]


def listed(sock):
    """Every schedule the broker lists, every page."""
    schedules, after, page = [], None, 0
    while True:
        page += 1
        op = {"op": "list"} if after is None else {"op": "list", "after": after}
        outcome = ask(sock, f"list-{page}", op)
        if outcome.get("result") != "listed":
            raise SystemExit(f"list failed: {outcome}")
        schedules += outcome["schedules"]
        after = outcome.get("next")
        if not after:
            return schedules


def grant_from_label(label):
    """A grant label from HATCHLING_API_GRANTS, as the API's JSON grant."""
    kind, _, rest = label.partition(":")
    if kind == "egress":
        return {"grant": "egress", "destination": rest}
    if kind == "channel-send":
        return {"grant": "channel-send", "channel": rest}
    raise SystemExit(f"no seed grant for {label}")


def seed(path, record):
    sock, grants = session(path)
    if not grants:
        # A broker from before the welcome named them: the installer's.
        grants = [grant_from_label(label) for label in os.environ["SEED_GRANTS"].split(",")]
    made = []
    for index, wanted in enumerate(SEEDED):
        outcome = ask(sock, f"create-{index}", {"op": "create", "when": wanted["when"], "task": wanted["task"],
                                                "grants": grants, "channel": "local"})
        if outcome.get("result") != "created":
            raise SystemExit(f"create failed: {outcome}")
        made.append({"id": outcome["id"], "when": wanted["when"], "grants": grants})
    before = {s["id"]: s for s in listed(sock)}
    for schedule in made:
        schedule["next_due"] = before[schedule["id"]]["next_due"]
    with open(record, "w", encoding="utf-8") as out:
        json.dump(made, out, indent=2)
    print(f"seeded {len(made)} schedules: {', '.join(s['id'] for s in made)}")
    return 0


def verify(path, record):
    with open(record, encoding="utf-8") as recorded:
        made = json.load(recorded)
    sock, _ = session(path)
    after = {s["id"]: s for s in listed(sock)}
    problems = []
    for schedule in made:
        kept = after.get(schedule["id"])
        if kept is None:
            problems.append(f"{schedule['id']} is gone")
            continue
        for field in ("when", "grants"):
            if kept[field] != schedule[field]:
                problems.append(f"{schedule['id']}: {field} was {schedule[field]}, is {kept[field]}")
        if kept.get("held"):
            problems.append(f"{schedule['id']} is held: {kept['held']}")
        if kept["next_due"] < time.time():
            problems.append(f"{schedule['id']} is no longer due: next_due {kept['next_due']}")
    if problems:
        print("the upgrade lost state:\n  " + "\n  ".join(problems))
        return 1
    print(f"kept all {len(made)} schedules across the upgrade")
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 4 or sys.argv[1] not in ("seed", "verify"):
        raise SystemExit(__doc__)
    sys.exit((seed if sys.argv[1] == "seed" else verify)(sys.argv[2], sys.argv[3]))
