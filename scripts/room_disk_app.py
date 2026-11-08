"""Prove the installed root service's real room disk reaches Settings.

Run `snapshot` as root, then `check <snapshot.json> <output-directory>` as
the install's owner. No key, bearer address or WebSocket trace is recorded.
"""

import json
import math
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import time
from urllib.parse import urlsplit


class ProofError(ValueError):
    """Only fixed, nonsecret diagnostics may reach the CI log."""


def snapshot(directory=Path('/var/lib/hatchling/room')):
    """Read independent filesystem evidence; never create or change a disk."""
    image = directory / 'disk.img'
    mount = directory / 'mnt'
    info = image.lstat()
    if not stat.S_ISREG(info.st_mode) or not os.path.ismount(mount):
        raise ProofError('The room image must be a regular file with a mounted filesystem')
    if info.st_size <= 0 or info.st_blocks <= 0:
        raise ProofError('The room image must occupy real storage')
    space = os.statvfs(mount)
    return {'capacity': info.st_size, 'allocated': info.st_blocks * 512, 'mounted': True,
            'used': (space.f_blocks - space.f_bfree) * space.f_frsize,
            'free': space.f_bavail * space.f_frsize}


def snapshot_as_owner():
    """A second independent read after the app's answer; no prompt or writes."""
    raw = subprocess.check_output(['sudo', '-n', sys.executable, str(Path(__file__).resolve()), 'snapshot'],
                                  text=True, stderr=subprocess.DEVNULL, timeout=30)
    return json.loads(raw)


def setup_from_frame(payload):
    """Keep only the broker's setup answer; no welcome, token or chat data."""
    try:
        message = json.loads(payload)
    except (ValueError, TypeError):
        return None
    if not isinstance(message, dict) or message.get('type') != 'action_result':
        return None
    result = message.get('result')
    if not isinstance(result, dict) or result.get('result') != 'setup':
        return None
    return result.get('setup')


SIZE_LINE = re.compile(r'It holds up to ([0-9.]+) (GB|MB), ([0-9.]+) (GB|MB) of it in use, and takes ([0-9.]+) (GB|MB) on this machine\.')
FILE_WARNING = 'It’s encrypted, but what opens it is on this same disk until you protect your vault.'


def validate_view(physical, setup, text, after=None):
    """Reject off/demo/stale values as well as missing or overstated copy."""
    after = physical if after is None else after
    if any(item.get('mounted') is not True or item.get('capacity', 0) <= 0 for item in [physical, after]):
        raise ProofError('No independent mounted-disk evidence')
    if setup.get('vault', {}).get('tier') != 'file':
        raise ProofError('This disposable install must use the file tier')
    disk = setup.get('room_disk', {})
    if disk.get('state') != 'open' or disk.get('off') is not None:
        raise ProofError('The broker did not report a successfully opened room disk')
    if disk.get('cap') != physical['capacity'] or disk['cap'] != after['capacity']:
        raise ProofError('The broker capacity differs from the real image')
    for name in ['cap', 'used', 'free', 'on_disk']:
        if type(disk.get(name)) is not int or not 0 < disk[name] <= disk['cap']:
            raise ProofError('The broker omitted real open-disk counters')
    # ext4 lazy initialization can change allocation and space while opening
    # the page. Bind the answer to independent observations on both sides;
    # never substitute a broad tolerance or an assumed immutable snapshot.
    for wire, field in [('on_disk', 'allocated'), ('used', 'used'), ('free', 'free')]:
        if not min(physical[field], after[field]) <= disk[wire] <= max(physical[field], after[field]):
            raise ProofError('The broker counters are outside the independent filesystem observations')
    match = SIZE_LINE.search(text)
    if match is None:
        raise ProofError('Settings omitted the open disk size line')
    for name, offset in [('cap', 1), ('used', 3), ('on_disk', 5)]:
        shown, unit = float(match[offset]), match[offset + 1]
        scale = 1e9 if unit == 'GB' else 1e6
        tolerance = scale * (0.05 if unit == 'GB' else 0.5)
        if not math.isfinite(shown) or abs(shown * scale - disk[name]) > tolerance + 1:
            raise ProofError('Settings differs from the broker disk counters')
    if FILE_WARNING not in text or 'Deleting files inside it doesn’t make it smaller' not in text:
        raise ProofError('Settings omitted the encryption or allocation limit')
    if any(phrase in text for phrase in ['still unlocked', 'failed to start', 'It’s off', 'couldn’t read', 'stolen disk can’t open']):
        raise ProofError('Settings made a failed-state or unconditional protection claim')
    return {name: disk[name] for name in ['cap', 'used', 'free', 'on_disk']}


def check(address, physical, output, read_after=snapshot_as_owner):
    """Use the real owner app; retain only nonsecret counters and a screenshot."""
    from playwright.sync_api import expect, sync_playwright

    parsed = urlsplit(address)
    if (parsed.scheme, parsed.netloc, parsed.path, parsed.query) != ('http', '127.0.0.1:7340', '/', '') or not re.fullmatch(r'hatchling-ui=[0-9a-f]+', parsed.fragment):
        raise ProofError('The host did not give the local owner app address')
    setups, errors, closed = [], [], []

    def received(socket, payload):
        setup = setup_from_frame(payload)
        if setup is not None:
            setups.append((socket, setup))

    def opened(socket):
        socket.on('framereceived', lambda payload: received(socket, payload))
        socket.on('close', lambda: closed.append(socket))

    with sync_playwright() as driver:
        browser = driver.chromium.launch(executable_path=os.environ.get('CHROMIUM_PATH'),
                                         args=['--no-sandbox'] if os.geteuid() == 0 else [])
        try:
            page = browser.new_page(viewport={'width': 1280, 'height': 900}, reduced_motion='reduce')
            page.on('websocket', opened)
            page.on('pageerror', lambda _error: errors.append(True))
            page.goto(address)
            expect(page.locator('#message-input')).to_be_visible()
            page.wait_for_function("!location.hash.includes('hatchling-ui=')", timeout=10_000)
            deadline = time.monotonic() + 30
            while not setups and time.monotonic() < deadline:
                page.wait_for_timeout(50)
            if not setups:
                raise ProofError('The connected broker gave no setup answer')
            page.keyboard.press('Control+,')
            page.get_by_role('navigation', name='Settings sections').get_by_role('button', name='Data controls', exact=True).click()
            block = page.locator('.room-disk')
            expect(block.get_by_role('heading', name='Your agent’s computer', exact=True)).to_be_visible()
            after = read_after()
            socket, setup = setups[-1]
            counters = validate_view(physical, setup, block.inner_text(), after)
            # The Settings pane scrolls independently of the page. DOM
            # visibility alone can pass for a block below its viewport.
            block.scroll_into_view_if_needed()
            expect(block).to_be_in_viewport(ratio=1)
            expect(page.locator('.connection-banner')).to_have_count(0)
            if socket in closed:
                raise ProofError('The setup connection closed before the screenshot')
            if errors:
                raise ProofError('The connected app raised a page error')
            # The fragment must be gone before any screenshot is saved.
            if 'hatchling-ui=' in page.url:
                raise ProofError('The app did not consume its bearer fragment')
            output.mkdir(parents=True, exist_ok=True)
            page.screenshot(path=str(output / 'room-disk-settings.png'), full_page=True)
            (output / 'proof.json').write_text(json.dumps({'before': physical, 'after': after, 'broker': counters, 'tier': 'file', 'fragment_consumed': True, 'block_in_viewport': True}, indent=2) + '\n')
        finally:
            browser.close()


def main(args):
    try:
        if args == ['snapshot']:
            print(json.dumps(snapshot()))
        elif len(args) == 3 and args[0] == 'check':
            physical = json.loads(Path(args[1]).read_text())
            address = subprocess.check_output(['hatchling-host', 'url'], text=True, stderr=subprocess.DEVNULL, timeout=30).strip()
            check(address, physical, Path(args[2]))
            print('room disk app: real mounted disk and broker counters shown in Settings; fragment consumed')
        else:
            print('usage: room_disk_app.py snapshot | check <snapshot.json> <output-directory>', file=sys.stderr)
            return 64
    except ProofError as error:
        print(f"room disk app: {error}", file=sys.stderr)
        return 1
    except Exception:
        # Playwright and subprocess errors can contain the bearer address.
        # Never print their traceback, command, response or message.
        print('room disk app: proof failed; no bearer address or trace recorded', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
