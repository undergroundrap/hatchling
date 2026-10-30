"""Drive the agent's browser from the app, as the owner would (WO-029): allow
the site it asks for, open its browser from the chat, take control, scroll
and type, keep its sign-in (WO-030), and hand it back; the agent says it
waited while the owner drove, and Settings lists the site kept signed in.

Then the owner keeps a canary sign-in for the site, and asks the agent to
sign in with it (WO-031). The site gets the sign-in, the vault keeper
records its fill, and neither the broker's memory, its log, nor any request
the model was sent holds the sign-in's username or password.

Usage: python3 scripts/browser_end_to_end.py <address>

The address is the one hatchling-host printed, with the UI token in its
fragment. The broker behind it must have a browser configured and answer
with scripts/browsing_model.py, the site served by scripts/sign_in_site.py;
scripts/browser_end_to_end.sh names the broker's process, its log, the
keeper's vault log, and what the site and the model recorded, in the
HATCHLING_E2E_* variables. Needs Playwright
(frontend/tests/e2e/requirements.txt) and Chromium: CHROMIUM_PATH, else
Playwright's own.
"""

import json
import os
from pathlib import Path
import pwd
import re
import secrets
import subprocess
import sys

from browsing_model import (ANSWER_END, BROWSER_URL, NOT_SIGNED_IN, SAW_DRIVING, SIGN_IN_TASK,
                            SIGNED_IN, WAIT_SECS)
from hosted_model_support import memory_holds

TASK = "Use the browser, then tell me what happened."
# A canary sign-in the owner keeps for it, new each run: nothing a page or
# a process would hold by chance.
SIGN_IN_USER = f"zoe-{secrets.token_urlsafe(18)}@hatchling-end-to-end.invalid"
SIGN_IN_PASSWORD = f"pw-{secrets.token_urlsafe(24)}"
CANARIES = (SIGN_IN_USER.encode(), SIGN_IN_PASSWORD.encode())
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
    pane.get_by_label("Name", exact=True).fill("Work")
    pane.get_by_label("Username", exact=True).fill(SIGN_IN_USER)
    pane.get_by_label("Password", exact=True).fill(SIGN_IN_PASSWORD)
    pane.get_by_role("button", name="Keep sign-in").click()
    expect(page.get_by_text("Kept. The agent can sign in with it").first).to_be_visible(timeout=STEP_TIMEOUT_MS)
    listed = pane.locator(".signed-in-sign-in")
    expect(listed.get_by_text("Work")).to_be_visible(timeout=STEP_TIMEOUT_MS)
    expect(listed.get_by_text(f"Filled on {BROWSER_URL.rstrip('/')}")).to_be_visible(timeout=STEP_TIMEOUT_MS)
    screenshot(page, "browser-6-sign-in-kept.png")
    held = page.evaluate("() => document.documentElement.outerHTML + JSON.stringify(localStorage) + JSON.stringify(sessionStorage)")
    if SIGN_IN_PASSWORD in held or SIGN_IN_USER in held:
        raise ValueError("The page still holds the sign-in it sent to the broker")


def answered(page, task, answers):
    """Waits for an agent bubble after the user's `task` holding one of
    `answers`, and returns its text."""
    page.wait_for_function("""([task, answers]) => {
        const asked = [...document.querySelectorAll('.user-bubble')].find(b => b.textContent.includes(task));
        return asked !== undefined && [...document.querySelectorAll('.agent-bubble')].some(b =>
            answers.some(answer => b.textContent.includes(answer))
            && (asked.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0);
    }""", arg=[task, answers], timeout=ANSWER_TIMEOUT_MS)
    return page.evaluate("""(answers) => [...document.querySelectorAll('.agent-bubble')].map(b => b.textContent)
        .filter(text => answers.some(answer => text.includes(answer))).pop()""", answers)


def sign_in(page):
    """The owner asks the agent to sign in with the sign-in they kept: raise
    unless it says the site signed it in."""
    from playwright.sync_api import expect

    page.keyboard.press("Escape")
    expect(page.locator("#message-input")).to_be_visible(timeout=STEP_TIMEOUT_MS)
    page.locator("#message-input").fill(SIGN_IN_TASK)
    page.get_by_role("button", name="Send message", exact=True).click()
    said = answered(page, SIGN_IN_TASK, [SIGNED_IN, NOT_SIGNED_IN])
    screenshot(page, "browser-7-signed-in.png")
    if SIGNED_IN not in said:
        raise ValueError("The agent didn't sign in: " + said)


def environment(name):
    value = os.environ.get(name)
    if not value:
        raise ValueError(f"{name} isn't set: run this from scripts/browser_end_to_end.sh")
    return value


# Runs of hex or base64 long enough to be a key or a token: masked in what a
# failure shows of the broker's memory.
SECRET_LIKE = re.compile(rb"[A-Za-z0-9+/=_-]{16,}")


def where_held(pid, canary, limit=4):
    """Where the process `pid` holds `canary`, for a failure to show: each
    copy's mapping, and the 96 bytes before it, with the canaries, anything
    shaped like a key or a token, and every byte that isn't printable
    masked."""
    found = []
    with open(f"/proc/{pid}/maps", encoding="utf-8") as maps, open(f"/proc/{pid}/mem", "rb", 0) as mem:
        for line in maps:
            fields = line.split()
            start, end = (int(bound, 16) for bound in fields[0].split("-"))
            if not fields[1].startswith("r") or fields[-1] in ("[vvar]", "[vvar_vclock]", "[vsyscall]"):
                continue
            mem.seek(start)
            data = mem.read(end - start)
            at = data.find(canary)
            while at != -1 and len(found) < limit:
                before = data[max(0, at - 96):at]
                for each in CANARIES:
                    before = before.replace(each, b"<canary>")
                before = SECRET_LIKE.sub(b"<masked>", before)
                shown = "".join(chr(byte) if 32 <= byte < 127 else "." for byte in before)
                found.append((fields[5] if len(fields) > 5 else "[anonymous]", shown))
                at = data.find(canary, at + 1)
    return found


def broker_holds():
    """Which of the canary's secrets the broker's memory holds, after a scan
    that read some of it."""
    pid = int(environment("HATCHLING_E2E_BROKER_PID"))
    held = []
    for canary in CANARIES:
        found, scanned = memory_holds(pid, canary)
        if scanned == 0:
            raise ValueError("The broker's memory couldn't be read")
        held.append(found)
    return held


def filled(kept_held):
    """Raise unless the site got the canary sign-in, the keeper recorded its
    fill, and the broker holds it nowhere: not in its memory, its log, or a
    request the model was sent. `kept_held` is what the broker's memory
    held right after the owner kept the sign-in."""
    sent = [json.loads(line) for line in Path(environment("HATCHLING_E2E_SITE_RECORD")).read_text().splitlines()]
    if sent != [{"email": SIGN_IN_USER, "password": SIGN_IN_PASSWORD}]:
        raise ValueError(f"The site got {len(sent)} sign-ins, not the one kept: "
                         + json.dumps([{name: value is not None for name, value in each.items()} for each in sent]))
    vault_log = Path(environment("HATCHLING_E2E_VAULT_LOG")).read_text()
    if "browser-fill" not in vault_log:
        raise ValueError("The vault keeper recorded no browser fill")
    held = broker_holds()
    if any(held):
        pid = int(environment("HATCHLING_E2E_BROKER_PID"))
        raise ValueError(f"The broker's memory held the canary sign-in after its fill "
                         f"(username, password: {held}; right after it was kept: {kept_held}); "
                         f"where: {[where_held(pid, canary) for canary in CANARIES]}")
    broker_log = Path(environment("HATCHLING_E2E_BROKER_LOG")).read_bytes()
    if any(canary in broker_log for canary in CANARIES):
        raise ValueError("The broker logged the canary sign-in")
    model_requests = Path(environment("HATCHLING_E2E_MODEL_RECORD")).read_bytes()
    if any(canary in model_requests for canary in CANARIES):
        raise ValueError("A request the model was sent held the canary sign-in")
    print(f"browser end to end: the canary sign-in was filled; right after it was kept, "
          f"the broker's memory held username, password: {kept_held}")


def store_is_the_keepers():
    """Raise unless the sign-in store holds the kept sign-in and is the
    vault keeper's user's alone: its user is refused it, as the broker's,
    and the broker has nothing of it open (WO-031)."""
    store = Path(environment("HATCHLING_BROWSER_CREDENTIALS_DIR"))
    keeper, broker = pwd.getpwnam("hatchling-keeper"), pwd.getpwnam("hatchling-broker")
    files = [path for path in store.rglob("*") if path.is_file()]
    if not files:
        raise ValueError("The sign-in store holds nothing")
    if store.stat().st_mode & 0o077:
        raise ValueError(f"The sign-in store is open to others: {oct(store.stat().st_mode & 0o777)}")
    owners = {path.stat().st_uid for path in [store, *files]}
    if owners != {keeper.pw_uid}:
        raise ValueError(f"The sign-in store isn't the keeper's user's alone: {sorted(owners)}")
    for read in (["ls", "-A", str(store)], ["cat", str(files[0])]):
        refused = subprocess.run(["setpriv", f"--reuid={broker.pw_uid}", f"--regid={broker.pw_gid}",
                                  "--clear-groups", *read], capture_output=True)
        if refused.returncode == 0 or b"Permission denied" not in refused.stderr:
            raise ValueError(f"The broker's user wasn't refused the sign-in store ({read[0]})")
    descriptors = Path(f"/proc/{environment('HATCHLING_E2E_BROKER_PID')}/fd")
    opened = [os.readlink(each) for each in descriptors.iterdir()]
    if not opened or any(link.startswith(str(store)) for link in opened):
        raise ValueError("The broker has the sign-in store open, or its descriptors couldn't be read")
    print(f"browser end to end: the sign-in store's {len(files)} files are the keeper's; "
          f"the broker's user is refused it, and the broker has none of it open")


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
            kept_held = broker_holds()
            sign_in(page)
            filled(kept_held)
            store_is_the_keepers()
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
