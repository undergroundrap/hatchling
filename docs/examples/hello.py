"""A minimal local API client, using only Python's standard library.

Usage: python3 docs/examples/hello.py "$HATCHLING_API_SOCKET"

It shakes hands with the broker, schedules a weekday-morning task, lists
the schedules back, and cancels the one it made. The broker needs
HATCHLING_API_GRANTS=channel-send:local. See docs/local-api.md.
"""
import json
import socket
import struct
import sys

GRANTS = [{"grant": "channel-send", "channel": "local"}]

# The local API's version these examples speak first.
SCHEMA_VERSION = 5


def send(sock, value):
    """Writes one frame: a 4-byte big-endian length, then UTF-8 JSON."""
    data = json.dumps(value, separators=(",", ":")).encode()
    sock.sendall(struct.pack(">I", len(data)) + data)


def receive(sock):
    """Reads one frame."""
    def exactly(n):
        buf = b""
        while len(buf) < n:
            chunk = sock.recv(n - len(buf))
            if not chunk:
                raise EOFError("the broker closed the connection")
            buf += chunk
        return buf
    (length,) = struct.unpack(">I", exactly(4))
    return json.loads(exactly(length))


def handshake(connect, client):
    """A welcomed connection, from `connect` (which opens a new one), and
    the welcome; or no connection, and the broker's answer. A broker of
    another version says which it speaks, and is asked again in it: what
    these examples ask for is the same in every version they meet."""
    version = SCHEMA_VERSION
    answer = None
    for _ in range(2):
        sock = connect()
        send(sock, {"type": "hello", "schema_version": version, "client": client, "token": None})
        answer = receive(sock)
        if answer.get("type") == "welcome":
            return sock, answer
        sock.close()
        if answer.get("type") != "incompatible" or answer.get("backend") == version:
            break
        version = answer.get("backend")
    return None, answer


def schedule(sock, request_id, op):
    """Sends one schedule action and waits for its result."""
    send(sock, {"type": "action", "request_id": request_id,
                "action": {"action": "manage_schedule", "op": op}})
    while True:
        reply = receive(sock)
        if reply.get("type") == "action_result" and reply.get("request_id") == request_id:
            return reply["result"]


def main(path):
    def connect():
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.connect(path)
        return sock

    sock, welcome = handshake(connect, "hello.py")
    if sock is None:
        print("handshake failed:", json.dumps(welcome))
        return 1
    print("connected:", json.dumps(welcome))

    created = schedule(sock, "create", {
        "op": "create",
        "when": {"every": "cron", "expr": "0 9 * * 1-5", "tz": "Europe/Berlin"},
        "task": "Say good morning.",
        "grants": GRANTS,
        "channel": "local",
    })
    outcome = created.get("outcome", {})
    if outcome.get("result") != "created":
        print("create refused:", json.dumps(created))
        return 1
    print("created:", outcome["id"])

    listed = schedule(sock, "list", {"op": "list"})
    for shown in listed.get("outcome", {}).get("schedules", []):
        print("scheduled:", shown["id"], json.dumps(shown["when"]), "next due", shown["next_due"])

    cancelled = schedule(sock, "cancel", {"op": "cancel", "id": outcome["id"]})
    print("cancelled:", json.dumps(cancelled.get("outcome")))
    sock.close()
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__.strip().splitlines()[2])
        sys.exit(64)
    sys.exit(main(sys.argv[1]))
