"""Gives an agent its first task, and prints its answer.

Usage: python3 docs/examples/first-run.py "$HATCHLING_API_SOCKET"

It sends the task on the local chat. The broker answers each message there
with an agent run: the agent, in a fresh cell, given the message as its task,
holding the API's grants (HATCHLING_API_GRANTS), which must include reaching
the model (egress to it) and replying on the local channel. Its answer comes
back here as a chat message, usually within seconds. When the run fails,
Hatchling says why as a message of its own, and this prints it and stops.
See docs/local-api.md.
"""
import json
import socket
import sys
import time

from hello import handshake, receive, send

TASK = "Introduce yourself in one sentence."

# How long to wait for a broker that was just started to make its socket.
START_WAIT_SECS = 30


def connect(path, wait=START_WAIT_SECS):
    """A connection to the API socket at `path`, once there is one: the
    broker started a moment ago (`hatchling-broker &`) makes it as it starts."""
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


def main(path):
    sock, welcome = handshake(lambda: connect(path), "first-run.py")
    if sock is None:
        print("handshake failed:", welcome)
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
    # Messages before the task is accepted are the conversation the broker
    # kept, replayed: none of them is about this task.
    accepted = False
    while time.monotonic() < deadline:
        frame = receive(sock)
        if frame.get("type") == "action_result" and frame.get("request_id") == "task":
            if frame["result"].get("result") != "message_accepted":
                print("message refused:", json.dumps(frame["result"]))
                return 1
            accepted = True
        body = frame.get("body", {}) if frame.get("type") == "event" else {}
        if not accepted or body.get("kind") != "chat_message":
            continue
        if body.get("author") == "agent":
            print("agent:", body["text"])
            return 0
        if body.get("author") == "system":
            # Hatchling's own word on the run: it failed, and why.
            print("hatchling:", body["text"])
            return 1
    print("no answer within five minutes; check the broker's log")
    return 1


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        sys.exit(64)
    sys.exit(main(sys.argv[1]))
