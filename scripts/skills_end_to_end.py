"""Enable one of Hatchling's skills in the app, and see the agent use it,
as the owner would (WO-046): run by scripts/skills_end_to_end.sh, which
starts the broker with the skills an install ships, the scripted skills
model, and the app's host.

Settings, Skills lists the three shipped skills, each waiting for the
owner, signed by the key the shipped trust roots name. The owner reviews
write-a-report, its playbook among its files, and enables it. Asked for a
report in the chat, the agent's model finds that skill listed, and no other,
reads its signed playbook through the broker, and answers with a line of
it. The other two still wait.

Usage, as root: skills_end_to_end.py <app-address>, with
HATCHLING_E2E_SCREENSHOTS naming a directory for screenshots.
"""
import sys

from approval_end_to_end import screenshot
from skills_model import ENABLED, MARKER, READ, TASK, WAITING, signer

SHIPPED = sorted((ENABLED,) + WAITING)
# A chat run starts cells for the turn: give it longer than a step.
TURN_TIMEOUT_MS = 120_000
STEP_TIMEOUT_MS = 30_000


def skills_pane(page):
    """Settings, Skills: the pane, its rows checked against what ships."""
    from playwright.sync_api import expect

    page.keyboard.press("Control+,")
    page.get_by_role("navigation", name="Settings sections").get_by_role("button", name="Skills", exact=True).click()
    pane = page.locator(".settings-pane")
    rows = pane.locator(".skill-row")
    # Opened as soon as the app loads, the pane may be drawn before the app
    # has the list: it's drawn again when the list comes (#625).
    expect(rows).to_have_count(len(SHIPPED), timeout=STEP_TIMEOUT_MS)
    names = sorted(row.get_attribute("data-skill") for row in rows.all())
    if names != SHIPPED:
        raise AssertionError(f"the skills listed: {names}")
    return pane


def states(pane):
    return {row.get_attribute("data-skill"): row.get_attribute("data-state") for row in pane.locator(".skill-row").all()}


def enable(page, fingerprint):
    """Review ENABLED as the owner would, and enable it."""
    from playwright.sync_api import expect

    pane = skills_pane(page)
    if set(states(pane).values()) != {"waiting"}:
        raise AssertionError(f"before the owner enabled any: {states(pane)}")
    screenshot(page, "skills-1-waiting.png")
    pane.get_by_role("button", name=f"Review {ENABLED}", exact=True).click()
    detail = pane.locator(".skill-detail")
    expect(detail).to_be_visible(timeout=STEP_TIMEOUT_MS)
    signed_by = detail.locator(".skill-facts dd").nth(1)
    if signed_by.get_attribute("title") != fingerprint:
        raise AssertionError(f"signed by: {signed_by.get_attribute('title')}")
    files = detail.locator(".skill-files li").all_inner_texts()
    if not any(line.startswith("SKILL.md") and line.endswith("its playbook") for line in files):
        raise AssertionError(f"its files: {files}")
    screenshot(page, "skills-2-review.png")
    detail.get_by_role("button", name="Enable", exact=True).click()
    page.locator(".confirm-dialog").get_by_role("button", name="Enable skill", exact=True).click()
    expect(detail.get_by_role("button", name="Disable", exact=True)).to_be_visible(timeout=STEP_TIMEOUT_MS)


def main():
    from playwright.sync_api import expect, sync_playwright

    address = sys.argv[1]
    fingerprint = signer()
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True, args=["--no-sandbox"])
        page = browser.new_page(viewport={"width": 1400, "height": 900})
        errors = []
        page.on("pageerror", lambda error: errors.append(str(error)))
        page.goto(address)
        enable(page, fingerprint)
        # Settings is a dialog over the chat: a reload opens the app afresh,
        # where going to the same address only changes its hash.
        page.reload()
        page.locator("#message-input").fill(TASK)
        page.get_by_role("button", name="Send message", exact=True).click()
        said = page.locator(".message-row.kind-text").filter(has_text=READ)
        expect(said).to_have_count(1, timeout=TURN_TIMEOUT_MS)
        answer = said.inner_text()
        if MARKER not in " ".join(answer.split()):
            raise AssertionError(f"the agent's answer: {answer!r}")
        screenshot(page, "skills-3-used.png")
        page.reload()
        pane = skills_pane(page)
        expect(pane.locator(f'.skill-row[data-skill="{ENABLED}"]')).to_have_attribute("data-state", "enabled")
        left = {name: state for name, state in states(pane).items() if name != ENABLED}
        if set(left.values()) != {"waiting"}:
            raise AssertionError(f"the others: {left}")
        screenshot(page, "skills-4-after.png")
        browser.close()
        if errors:
            raise AssertionError(f"the page's errors: {errors}")
    print(f"enabled {ENABLED} in the app, and the agent read its signed playbook: {answer.strip()[:120]}")


if __name__ == "__main__":
    main()
