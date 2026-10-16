"""Use the app as a person would: open it, send a message, see the agent's answer.

Usage: python3 scripts/app_end_to_end.py <address> [machine address]

The address is the one hatchling-host printed, with the UI token in its
fragment; the Machine view's, when given, has the operator token in its. The broker behind it must answer with the stand-in model. Needs
Playwright (frontend/tests/e2e/requirements.txt) and Chromium: CHROMIUM_PATH,
else Playwright's own.
"""

import os
from pathlib import Path
import re
import sys

from readme_quickstart import expected_answer

# Not first-run.py's task: the page replays the stream, so its chat may
# already hold that message and its answer.
TASK = "Say hello from the app."
TOKEN_FRAGMENT = "#hatchling-ui="
ANSWER_TIMEOUT_MS = 120_000


# True once an agent bubble holding the answer follows the user's bubble
# holding the task, in the page's order.
ANSWERED = """([task, answer]) => {
    const asked = [...document.querySelectorAll('.user-bubble')].find(b => b.textContent.includes(task));
    return asked !== undefined && [...document.querySelectorAll('.agent-bubble')].some(b =>
        b.textContent.includes(answer) && (asked.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0);
}"""

# A hello on the host's channel, as the page sends it: the broker's answer,
# or nothing if it closes the connection first.
HELLO = """async ([url, token]) => await new Promise(resolve => {
    const socket = new WebSocket(url);
    const hello = { type: 'hello', schema_version: 4, client: 'app-end-to-end' };
    if (token !== null) hello.token = token;
    socket.onopen = () => socket.send(JSON.stringify(hello));
    socket.onmessage = event => { resolve(JSON.parse(event.data).type); socket.close(); };
    socket.onclose = () => resolve(null);
    setTimeout(() => resolve('no answer in 10 s'), 10000);
})"""


def check_token(page, address):
    """Raise unless the broker welcomes the page's token and refuses a
    connection without it or with another: through the host, which checks
    nothing itself."""
    token = address.split(TOKEN_FRAGMENT, 1)[1]
    channel = address.split("#", 1)[0].replace("http://", "ws://", 1).rstrip("/") + "/ui"
    wrong = ("0" if token[0] != "0" else "1") + token[1:]
    for given, expected in ((token, "welcome"), (None, None), (wrong, None)):
        answer = page.evaluate(HELLO, [channel, given])
        if answer != expected:
            raise ValueError(f"A hello with {'the token' if given == token else 'no token' if given is None else 'a wrong token'} got {answer!r}")


MACHINE_FRAGMENT = "#hatchling-operator="


def screenshot(page, name):
    """Saves `page` as `name` under HATCHLING_E2E_SCREENSHOTS, when it is set."""
    folder = os.environ.get("HATCHLING_E2E_SCREENSHOTS")
    if folder:
        page.screenshot(path=str(Path(folder) / name), full_page=True)


# The egress decisions the Machine view shows, counted from its own heading.
EGRESS_COUNT = "() => Number(document.querySelector('#machine-egress-title .machine-count')?.textContent ?? -1)"


def open_machine(browser, machine):
    """The Machine view at `machine`, live, with its page errors gathered."""
    from playwright.sync_api import expect

    errors = []
    page = browser.new_page()
    page.on("pageerror", lambda error: errors.append(str(error)))
    page.goto(machine)
    if MACHINE_FRAGMENT in page.url:
        raise ValueError("The operator token stayed in the address bar")
    expect(page.locator(".machine-status")).to_have_text("Live, refreshed every 2 seconds.")
    expect(page.locator("#machine-egress-title")).to_be_visible()
    # One full refresh, so every count read next is an answer's, not the
    # empty view's before its first answers.
    page.wait_for_timeout(2500)
    return page, errors


# A task the stand-in answers slowly, so its run lives long enough to be seen.
SLOW_TASK = "Take your time, then say hello."
# The cells running that the Machine view lists as runs.
RUNS = "() => [...document.querySelectorAll('#machine-cells-title ~ * td:nth-child(2)')].filter(td => td.textContent === 'Run').length"


def check_a_run_comes_and_goes(app, machine):
    """Raise unless, while the app's slow message runs, the Machine view lists
    a run cell, and once it is answered, lists none."""
    app.locator("#message-input").fill(SLOW_TASK)
    app.get_by_role("button", name="Send message", exact=True).click()
    machine.wait_for_function(f"() => ({RUNS})() > 0", timeout=ANSWER_TIMEOUT_MS)
    screenshot(machine, "machine-running.png")
    machine.wait_for_function(f"() => ({RUNS})() === 0", timeout=ANSWER_TIMEOUT_MS)
    if SLOW_TASK in machine.inner_text("body"):
        raise ValueError("The Machine view showed what the person said")


def check_machine(page, errors, before):
    """Raise unless the open Machine view, without a reload, came to show
    more egress decisions than `before`, the app's connection, and the chat's
    allowed egress, and holds none of the chat's text."""
    from playwright.sync_api import expect

    page.wait_for_function(f"() => ({EGRESS_COUNT})() > {before}", timeout=ANSWER_TIMEOUT_MS)
    sessions = page.locator("section").filter(has=page.locator("#machine-sessions-title"))
    expect(sessions.locator("td", has_text=re.compile(r"^App$")).first).to_be_visible()
    egress = page.locator("tr").filter(has_text="localhost:18089").filter(has_text=re.compile(r"Allowed"))
    expect(egress.first).to_be_visible()
    if TASK in page.inner_text("body"):
        raise ValueError("The Machine view showed what the person said")
    screenshot(page, "machine.png")
    if errors:
        raise ValueError("The Machine view raised: " + "; ".join(errors))


def check(address, answer, machine=None):
    """Raise unless the app at `address` shows the agent's `answer` to TASK,
    and, given the Machine view's address, that view follows the chat."""
    from playwright.sync_api import expect, sync_playwright

    if TOKEN_FRAGMENT not in address:
        raise ValueError("The address has no UI token")
    errors = []
    with sync_playwright() as driver:
        # Root, in CI's disposable namespace, can't use Chromium's sandbox.
        browser = driver.chromium.launch(executable_path=os.environ.get("CHROMIUM_PATH"),
                                         headless=True, args=["--no-sandbox"])
        try:
            # The Machine view opens first, so it must update to see the chat.
            watching = None
            if machine is not None:
                machine_page, machine_errors = open_machine(browser, machine)
                watching = (machine_page, machine_errors, machine_page.evaluate(f"({EGRESS_COUNT})()"))
            page = browser.new_page()
            page.on("pageerror", lambda error: errors.append(str(error)))
            page.goto(address)
            expect(page.locator("#message-input")).to_be_visible()
            # The page took the token and left the address bar without it.
            if TOKEN_FRAGMENT in page.url:
                raise ValueError("The token stayed in the address bar")
            page.locator("#message-input").fill(TASK)
            page.get_by_role("button", name="Send message", exact=True).click()
            expect(page.locator(".user-bubble").filter(has_text=TASK)).to_have_count(1)
            # Only an answer after this message counts: the page may already
            # show first-run.py's answer, the same sentence.
            page.wait_for_function(ANSWERED, arg=[TASK, answer], timeout=ANSWER_TIMEOUT_MS)
            check_token(page, address)
            screenshot(page, "app.png")
            if watching is not None:
                check_machine(*watching)
                check_a_run_comes_and_goes(page, watching[0])
        finally:
            browser.close()
    if errors:
        raise ValueError("The page raised: " + "; ".join(errors))


def main():
    if len(sys.argv) not in (2, 3):
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 64
    answer = expected_answer(Path(__file__).resolve().parents[1] / "docs/examples/stand-in-model.py")
    try:
        check(sys.argv[1], answer, sys.argv[2] if len(sys.argv) == 3 else None)
    except ValueError as error:
        print(f"app end to end: {error}", file=sys.stderr)
        return 1
    print("app end to end: the agent's answer reached the page"
          + (", and the Machine view saw it run" if len(sys.argv) == 3 else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
