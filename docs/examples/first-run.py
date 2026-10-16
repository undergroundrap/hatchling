"""Gives an agent its first task, and prints its answer.

Usage: python3 docs/examples/first-run.py "$HATCHLING_API_SOCKET"

It sends the task on the local chat. The broker answers each message there
with an agent run: the agent, in a fresh cell, given the message as its task,
holding the API's grants (HATCHLING_API_GRANTS), which must include reaching
the model (egress to it) and replying on the local channel. Its answer comes
back here as a chat message, usually within seconds. See docs/local-api.md.
"""
import json
import socket
import sys
import time

from hello import receive, send

TASK = "Introduce yourself in one sentence."


def main(path):
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(path)
    send(sock, {"type": "hello", "schema_version": 4, "client": "first-run.py", "token": None})
    if receive(sock).get("type") != "welcome":
        print("handshake failed")
        return 1

    # Saying something on the local chat also makes this connection the
    # place the agent's replies are delivered.
    send(sock, {"type": "subscribe", "after_seq": None})
    send(sock, {"type": "action", "request_id": "task",
                "action": {"action": "send_message", "chat_id": "local", "text": TASK}})
    print("sent:", TASK)

    sock.settimeout(300)
    deadline = time.monotonic() + 300
    print("waiting for the agent...")
    while time.monotonic() < deadline:
        frame = receive(sock)
        if frame.get("type") == "action_result" and frame.get("request_id") == "task":
            if frame["result"].get("result") != "message_accepted":
                print("message refused:", json.dumps(frame["result"]))
                return 1
        body = frame.get("body", {}) if frame.get("type") == "event" else {}
        if body.get("kind") == "chat_message" and body.get("author") == "agent":
            print("agent:", body["text"])
            return 0
    print("no answer within five minutes; check the broker's log")
    return 1


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        sys.exit(64)
    sys.exit(main(sys.argv[1]))
