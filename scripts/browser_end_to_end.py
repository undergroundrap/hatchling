"""Drive the agent's browser from the app, as the owner would (WO-029): allow
the site it asks for, open its browser from the chat, take control, scroll
and type, keep its sign-in (WO-030), and hand it back; the agent says it
waited while the owner drove, and Settings lists the site kept signed in.

Usage: python3 scripts/browser_end_to_end.py <address>

The address is the one hatchling-host printed, with the UI token in its
fragment. The broker behind it must have a browser configured and answer
with scripts/browsing_model.py. Needs Playwright
(frontend/tests/e2e/requirements.txt) and Chromium: CHROMIUM_PATH, else
Playwright's own.
"""

import os
from pathlib import Path
import re
import sys

from browsing_model import ANSWER_END, BROWSER_URL, SAW_DRIVING, WAIT_SECS

TASK = "Use the browser, then tell me what happened."
# A sign-in the owner keeps for it: nothing a page would show by chance.
SIGN_IN_USER = "zoe-user@hatchling-end-to-end.invalid"
SIGN_IN_PASSWORD = "hunter2-end-to-end"
# The site the agent's browser visits, as the broker names it.
SITE = BROWSER_URL.removeprefix("https://").rstrip("/") + ":443"
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


# The sequence of the frame the browser view shows.
FRAME_SEQUENCE = "() => Number(document.querySelector('.browser-session-stage .browser-frame')?.dataset.sequence ?? 0)"


def newer(page, act):
    """Does `act`, and waits until the browser view shows a frame newer than
    the one it showed before: the picture the owner's input was answered
    with."""
    before = page.evaluate(FRAME_SEQUENCE)
    act()
    page.wait_for_function(f"(before) => ({FRAME_SEQUENCE})() > before", arg=before, timeout=STEP_TIMEOUT_MS)


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
    # The owner has it: their own input goes to the page, each answered
    # with a newer picture of it.
    controls = page.locator("form[aria-label='Your controls for the page']")
    expect(controls).to_be_visible(timeout=STEP_TIMEOUT_MS)
    newer(page, controls.get_by_role("button", name="Scroll down").click)
    controls.get_by_role("textbox", name="Text to type into the page").fill("by hand")
    newer(page, controls.get_by_role("button", name="Type", exact=True).click)
    # The owner keeps its sign-in: asked first, then kept by the broker,
    # which names the site it kept.
    page.get_by_role("button", name="Keep me signed in").first.click()
    page.locator("dialog.dialog").get_by_role("button", name="Keep me signed in").click()
    expect(page.get_by_text(f"opens {SITE} signed in from now on").first).to_be_visible(timeout=STEP_TIMEOUT_MS)
    # Meanwhile the agent's reads wait, for a few of its steps. Handed back
    # from the picture the owner sees, the latest frame.
    back = page.get_by_role("button", name="Return control to agent")
    expect(back).to_be_enabled(timeout=STEP_TIMEOUT_MS)
    page.wait_for_timeout(2 * WAIT_SECS * 1000)
    screenshot(page, "browser-3-driving.png")
    expect(back).to_be_enabled(timeout=STEP_TIMEOUT_MS)
    # Handed back: the agent looks again, answers, and its run ends, closing
    # the browser; its answer is what shows the handback reached it.
    back.click()


def signed_in(page):
    """Raise unless Settings lists the site the owner kept signed in, and
    keeps a sign-in for it that the page then holds nowhere."""
    from playwright.sync_api import expect

    page.keyboard.press("Control+,")
    page.get_by_role("navigation", name="Settings sections").get_by_role("button", name="Permissions", exact=True).click()
    pane = page.locator(".settings-pane")
    pane.get_by_role("button", name=re.compile("^Signed-in sites")).click()
    expect(pane.locator(".signed-in-sites").get_by_text(SITE)).to_be_visible(timeout=STEP_TIMEOUT_MS)
    screenshot(page, "browser-5-signed-in-sites.png")
    # A sign-in, typed in the form and sent straight to the broker.
    pane.get_by_role("button", name="Add a sign-in").click()
    pane.get_by_label("Name").fill("Work")
    pane.get_by_label("Username").fill(SIGN_IN_USER)
    pane.get_by_label("Password").fill(SIGN_IN_PASSWORD)
    pane.get_by_role("button", name="Keep sign-in").click()
    expect(page.get_by_text("Kept. The agent can sign in with it").first).to_be_visible(timeout=STEP_TIMEOUT_MS)
    listed = pane.locator(".signed-in-sign-in")
    expect(listed.get_by_text("Work")).to_be_visible(timeout=STEP_TIMEOUT_MS)
    expect(listed.get_by_text(f"Filled on {BROWSER_URL.rstrip('/')}")).to_be_visible(timeout=STEP_TIMEOUT_MS)
    screenshot(page, "browser-6-sign-in-kept.png")
    held = page.evaluate("() => document.documentElement.outerHTML + JSON.stringify(localStorage) + JSON.stringify(sessionStorage)")
    if SIGN_IN_PASSWORD in held or SIGN_IN_USER in held:
        raise ValueError("The page still holds the sign-in it sent to the broker")


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
            signed_in(page)
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
