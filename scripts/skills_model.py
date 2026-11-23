"""A scripted stand-in for a model, for the skills end-to-end test
(WO-046): it uses a skill the owner enabled as a model would.

Usage: python3 scripts/skills_model.py [port]   (default 18092)

Like docs/examples/stand-in-model.py, it passes the agent's tool-calling
check. Then, asked TASK, it:

- checks the run's prompt lists ENABLED, and none of WAITING;
- reads ENABLED's playbook with skill_read;
- answers READ and the line of the playbook it quotes (MARKER), when the
  playbook came back labelled with the signer the shipped trust roots name,
  or says what was wrong.

Nothing here is a model.
"""
import base64
import hashlib
import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TASK = "Write me a short report on what lives in a tide pool."
ENABLED = "write-a-report"
WAITING = ("tidy-a-folder", "research-a-question")
# A line of write-a-report's signed playbook, as it reads.
MARKER = "load-bearing claim traces to a source the run actually read."
READ = "I read the write-a-report playbook, signed by Hatchling's maintainer:"
NOT_LISTED = "The skill I was to use isn't listed:"
LISTED_WAITING = "A skill the owner hasn't enabled is listed:"
NOT_READ = "I couldn't read the playbook as signed:"

TRUST_ROOTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "packaging", "skill-trust-roots")


def signer():
    """The fingerprint of the shipped trust roots' primary key, as a read
    labels its signer: the SHA-256 of the key, in hex."""
    with open(TRUST_ROOTS, encoding="utf-8") as roots:
        for line in roots:
            role, _, key = line.strip().partition(" ")
            if role == "primary":
                return hashlib.sha256(base64.b64decode(key)).hexdigest()
    raise ValueError(f"{TRUST_ROOTS} names no primary key")


def text(content):
    return content if isinstance(content, str) else json.dumps(content)


def prompt(messages):
    """The run's system prompt, all of it."""
    return "\n".join(text(m.get("content")) for m in messages if m.get("role") == "system")


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


def read_text(result):
    """The text of a skill_read result. The agent gives the model each tool
    output as JSON inside an untrusted-data envelope
    (`<tool_result tool=".." status="ok">
{json}
</tool_result>` and a
    line after it), so the read's own text is the JSON's `text` field, its
    quotes unescaped. Anything else, as it came."""
    start, end = result.find("{"), result.rfind("}")
    try:
        value = json.loads(result[start:end + 1]) if start != -1 else None
    except ValueError:
        return result
    if isinstance(value, dict) and isinstance(value.get("text"), str):
        return value["text"]
    return result


def call(name, arguments):
    return {"role": "assistant", "content": None, "tool_calls": [{
        "id": f"call-{time.monotonic_ns()}", "type": "function",
        "function": {"name": name, "arguments": json.dumps(arguments)},
    }]}


def answer(content):
    return {"role": "assistant", "content": content}


def next_message(messages, fingerprint):
    """What the model says next, given the conversation so far."""
    seen = results(messages)
    if not seen:
        listed = prompt(messages)
        if f"- {ENABLED}@1:" not in listed:
            return answer(f"{NOT_LISTED} {listed[-600:]}")
        waiting = [name for name in WAITING if f"- {name}@" in listed]
        if waiting:
            return answer(f"{LISTED_WAITING} {', '.join(waiting)}")
        return call("skill_read", {"skill": ENABLED})
    got = read_text(seen[-1])
    if MARKER in got and f'signer="{fingerprint}"' in got:
        return answer(f"{READ} {MARKER}")
    return answer(f"{NOT_READ} {got[:400]}")


class Handler(BaseHTTPRequestHandler):
    fingerprint = ""

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
            message = next_message(request.get("messages", []), self.fingerprint)
        body = json.dumps({"choices": [{"index": 0, "message": message,
                                        "finish_reason": "tool_calls" if message.get("tool_calls") else "stop"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        sys.stderr.write("skills model: " + (fmt % args) + "\n")


if __name__ == "__main__":
    Handler.fingerprint = signer()
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 18092
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
