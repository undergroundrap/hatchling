"""A scripted stand-in for a model, for the browser's end-to-end test: it
drives the agent's browser tools as a model would, and says what it saw.

Usage: python3 scripts/browsing_model.py [port]   (default 18090)

Like docs/examples/stand-in-model.py, it passes the agent's tool-calling
check. Then, for each task, it reads the tool results so far and:

- opens BROWSER_URL, and opens it again while the owner is being asked;
- reads the page, again and again, waiting a little each time: while the
  owner drives, every read is answered that the owner is using it;
- once a read is answered that the owner handed it back, it answers the
  task, saying whether it was ever told the owner was driving.

A chat run has a budget of steps, not of time, so it waits in its own
answers rather than spending steps. Nothing here is a model.
"""
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# A site that never resolves: the owner approves it, the browser can't load
# it, and the page is the browser's own error page. That's enough for the
# owner to drive it, without a site the test would have to serve as public.
BROWSER_URL = "https://hatchling-end-to-end.invalid/"
REASON = "to show the owner the browser"
WAIT_SECS = 6
DRIVEN = "the owner is using the browser"
HANDED_BACK = "handed_back"
WAITING = "the owner has been asked to allow this site"
# What the answer says: the test looks for these.
SAW_DRIVING = "I waited while you drove the browser"
NEVER_DRIVEN = "You never drove the browser"
ANSWER_END = "and you handed it back."


def results(messages):
    """The text of each tool result so far, oldest first."""
    texts = []
    for message in messages:
        if message.get("role") == "tool":
            content = message.get("content")
            texts.append(content if isinstance(content, str) else json.dumps(content))
    return texts


def call(name, arguments):
    return {"role": "assistant", "content": None, "tool_calls": [{
        "id": f"call-{time.monotonic_ns()}", "type": "function",
        "function": {"name": name, "arguments": json.dumps(arguments)},
    }]}


def next_message(messages):
    """What the model says next, given the conversation so far."""
    seen = results(messages)
    if not seen:
        return call("browser_open", {"url": BROWSER_URL, "reason": REASON})
    last = seen[-1]
    if HANDED_BACK in last:
        said = SAW_DRIVING if any(DRIVEN in result for result in seen) else NEVER_DRIVEN
        return {"role": "assistant", "content": f"{said}, {ANSWER_END}"}
    time.sleep(WAIT_SECS)
    if WAITING in last:
        return call("browser_open", {"url": BROWSER_URL, "reason": REASON})
    return call("browser_read", {})


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
        sys.stderr.write("browsing model: " + (fmt % args) + "\n")


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 18090
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
