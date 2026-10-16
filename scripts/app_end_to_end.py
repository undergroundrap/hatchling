"""Use the app as a person would: open it, send a message, see the agent's answer.

Usage: python3 scripts/app_end_to_end.py <address>

The address is the one hatchling-host printed, with the UI token in its
fragment. The broker behind it must answer with the stand-in model. Needs
Playwright (frontend/tests/e2e/requirements.txt) and Chromium: CHROMIUM_PATH,
else Playwright's own.
"""

import os
from pathlib import Path
import sys

from readme_quickstart import expected_answer

TASK = "Introduce yourself in one sentence."
TOKEN_FRAGMENT = "#hatchling-ui="
ANSWER_TIMEOUT_MS = 120_000


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


def check(address, answer):
    """Raise unless the app at `address` shows the agent's `answer` to TASK."""
    from playwright.sync_api import expect, sync_playwright

    if TOKEN_FRAGMENT not in address:
        raise ValueError("The address has no UI token")
    errors = []
    with sync_playwright() as driver:
        # Root, in CI's disposable namespace, can't use Chromium's sandbox.
        browser = driver.chromium.launch(executable_path=os.environ.get("CHROMIUM_PATH"),
                                         headless=True, args=["--no-sandbox"])
        try:
            page = browser.new_page()
            page.on("pageerror", lambda error: errors.append(str(error)))
            page.goto(address)
            expect(page.locator("#message-input")).to_be_visible()
            # The page took the token and left the address bar without it.
            if TOKEN_FRAGMENT in page.url:
                raise ValueError("The token stayed in the address bar")
            page.locator("#message-input").fill(TASK)
            page.get_by_role("button", name="Send message", exact=True).click()
            expect(page.locator(".user-bubble").filter(has_text=TASK)).to_be_visible()
            reply = page.locator(".agent-bubble").filter(has_text=answer)
            expect(reply).to_be_visible(timeout=ANSWER_TIMEOUT_MS)
            check_token(page, address)
        finally:
            browser.close()
    if errors:
        raise ValueError("The page raised: " + "; ".join(errors))


def main():
    if len(sys.argv) != 2:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 64
    answer = expected_answer(Path(__file__).resolve().parents[1] / "docs/examples/stand-in-model.py")
    try:
        check(sys.argv[1], answer)
    except ValueError as error:
        print(f"app end to end: {error}", file=sys.stderr)
        return 1
    print("app end to end: the agent's answer reached the page")
    return 0


if __name__ == "__main__":
    sys.exit(main())
