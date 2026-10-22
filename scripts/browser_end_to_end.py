"""Drive the agent's browser from the app, as the owner would (WO-029): allow
the site it asks for, open its browser from the chat, take control, and
hand it back; the agent says it waited while the owner drove.

Usage: python3 scripts/browser_end_to_end.py <address>

The address is the one hatchling-host printed, with the UI token in its
fragment. The broker behind it must have a browser configured and answer
with scripts/browsing_model.py. Needs Playwright
(frontend/tests/e2e/requirements.txt) and Chromium: CHROMIUM_PATH, else
Playwright's own.
"""

import os
from pathlib import Path
import sys

from browsing_model import ANSWER_END, SAW_DRIVING, WAIT_SECS

TASK = "Use the browser, then tell me what happened."
TOKEN_FRAGMENT = "#hatchling-ui="
STEP_TIMEOUT_MS = 90_000
ANSWER_TIMEOUT_MS = 180_000

# True once an agent bubble holding `answer` follows the user's bubble
# holding `task`, in the page's order.
ANSWERED = """([task, answer]) => {
    const asked = [...document.querySelectorAll('.user-bubble')].find(b => b.textContent.includes(task));
    return asked !== undefined && [...document.querySelectorAll('.agent-bubble')].some(b =>
        b.textContent.includes(answer) && (asked.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0);
}"""


def screenshot(page, name):
    """Saves `page` as `name` under HATCHLING_E2E_SCREENSHOTS, when it is set."""
    folder = os.environ.get("HATCHLING_E2E_SCREENSHOTS")
    if folder:
        page.screenshot(path=str(Path(folder) / name), full_page=True)


def drive(page):
    """The owner's part, on the app open at the chat: raise unless each step
    shows on the page in time."""
    from playwright.sync_api import expect

    page.locator("#message-input").fill(TASK)
    page.get_by_role("button", name="Send message", exact=True).click()
    # The agent asks for the site: allowed once.
    allow = page.locator("[id^='approval-once-']").first
    expect(allow).to_be_enabled(timeout=STEP_TIMEOUT_MS)
    screenshot(page, "browser-1-asked.png")
    allow.click()
    # Its browser shows in the chat: open it.
    open_browser = page.locator(".open-browser-button").first
    expect(open_browser).to_be_visible(timeout=STEP_TIMEOUT_MS)
    open_browser.click()
    # The owner watches: the agent's read of the page brings its picture.
    expect(page.locator(".browser-session-stage img.recorded-browser-frame")).to_be_visible(timeout=STEP_TIMEOUT_MS)
    take = page.get_by_role("button", name="Take control of the browser")
    expect(take).to_be_visible(timeout=STEP_TIMEOUT_MS)
    screenshot(page, "browser-2-watching.png")
    take.click()
    # The owner has it: the agent's reads now wait, for a few of its steps.
    # Handed back from the picture the owner sees, the latest frame.
    back = page.get_by_role("button", name="Return control to agent")
    expect(back).to_be_enabled(timeout=STEP_TIMEOUT_MS)
    page.wait_for_timeout(2 * WAIT_SECS * 1000)
    screenshot(page, "browser-3-driving.png")
    expect(back).to_be_enabled(timeout=STEP_TIMEOUT_MS)
    # Handed back: the agent looks again, answers, and its run ends, closing
    # the browser; its answer is what shows the handback reached it.
    back.click()


def check(address):
    """Raise unless the owner drives the agent's browser from the app at
    `address`, and the agent says it waited while they did."""
    from playwright.sync_api import sync_playwright

    if TOKEN_FRAGMENT not in address:
        raise ValueError("The address has no UI token")
    errors = []
    with sync_playwright() as driver:
        # Root, in CI's disposable namespace, can't use Chromium's sandbox.
        browser = driver.chromium.launch(executable_path=os.environ.get("CHROMIUM_PATH"),
                                         headless=True, args=["--no-sandbox"])
        page = browser.new_page(viewport={"width": 1440, "height": 900})
        page.on("pageerror", lambda error: errors.append(str(error)))
        try:
            page.goto(address)
            page.locator("#message-input").wait_for(timeout=STEP_TIMEOUT_MS)
            drive(page)
            page.wait_for_function(ANSWERED, arg=[TASK, f"{SAW_DRIVING}, {ANSWER_END}"], timeout=ANSWER_TIMEOUT_MS)
            screenshot(page, "browser-4-handed-back.png")
        except Exception:
            screenshot(page, "browser-failed.png")
            raise
        finally:
            browser.close()
    if errors:
        raise ValueError("The page raised: " + "; ".join(errors))


def main():
    if len(sys.argv) != 2:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 64
    try:
        check(sys.argv[1])
    except ValueError as error:
        print(f"browser end to end: {error}", file=sys.stderr)
        return 1
    print("browser end to end: the owner drove the agent's browser from the app, and handed it back")
    return 0


if __name__ == "__main__":
    sys.exit(main())
