"""Memory and goals across an upgrade from 0.9.0, on a real install (WO-047).

CI's upgrade job installs the last release and gives it what 0.9.0 kept
in the clear: a goal set through the local API, as the app sets one, and
a remembered item and a tracking log written in 0.9.0's layout, as its
broker wrote them (the API can't make either: the agent does). After the
upgrade, the app still lists the goal and the item, no word of any of the
three is left in a file under Hatchling's state, and the plaintext layout
is gone.

Usage:
  python3 scripts/sealed_drill.py seed-api <api socket> <record file>
  sudo python3 scripts/sealed_drill.py seed-files <state dir>
  python3 scripts/sealed_drill.py verify-api <api socket> <record file>
  sudo python3 scripts/sealed_drill.py verify-files <state dir>
"""
import json
import os
import pwd
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from upgrade_state import session  # noqa: E402

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "docs", "examples"))
from hello import receive, send  # noqa: E402

# Words nothing else writes: what must be on no disk after the upgrade.
GOAL = "Drill goal: paddle the lapis-blue kayak"
KEPT = "Drill memory: the owner keeps a lapis-blue kayak"
TRACKED = "Drill tracking: paddled the lapis-blue kayak 5 km"
WORDS = "lapis-blue kayak"

# 0.9.0's layout: who the broker ran as, and whose memory the owner's is.
BROKER_USER = "hatchling-broker"
AGENT_USER = "hatchling-cell"
KEPT_ID = "kept-0d71a1e5c0ffee00"


def act(sock, request_id, action, op, answered_as):
    """The broker's answer to `op` of the API action `action`."""
    send(sock, {"type": "action", "request_id": request_id,
                "action": {"action": action, "op": op}})
    sock.settimeout(30)
    while True:
        frame = receive(sock)
        if frame.get("type") == "action_result" and frame.get("request_id") == request_id:
            result = frame["result"]
            if result.get("result") != answered_as:
                raise SystemExit(f"{action} wasn't answered as {answered_as}: {result}")
            return result["outcome"]


def seed_api(path, record):
    sock, _ = session(path)
    outcome = act(sock, "goal-create", "manage_goals",
                  {"op": "create", "title": GOAL, "detail": ""}, "goals")
    if outcome.get("result") != "saved":
        raise SystemExit(f"the goal wasn't set: {outcome}")
    with open(record, "w", encoding="utf-8") as out:
        json.dump({"goal": outcome["goal"]["id"]}, out)
    print(f"set goal {outcome['goal']['id']}")
    return 0


def owned_dir(path, uid, gid):
    """`path` and what's missing above it, `0700`, as a store makes them."""
    if not os.path.isdir(path):
        owned_dir(os.path.dirname(path), uid, gid)
        os.mkdir(path, 0o700)
    os.chown(path, uid, gid)
    os.chmod(path, 0o700)


def owned_file(path, data, uid, gid):
    with open(path, "wb") as out:
        out.write(data)
    os.chown(path, uid, gid)
    os.chmod(path, 0o600)


def seed_files(state):
    broker = pwd.getpwnam(BROKER_USER)
    uid, gid = broker.pw_uid, broker.pw_gid
    agent = str(pwd.getpwnam(AGENT_USER).pw_uid)
    now = int(time.time())
    kept_dir = os.path.join(state, "memory", "user", agent)
    owned_dir(kept_dir, uid, gid)
    item = {"id": KEPT_ID, "scope": "User", "content": KEPT, "tags": ["recall", "approved"],
            "created_at_secs": now, "updated_at_secs": now, "retention": "Keep"}
    owned_file(os.path.join(kept_dir, f"{KEPT_ID}.json"), json.dumps(item).encode(), uid, gid)
    log_dir = os.path.join(state, "goals", "tracking", agent)
    owned_dir(log_dir, uid, gid)
    entry = {"seq": 0, "at_secs": now, "source": "user", "text": TRACKED}
    owned_file(os.path.join(log_dir, "log.ndjson"), (json.dumps(entry) + "\n").encode(), uid, gid)
    print(f"wrote 0.9.0's memory item and tracking log for uid {agent}")
    return 0


def verify_api(path, record):
    with open(record, encoding="utf-8") as recorded:
        goal = json.load(recorded)["goal"]
    sock, _ = session(path)
    problems = []
    goals = act(sock, "goal-list", "manage_goals", {"op": "list"}, "goals")
    if not any(g["id"] == goal and g["title"] == GOAL for g in goals.get("goals", [])):
        problems.append(f"the goal {goal} isn't listed: {goals}")
    kept = act(sock, "kept-list", "manage_memory", {"op": "list"}, "memory")
    if not any(k["content"] == KEPT for k in kept.get("kept", [])):
        problems.append(f"the remembered item isn't listed: {kept}")
    if problems:
        print("the upgrade lost memory or goals:\n  " + "\n  ".join(problems))
        return 1
    print("the goal and the remembered item are kept across the upgrade")
    return 0


def holds(path, needle):
    """Whether the file at `path` holds `needle`, read a chunk at a time:
    the agent's room disk is gigabytes."""
    tail = b""
    with open(path, "rb") as held:
        while True:
            chunk = held.read(1 << 20)
            if not chunk:
                return False
            if needle in tail + chunk:
                return True
            tail = chunk[-(len(needle) - 1):]


def verify_files(state):
    problems = []
    for gone in (("memory", "user"), ("goals", "goals"), ("goals", "tracking")):
        if os.path.exists(os.path.join(state, *gone)):
            problems.append(f"{os.path.join(*gone)} is still there")
    for kept in (("memory", "sealed"), ("goals", "sealed")):
        if not os.path.isdir(os.path.join(state, *kept)):
            problems.append(f"{os.path.join(*kept)} isn't there")
    for top, _, names in os.walk(state):
        for name in names:
            path = os.path.join(top, name)
            if os.path.islink(path) or not os.path.isfile(path):
                continue
            if holds(path, WORDS.encode()):
                problems.append(f"{path} holds the words in the clear")
    if problems:
        print("memory or goals aren't sealed:\n  " + "\n  ".join(problems))
        return 1
    print(f"no file under {state} holds the words; memory and goals are sealed")
    return 0


COMMANDS = {
    "seed-api": (seed_api, 2),
    "seed-files": (seed_files, 1),
    "verify-api": (verify_api, 2),
    "verify-files": (verify_files, 1),
}

if __name__ == "__main__":
    command = COMMANDS.get(sys.argv[1] if len(sys.argv) > 1 else "")
    if command is None or len(sys.argv) != 2 + command[1]:
        raise SystemExit(__doc__)
    sys.exit(command[0](*sys.argv[2:]))
