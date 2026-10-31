"""A scripted stand-in for a model, for the chat-build end-to-end test
(WO-032): it uses the agent's idea tools as a model would.

Usage: python3 scripts/idea_model.py [port]   (default 18091)

Like docs/examples/stand-in-model.py, it passes the agent's tool-calling
check. Then, asked TASK, it reads the tool results so far and:

- saves IDEA with idea_capture;
- proposes building it as a page with build_propose, saying DESCRIPTION;
- answers PROPOSED, or NOT_PROPOSED with the tool's error.

Nothing here is a model.
"""
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TASK = "Save my tide pool zine idea, and ask me to build it as a page."
IDEA = "Tide pool zine\nOne page on what lives in a tide pool."
DESCRIPTION = "A one-page zine about tide pools, for the owner."
KIND = "page"
PROPOSED = "I asked you to approve building it."
NOT_PROPOSED = "I couldn't ask you to build it:"


def text(content):
    return content if isinstance(content, str) else json.dumps(content)


def results(messages):
    """The text of each tool result since the owner's last message, oldest
    first."""
    texts = []
    for message in messages:
        if message.get("role") == "user":
            texts = []
        elif message.get("role") == "tool":
            texts.append(text(message.get("content")))
    return texts


def shown(result):
    """The JSON object a tool result holds, or `None`."""
    start, end = result.find("{"), result.rfind("}")
    try:
        value = json.loads(result[start:end + 1]) if start != -1 else None
    except ValueError:
        return None
    return value if isinstance(value, dict) else None


def call(name, arguments):
    return {"role": "assistant", "content": None, "tool_calls": [{
        "id": f"call-{time.monotonic_ns()}", "type": "function",
        "function": {"name": name, "arguments": json.dumps(arguments)},
    }]}


def answer(content):
    return {"role": "assistant", "content": content}


def next_message(messages):
    """What the model says next, given the conversation so far."""
    seen = results(messages)
    if not seen:
        return call("idea_capture", {"text": IDEA})
    last = shown(seen[-1]) or {}
    if len(seen) == 1:
        if "idea" not in last:
            return answer(f"{NOT_PROPOSED} {seen[-1][:300]}")
        return call("build_propose", {"idea": last["idea"], "kind": KIND, "description": DESCRIPTION})
    if "proposed" in last:
        return answer(PROPOSED)
    return answer(f"{NOT_PROPOSED} {seen[-1][:300]}")


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
            message = next_message(request.get("messages", []))
        body = json.dumps({"choices": [{"index": 0, "message": message,
                                        "finish_reason": "tool_calls" if message.get("tool_calls") else "stop"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        sys.stderr.write("idea model: " + (fmt % args) + "\n")


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 18091
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
