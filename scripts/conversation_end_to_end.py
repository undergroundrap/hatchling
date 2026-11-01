"""Talk to the agent in the app, across broker restarts and a new
conversation, as the owner would (WO-026): run by
scripts/conversation_end_to_end.sh, which starts the broker, the scripted
turns model and the app's host, and restarts the broker between phases.

Each answer is the model saying how many earlier turns the broker gave it,
and what the owner said in them. The phases:

- talk: the second message is given the first.
- resumed: after a restart, the app shows the conversation kept, under
  Hatchling's notice, and the next message is given both exchanges. Then
  the owner starts a new conversation, and the next message is given
  nothing said before it.
- new: after another restart, only the new conversation is kept, shown,
  and given.

Usage, as root: conversation_end_to_end.py <app-address> <phase>, with
HATCHLING_E2E_SCREENSHOTS naming a directory for screenshots.
"""
import sys

from approval_end_to_end import screenshot
from turns_model import GIVEN, SAID

FIRST = "Remember the word heron."
SECOND = "Which word did I ask you to remember?"
THIRD = "Still there after a restart?"
FOURTH = "And now, what do you remember?"
FIFTH = "And after another restart?"
NOTICE = "New conversation. The agent isn't given anything said before this."
KEPT = "Kept from before the broker restarted: the agent is still given this conversation."
# A chat run starts a cell for the turn: give it longer than a step.
TURN_TIMEOUT_MS = 120_000
STEP_TIMEOUT_MS = 30_000


def given(*said):
    """What the model answers when given the exchanges where the owner said
    `said`."""
    words = GIVEN.format(count=len(said))
    return words + " " + SAID.format(said=" | ".join(said)) if said else words


class App:
    def __init__(self, page, expect):
        self.page = page
        self.expect = expect
        self.answers = page.locator(".message-row.agent.kind-text")
        self.sent = page.locator(".message-row.user.kind-text")
        self.notices = page.locator(".message-row.kind-notice")

    def ask(self, text, expected, count=1):
        self.page.locator("#message-input").fill(text)
        self.page.get_by_role("button", name="Send message", exact=True).click()
        self.expect(self.answers.filter(has_text=expected)).to_have_count(
            count, timeout=TURN_TIMEOUT_MS)

    def shows(self, sent, answers):
        """The app shows each message the owner sent once, and each
        answer."""
        for text in sent:
            self.expect(self.sent.filter(has_text=text)).to_have_count(1, timeout=STEP_TIMEOUT_MS)
        for text in answers:
            self.expect(self.answers.filter(has_text=text)).to_have_count(1, timeout=STEP_TIMEOUT_MS)

    def notice(self, text):
        self.expect(self.notices.filter(has_text=text)).to_have_count(1, timeout=STEP_TIMEOUT_MS)
        self.expect(self.answers.filter(has_text=text)).to_have_count(0)


def talk(app):
    app.ask(FIRST, given())
    app.ask(SECOND, given(FIRST))
    screenshot(app.page, "conversation-1-remembered.png")


def resumed(app):
    # What the agent is still given is on screen, as Hatchling's.
    app.notice(KEPT)
    app.shows([FIRST, SECOND], [given(), given(FIRST)])
    app.ask(THIRD, given(FIRST, SECOND))
    screenshot(app.page, "conversation-2-resumed.png")
    # The owner starts a new conversation: Hatchling says where, in its
    # own words, and every message above stays.
    app.page.locator("#chat-new-conversation").click()
    app.notice(NOTICE)
    app.shows([FIRST, SECOND, THIRD], [])
    # The next message is given nothing said before it.
    app.ask(FOURTH, given(), count=2)
    screenshot(app.page, "conversation-3-new.png")


def new(app):
    # Only the new conversation was kept.
    app.notice(KEPT)
    app.shows([FOURTH], [given()])
    for text in (FIRST, SECOND, THIRD):
        app.expect(app.sent.filter(has_text=text)).to_have_count(0)
    app.ask(FIFTH, given(FOURTH))
    screenshot(app.page, "conversation-4-new-kept.png")


PHASES = {"talk": talk, "resumed": resumed, "new": new}


def main():
    from playwright.sync_api import expect, sync_playwright

    address, phase = sys.argv[1], PHASES[sys.argv[2]]
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True, args=["--no-sandbox"])
        page = browser.new_page(viewport={"width": 1400, "height": 900})
        errors = []
        page.on("pageerror", lambda error: errors.append(str(error)))
        page.goto(address)
        phase(App(page, expect))
        browser.close()
        if errors:
            raise AssertionError(f"the page's errors: {errors}")
    print(f"{sys.argv[2]}: given what the owner was shown, across restarts")


if __name__ == "__main__":
    main()
