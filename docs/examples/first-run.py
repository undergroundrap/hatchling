"""Gives an agent its first task, and prints its answer.

Usage: python3 docs/examples/first-run.py "$HATCHLING_API_SOCKET" [model port or host:port]

It schedules a one-off task two minute boundaries ahead, with exactly two grants:
reaching the model (egress to the destination) and replying on the local
channel. The broker runs the agent in a fresh cell when the task is due, and
its answer comes back here as a chat message. The scheduler ticks once a
minute, so this takes one to three minutes. See docs/local-api.md.
"""
import datetime
import json
import socket
import struct
import sys
import time

from hello import receive, schedule, send

TASK = "Introduce yourself in one sentence."


def parse_destination(value):
    """Keep the local-port example, and accept a hosted host:port grant."""
    host, separator, port = value.rpartition(":")
    if not separator:
        host, port = "localhost", value
    if not host or ":" in host or not port.isascii() or not port.isdecimal():
        raise ValueError("Expected a model port or host:port destination")
    number = int(port)
    if not 1 <= number <= 65535:
        raise ValueError("Model port must be between 1 and 65535")
    return f"{host}:{number}"


def main(path, destination):
    grants = [{"grant": "egress", "destination": destination},
              {"grant": "channel-send", "channel": "local"}]
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(path)
    send(sock, {"type": "hello", "schema_version": 4, "client": "first-run.py", "token": None})
    if receive(sock).get("type") != "welcome":
        print("handshake failed")
        return 1

    due = datetime.datetime.now(datetime.timezone.utc).replace(second=0, microsecond=0) + datetime.timedelta(minutes=2)
    created = schedule(sock, "task", {
        "op": "create",
        "when": {"every": "once", "at": due.strftime("%Y-%m-%dT%H:%M"), "tz": "UTC"},
        "task": TASK, "grants": grants, "channel": "local",
    })
    if created.get("outcome", {}).get("result") != "created":
        print("create refused:", json.dumps(created))
        return 1
    print(f"scheduled for {due:%H:%M} UTC: {TASK}")

    # Saying something on the local chat makes this connection the place
    # the agent's replies are delivered.
    send(sock, {"type": "action", "request_id": "chat",
                "action": {"action": "send_message", "chat_id": "local", "text": "Hi!"}})
    send(sock, {"type": "subscribe", "after_seq": None})

    sock.settimeout(300)
    deadline = time.monotonic() + 300
    print("waiting for the agent (one to three minutes)...")
    while time.monotonic() < deadline:
        frame = receive(sock)
        if frame.get("type") == "action_result" and frame.get("request_id") == "chat":
            if frame["result"].get("result") != "message_accepted":
                print("chat refused:", json.dumps(frame["result"]))
                return 1
        body = frame.get("body", {}) if frame.get("type") == "event" else {}
        if body.get("kind") == "chat_message" and body.get("author") == "agent":
            print("agent:", body["text"])
            return 0
    print("no answer within five minutes; check the broker's log")
    return 1


if __name__ == "__main__":
    if len(sys.argv) not in (2, 3):
        print(__doc__.strip().splitlines()[2])
        sys.exit(64)
    try:
        model_destination = parse_destination(sys.argv[2] if len(sys.argv) == 3 else "18089")
    except ValueError as error:
        print(str(error), file=sys.stderr)
        sys.exit(64)
    sys.exit(main(sys.argv[1], model_destination))
