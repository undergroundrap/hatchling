"""A scripted stand-in for a model, for the browser's end-to-end test: it
drives the agent's browser tools as a model would, and says what it saw.

Usage: python3 scripts/browsing_model.py [port [record]]   (default 18090)

Like docs/examples/stand-in-model.py, it passes the agent's tool-calling
check. Then, for each task, it reads the tool results so far and:

- opens BROWSER_URL, and opens it again while the owner is being asked;
- reads the page, again and again, waiting a little each time: while the
  owner drives, every read is answered that the owner is using it;
- once a read is answered that the owner handed it back, it answers the
  task, saying whether it was ever told the owner was driving.

Asked SIGN_IN_TASK (WO-031), it opens the site again, signs in with the
sign-in the owner kept for it (browser_sign_in, naming the form's fields),
sends the form, and says whether the site said it was signed in.

Given a record file, it appends each request's body to it: the test reads
it for a sign-in's secrets, which no request may carry.

A chat run has a budget of steps, not of time, so it waits in its own
answers rather than spending steps. Nothing here is a model.
"""
import json
import os
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
# The sign-in task (WO-031), and what its answer says.
SIGN_IN_TASK = "Sign in to the site with the sign-in I kept, then tell me how it went."
SIGN_IN_REASON = "to sign in to it as the owner asked"
SIGNED_IN = "I signed in, and the site says so."
NOT_SIGNED_IN = "I couldn't sign in:"
# What sign_in_site.py's page says once a sign-in was sent.
SITE_SIGNED_IN = "You are signed in."


def text(content):
    return content if isinstance(content, str) else json.dumps(content)


def results(messages):
    """The text of each tool result since the owner's last message, oldest
    first: a chat's earlier tasks are its history, not this task's."""
    texts = []
    for message in messages:
        if message.get("role") == "user":
            texts = []
        elif message.get("role") == "tool":
            texts.append(text(message.get("content")))
    return texts


def task(messages):
    """The owner's last message."""
    asked = [text(message.get("content")) for message in messages if message.get("role") == "user"]
    return asked[-1] if asked else ""


def page(result):
    """The page a tool result shows, or `None` for a result that isn't one."""
    start, end = result.find("{"), result.rfind("}")
    try:
        shown = json.loads(result[start:end + 1]) if start != -1 else None
    except ValueError:
        return None
    return shown if isinstance(shown, dict) and "elements" in shown else None


def element(shown, matches):
    """The id of the first element of `shown` that `matches`, or `None`."""
    return next((each["id"] for each in shown["elements"] if matches(each)), None)


def failed(why):
    return {"role": "assistant", "content": f"{NOT_SIGNED_IN} {why[:300]}"}


def sign_in_message(seen):
    """The sign-in task's next step: open the site, sign in, send the form,
    and say what the site said."""
    if not seen:
        return call("browser_open", {"url": BROWSER_URL, "reason": SIGN_IN_REASON})
    shown = page(seen[-1])
    if shown is None:
        return failed(seen[-1])
    if len(seen) == 1:
        password = element(shown, lambda each: each["role"] == "password")
        username = element(shown, lambda each: each["role"] != "password" and each["name"].startswith("Email"))
        if password is None or username is None:
            return failed("the page has no sign-in form: " + json.dumps(shown["elements"]))
        return call("browser_sign_in", {"password_element": password, "username_element": username})
    if len(seen) == 2:
        button = element(shown, lambda each: each["role"] == "button" and each["name"] == "Sign in")
        if button is None:
            return failed("the form has no Sign in button: " + json.dumps(shown["elements"]))
        return call("browser_act", {"element": button, "action": "click"})
    if SITE_SIGNED_IN in shown.get("text", ""):
        return {"role": "assistant", "content": SIGNED_IN}
    return failed("the site didn't say I was signed in: " + shown.get("text", "")[:200])


def call(name, arguments):
    return {"role": "assistant", "content": None, "tool_calls": [{
        "id": f"call-{time.monotonic_ns()}", "type": "function",
        "function": {"name": name, "arguments": json.dumps(arguments)},
    }]}


def next_message(messages):
    """What the model says next, given the conversation so far."""
    seen = results(messages)
    if SIGN_IN_TASK in task(messages):
        return sign_in_message(seen)
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


# Where each request's body is appended, when the test names a file.
RECORD = None


def keep(body):
    if RECORD is not None:
        descriptor = os.open(RECORD, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
        with os.fdopen(descriptor, "ab") as file:
            file.write(body + b"\n")


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path.rstrip("/") != "/v1/chat/completions":
            self.send_error(404)
            return
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        keep(body)
        request = json.loads(body)
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
    RECORD = sys.argv[2] if len(sys.argv) > 2 else None
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
