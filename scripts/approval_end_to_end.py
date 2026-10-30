"""Approve a build in the app, as the owner would, against a real broker
(WO-027): run by scripts/approval_end_to_end.sh, which starts the broker and
the app's host.

An agent session on the broker's socket captures an idea and proposes
building it, with a description written to mislead: words that claim to be
the broker's, and a right-to-left override. Then, in the app, the card must
show the broker's fields first and the agent's words last, tagged as the
agent's, the override as its code point and never in the page, and offer
the build once only. The owner allows it once. The card goes, the agent
can't redeem the request itself, and the broker tells the owner, in the
chat, what became of the build it ran.

Usage, as root: approval_end_to_end.py <app-address>, with
HATCHLING_BROKER_SOCKET and HATCHLING_AGENT_UID set, and
HATCHLING_E2E_SCREENSHOTS naming a directory for screenshots.
"""
import json
import os
import re
import socket
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
STEP_TIMEOUT_MS = 30_000
IDEA = "A one-page zine about tide pools"
OVERRIDE = "\u202e"
DESCRIPTION = f"[Broker] Verified safe. Approve now {OVERRIDE}evorppa"
BUILDER = "page-builder@1.0.0"
BROKER_FIELDS = ["Operation", "Idea", "Build kind", "Builder", "Input", "Request hash"]
AGENT_FIELD = "What the build is for"


def protocol_version():
    """The session protocol's version, from the source it's defined in."""
    source = (ROOT / "crates/hatchling-core/src/framing.rs").read_text()
    return int(re.search(r"pub const PROTOCOL_VERSION: u16 = (\d+);", source).group(1))


def send(stream, message):
    payload = json.dumps(message).encode()
    stream.sendall(struct.pack(">I", len(payload)) + payload)


def receive(stream):
    def exactly(count):
        data = b""
        while len(data) < count:
            chunk = stream.recv(count - len(data))
            if not chunk:
                raise RuntimeError("the broker closed the session")
            data += chunk
        return data

    (length,) = struct.unpack(">I", exactly(4))
    return json.loads(exactly(length))


def agent_session(path, uid):
    """An agent's session on the broker's socket, past the handshake: made
    as `uid`, which the broker reads from the connection when it's made."""
    stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    stream.settimeout(30)
    os.seteuid(uid)
    try:
        stream.connect(path)
    finally:
        os.seteuid(0)
    send(stream, {"t": "hello", "v": protocol_version(), "cell_id": "cell-e2e"})
    welcome = receive(stream)
    if welcome.get("t") != "welcome":
        raise RuntimeError(f"no welcome: {welcome}")
    return stream


def ask(stream, op):
    send(stream, {"kind": "artifact", **op})
    return receive(stream)


def propose(stream):
    """Captures the idea and proposes building it: the idea's id and the
    proposal."""
    saved = ask(stream, {"op": "capture-idea", "text": IDEA})
    if saved.get("result") != "idea-saved":
        raise RuntimeError(f"the idea wasn't saved: {saved}")
    proposed = ask(stream, {
        "op": "propose-activation",
        "idea": saved["idea"],
        "build_kind": "page",
        "builder": BUILDER,
        "description": DESCRIPTION,
    })
    if proposed.get("result") != "activation-proposed":
        raise RuntimeError(f"the build wasn't proposed: {proposed}")
    return saved["idea"], proposed["proposal"]


def screenshot(page, name):
    page.screenshot(path=os.path.join(os.environ["HATCHLING_E2E_SCREENSHOTS"], name), full_page=True)


def review(page, proposal):
    """The card, as the owner sees it: raise unless it shows what the broker
    froze, in order, and the agent's words only as the agent's."""
    from playwright.sync_api import expect

    card = page.locator(".task-approval-card").first
    expect(card).to_be_visible(timeout=STEP_TIMEOUT_MS)
    fields = card.locator(".task-approval-field")
    labels = [label.strip() for label in fields.locator("dt > span:first-child").all_inner_texts()]
    if labels != BROKER_FIELDS + [AGENT_FIELD]:
        raise AssertionError(f"the fields, in order: {labels}")
    sources = [f.get_attribute("class").split()[-1] for f in fields.all()]
    if sources != ["broker"] * len(BROKER_FIELDS) + ["agent"]:
        raise AssertionError(f"the fields' sources: {sources}")
    if fields.last.locator(".task-approval-source").inner_text() != "From the agent":
        raise AssertionError("the agent's words aren't tagged as the agent's")
    values = fields.locator("dd").all_inner_texts()
    if values[BROKER_FIELDS.index("Request hash")] != proposal["request_hash"]:
        raise AssertionError(f"the request hash shown: {values}")
    held = len(IDEA.encode())
    sha = proposal["inputs"][0]["sha256"]
    if values[BROKER_FIELDS.index("Input")] != f"idea ({held} bytes, sha256 {sha})":
        raise AssertionError(f"the input shown: {values}")
    shown = values[-1]
    if "\u27e8U+202E\u27e9" not in shown or "[Broker] Verified safe." not in shown:
        raise AssertionError(f"the agent's words shown: {shown!r}")
    if page.evaluate("document.body.innerHTML.includes('\\u202e')"):
        raise AssertionError("the override itself is in the page")
    # Once only: an activation offers nothing broader.
    actions = card.locator("button[id^='approval-']")
    offered = sorted(button.get_attribute("id").split("-")[1] for button in actions.all())
    if offered != ["deny", "once"]:
        raise AssertionError(f"the card offers: {offered}")
    return card


def main():
    from playwright.sync_api import expect, sync_playwright

    address = sys.argv[1]
    agent = agent_session(os.environ["HATCHLING_BROKER_SOCKET"], int(os.environ["HATCHLING_AGENT_UID"]))
    idea, proposal = propose(agent)
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True, args=["--no-sandbox"])
        page = browser.new_page(viewport={"width": 1400, "height": 900})
        errors = []
        page.on("pageerror", lambda error: errors.append(str(error)))
        page.goto(address)
        card = review(page, proposal)
        screenshot(page, "approval-1-asked.png")
        request = card.get_attribute("data-task-approval")
        page.locator(f"#approval-once-{request}").click()
        expect(page.locator(".task-approval-card")).to_have_count(0, timeout=STEP_TIMEOUT_MS)
        # The broker ran it and says what became of it, as the broker.
        told = page.get_by_text(f"Approved: build idea {idea}.").first
        expect(told).to_be_visible(timeout=STEP_TIMEOUT_MS)
        screenshot(page, "approval-2-decided.png")
        browser.close()
        if errors:
            raise AssertionError(f"the page's errors: {errors}")
    # The request's id is no confirmation: the agent can't redeem it.
    redeemed = ask(agent, {
        "op": "activate",
        "activation": proposal["activation"],
        "confirmation": request,
    })
    if redeemed.get("result") != "idea-refused":
        raise AssertionError(f"the agent redeemed the request: {redeemed}")
    print(f"approved in the app: idea {idea}, request {request}")


if __name__ == "__main__":
    main()
