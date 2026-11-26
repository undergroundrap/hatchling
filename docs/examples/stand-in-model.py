"""A stand-in for a model server, so you can try Hatchling without one.

Usage: python3 docs/examples/stand-in-model.py [port] [--detach]   (default 18089)

With --detach it returns once it is listening, and goes on serving in the
background: a broker started after it never asks it before it can answer.

It speaks just enough of the OpenAI chat-completions API for an agent run:
it passes the agent's tool-calling check, then answers every task with a
fixed sentence. A task that says "take your time" gets its answer after a few
seconds, so you can watch its run in the Machine view. Nothing here is a model. To use a real one, point
HATCHLING_SCHEDULER_MODEL_ENDPOINT at it instead (for example Ollama's
http://localhost:11434/v1) and allow that port in the egress settings.
"""
import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

ANSWER = ("Hello from inside a Hatchling cell. This answer came from the stand-in "
          "model; point HATCHLING_SCHEDULER_MODEL_ENDPOINT at a real one.")
SLOW = "take your time"
SLOW_SECS = 5


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        # The list of models, which `hatchling-broker doctor` checks.
        if self.path.rstrip("/") != "/v1/models":
            self.send_error(404)
            return
        body = json.dumps({"object": "list", "data": [{"id": "stand-in", "object": "model"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        if self.path.rstrip("/") != "/v1/chat/completions":
            self.send_error(404)
            return
        request = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
        tools = request.get("tools") or []
        if request.get("tool_choice") == "required" and tools:
            # The agent's check that the model can call tools: call the one it offers.
            message = {"role": "assistant", "content": None, "tool_calls": [{
                "id": "call-1", "type": "function",
                "function": {"name": tools[0]["function"]["name"], "arguments": "{\"ready\": true}"},
            }]}
        else:
            if SLOW in json.dumps(request.get("messages", [])).lower():
                time.sleep(SLOW_SECS)
            message = {"role": "assistant", "content": ANSWER}
        body = json.dumps({"choices": [{"index": 0, "message": message,
                                        "finish_reason": "tool_calls" if message.get("tool_calls") else "stop"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        sys.stderr.write("stand-in model: " + (fmt % args) + "\n")


if __name__ == "__main__":
    args = [arg for arg in sys.argv[1:] if arg != "--detach"]
    port = int(args[0]) if args else 18089
    server = HTTPServer(("127.0.0.1", port), Handler)
    # Listening from here: give the shell back, and serve from a child.
    if "--detach" in sys.argv[1:] and os.fork():
        os._exit(0)
    server.serve_forever()
