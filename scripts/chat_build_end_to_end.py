"""Propose a build from the chat, and approve it in the app, as the owner
would (WO-032): run by scripts/chat_build_end_to_end.sh, which starts the
broker, the scripted idea model and the app's host.

The owner asks the agent, in the chat, to save an idea and propose building
it. The agent's model calls idea_capture, then build_propose: a card shows
the build the broker pinned, with the builder the operator named for the
kind, the idea's bytes by their length and hash, and the agent's words last,
as the agent's. The agent answers that it asked. The owner allows it once,
the card goes, and the broker tells the owner, as Hatchling's notice, what
became of the build.

Usage, as root: chat_build_end_to_end.py <app-address>, with
HATCHLING_E2E_SCREENSHOTS naming a directory for screenshots.
"""
import hashlib
import sys

from approval_end_to_end import AGENT_FIELD, BROKER_FIELDS, screenshot
from idea_model import DESCRIPTION, IDEA, KIND, PROPOSED, TASK

BUILDER = "page-builder@1.0.0"
# A chat run starts cells for the turn: give it longer than a step.
TURN_TIMEOUT_MS = 120_000
STEP_TIMEOUT_MS = 30_000


def review(page):
    """The card, as the owner sees it: raise unless it shows the build the
    broker pinned, with the operator's builder, and the agent's words only
    as the agent's."""
    from playwright.sync_api import expect

    card = page.locator(".task-approval-card").first
    expect(card).to_be_visible(timeout=TURN_TIMEOUT_MS)
    fields = card.locator(".task-approval-field")
    labels = [label.strip() for label in fields.locator("dt > span:first-child").all_inner_texts()]
    if labels != BROKER_FIELDS + [AGENT_FIELD]:
        raise AssertionError(f"the fields, in order: {labels}")
    sources = [f.get_attribute("class").split()[-1] for f in fields.all()]
    if sources != ["broker"] * len(BROKER_FIELDS) + ["agent"]:
        raise AssertionError(f"the fields' sources: {sources}")
    values = fields.locator("dd").all_inner_texts()
    shown = dict(zip(BROKER_FIELDS, values))
    if shown["Build kind"] != KIND or shown["Builder"] != BUILDER:
        raise AssertionError(f"the build shown: {values}")
    held = IDEA.encode()
    if shown["Input"] != f"idea ({len(held)} bytes, sha256 {hashlib.sha256(held).hexdigest()})":
        raise AssertionError(f"the input shown: {values}")
    if values[-1] != DESCRIPTION:
        raise AssertionError(f"the agent's words shown: {values[-1]!r}")
    return card


def main():
    from playwright.sync_api import expect, sync_playwright

    address = sys.argv[1]
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True, args=["--no-sandbox"])
        page = browser.new_page(viewport={"width": 1400, "height": 900})
        errors = []
        page.on("pageerror", lambda error: errors.append(str(error)))
        page.goto(address)
        page.locator("#message-input").fill(TASK)
        page.get_by_role("button", name="Send message", exact=True).click()
        card = review(page)
        expect(page.locator(".message-row.kind-text").filter(has_text=PROPOSED)).to_have_count(
            1, timeout=TURN_TIMEOUT_MS)
        screenshot(page, "chat-build-1-asked.png")
        request = card.get_attribute("data-task-approval")
        page.locator(f"#approval-once-{request}").click()
        expect(page.locator(".task-approval-card")).to_have_count(0, timeout=STEP_TIMEOUT_MS)
        # The broker tried it, and says what became of it: Hatchling's
        # notice, never the agent's words.
        outcome = "Approved: build idea idea-"
        expect(page.locator(".message-row.kind-notice").filter(has_text=outcome)).to_have_count(
            1, timeout=STEP_TIMEOUT_MS)
        expect(page.locator(".message-row.kind-text").filter(has_text=outcome)).to_have_count(0)
        screenshot(page, "chat-build-2-decided.png")
        told = page.locator(".message-row.kind-notice").filter(has_text=outcome).inner_text()
        browser.close()
        if errors:
            raise AssertionError(f"the page's errors: {errors}")
    print(f"proposed from the chat and approved in the app: {told.strip()}")


if __name__ == "__main__":
    main()
