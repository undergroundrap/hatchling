"""A scripted stand-in for a model, for the conversation end-to-end test
(WO-026): it says how many earlier turns it was given, and what the owner
said in them.

Usage: python3 scripts/turns_model.py [port]   (default 18092)

Like docs/examples/stand-in-model.py, it passes the agent's tool-calling
check. Then it answers each request with answer(): the number of earlier
exchanges in the request, each an owner's message followed by the answer
they were shown, and the owner's words in them.

Nothing here is a model.
"""
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

GIVEN = "Turns before this: {count}."
SAID = "Earlier you said: {said}"


def text(content):
    return content if isinstance(content, str) else json.dumps(content)


def earlier(messages):
    """The owner's words in each earlier exchange, oldest first: a user
    message followed at once by an answer."""
    said = []
    for message, following in zip(messages, messages[1:]):
        if (message.get("role") == "user" and following.get("role") == "assistant"
                and not following.get("tool_calls")):
            said.append(text(message.get("content")))
    return said


def answer(messages):
    """What the model says, given the request's messages."""
    said = earlier(messages)
    words = GIVEN.format(count=len(said))
    if said:
        words += " " + SAID.format(said=" | ".join(said))
    return {"role": "assistant", "content": words}


def call(name, arguments):
    return {"role": "assistant", "content": None, "tool_calls": [{
        "id": f"call-{time.monotonic_ns()}", "type": "function",
        "function": {"name": name, "arguments": json.dumps(arguments)},
    }]}


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path.rstrip("/") != "/v1/chat/completions":
            self.send_error(404)
            return
        request = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
        tools = request.get("tools") or []
        if request.get("tool_choice") == "required" and tools:
            # The agent's check that the model can call tools: call the one it offers.
            message = call(tools[0]["function"]["name"], {"ready": True})
        else:
            message = answer(request.get("messages", []))
        body = json.dumps({"choices": [{"index": 0, "message": message,
                                        "finish_reason": "tool_calls" if message.get("tool_calls") else "stop"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        sys.stderr.write("turns model: " + (fmt % args) + "\n")


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 18092
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
