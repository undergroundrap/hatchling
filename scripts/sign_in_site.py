"""A site to sign in to, for the browser's end-to-end test (WO-031): a form
with an email and a password, served over HTTPS as the agent's browser
reaches a site. It records what the form sent, and never shows it.

Usage: python3 scripts/sign_in_site.py <address> <port> <chain> <key> <record> <session>

<chain> is the site's certificate then its CA's, PEM; <key> its private
key. Each sign-in the form sends is written to <record>, JSON, readable by
its owner alone: the test compares it with the sign-in the owner kept.

A sign-in sets the cookie `session=<session>`, and the page after it shows
the cookie, as any page can (`document.cookie`): the test's canary for a
session the agent must never be shown (WO-030).
"""
import json
import os
from pathlib import Path
import ssl
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs

TITLE = "Sign in"
# What the page says once a sign-in was sent: the test's model looks for it.
SIGNED_IN = "You are signed in."
# What the page shows its cookie after.
SESSION_SHOWN = "Your session: "
FORM = f"""<!doctype html>
<html><head><meta charset="utf-8"><title>{TITLE}</title></head>
<body><h1>{TITLE}</h1>
<form method="post" action="/sign-in">
<label>Email <input name="email" type="text" autocomplete="username"></label>
<label>Password <input name="password" type="password" autocomplete="current-password"></label>
<button type="submit">Sign in</button>
</form></body></html>
"""
DONE = f"""<!doctype html>
<html><head><meta charset="utf-8"><title>Signed in</title></head>
<body><p>{SIGNED_IN}</p><p id="session"></p>
<script>document.getElementById("session").textContent = "{SESSION_SHOWN}" + document.cookie;</script>
</body></html>
"""
MAX_BODY_BYTES = 64 * 1024


def sent(body):
    """The email and password a form body sent: `None` for a field it didn't."""
    fields = parse_qs(body.decode("utf-8", "replace"), keep_blank_values=True)
    return {name: (fields.get(name) or [None])[0] for name in ("email", "password")}


def record(path, fields):
    """Appends one sign-in to `path`, a file only its owner reads."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    with os.fdopen(descriptor, "a", encoding="utf-8") as file:
        file.write(json.dumps(fields) + "\n")


def session_cookie(session):
    """The `Set-Cookie` a sign-in is answered with: not `HttpOnly`, so the
    page can show it."""
    return f"session={session}; Path=/; Secure; SameSite=Lax; Max-Age=3600"


def handler(record_path, session):
    class Handler(BaseHTTPRequestHandler):
        def page(self, status, html, cookie=None):
            body = html.encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            if cookie:
                self.send_header("Set-Cookie", cookie)
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path.split("?")[0] == "/":
                self.page(200, FORM)
            else:
                self.send_error(404)

        def do_POST(self):
            length = int(self.headers.get("Content-Length", 0))
            if self.path != "/sign-in" or not 0 <= length <= MAX_BODY_BYTES:
                self.send_error(404)
                return
            record(record_path, sent(self.rfile.read(length)))
            self.page(200, DONE, session_cookie(session))

        def log_message(self, fmt, *args):
            # The request line alone: never a body.
            sys.stderr.write("sign-in site: " + (fmt % args) + "\n")

    return Handler


def main():
    if len(sys.argv) != 7:
        print(__doc__.strip().splitlines()[3], file=sys.stderr)
        return 64
    address, port, chain, key, record_path, session = sys.argv[1:]
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(chain, key)
    server = ThreadingHTTPServer((address, int(port)), handler(Path(record_path), session))
    server.socket = context.wrap_socket(server.socket, server_side=True)
    server.serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
