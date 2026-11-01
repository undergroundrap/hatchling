"""Talk to the agent in the app, then start a new conversation as the owner
would (WO-026): run by scripts/conversation_end_to_end.sh, which starts the
broker, the scripted turns model and the app's host.

Each answer is the model saying how many earlier turns the broker gave it,
and what the owner said in them. The second message is given the first.
The owner starts a new conversation from the chat's header: Hatchling's
notice shows where it starts, the messages above stay, and the next message
is given nothing said before it.

Usage, as root: conversation_end_to_end.py <app-address>, with
HATCHLING_E2E_SCREENSHOTS naming a directory for screenshots.
"""
import sys

from approval_end_to_end import screenshot
from turns_model import GIVEN, SAID

FIRST = "Remember the word heron."
SECOND = "Which word did I ask you to remember?"
THIRD = "And now, what do you remember?"
NOTICE = "New conversation. The agent isn't given anything said before this."
# A chat run starts a cell for the turn: give it longer than a step.
TURN_TIMEOUT_MS = 120_000
STEP_TIMEOUT_MS = 30_000


def main():
    from playwright.sync_api import expect, sync_playwright

    address = sys.argv[1]
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True, args=["--no-sandbox"])
        page = browser.new_page(viewport={"width": 1400, "height": 900})
        errors = []
        page.on("pageerror", lambda error: errors.append(str(error)))
        page.goto(address)
        answers = page.locator(".message-row.agent.kind-text")
        sent = page.locator(".message-row.user.kind-text")

        def ask(text, expected, count=1):
            page.locator("#message-input").fill(text)
            page.get_by_role("button", name="Send message", exact=True).click()
            expect(answers.filter(has_text=expected)).to_have_count(count, timeout=TURN_TIMEOUT_MS)

        none = GIVEN.format(count=0)
        ask(FIRST, none)
        ask(SECOND, GIVEN.format(count=1) + " " + SAID.format(said=FIRST))
        screenshot(page, "conversation-1-remembered.png")
        # The owner starts a new conversation: Hatchling says where, in its
        # own words, and every message above stays.
        page.locator("#chat-new-conversation").click()
        notices = page.locator(".message-row.kind-notice").filter(has_text=NOTICE)
        expect(notices).to_have_count(1, timeout=STEP_TIMEOUT_MS)
        expect(answers.filter(has_text=NOTICE)).to_have_count(0)
        expect(sent.filter(has_text=FIRST)).to_have_count(1)
        expect(answers.filter(has_text=FIRST)).to_have_count(1)
        # The next message is given nothing said before it.
        ask(THIRD, none, count=2)
        expect(answers.filter(has_text="Turns before this: 1.")).to_have_count(1)
        screenshot(page, "conversation-2-new.png")
        browser.close()
        if errors:
            raise AssertionError(f"the page's errors: {errors}")
    print("given the turn before it, and nothing after a new conversation")


if __name__ == "__main__":
    main()
