// The agent's computer, its room disk, as the app says it: each state, each
// vault tier, and only what's true of this install.
import assert from 'node:assert/strict';
import test from 'node:test';
import { NO_SHRINK, ON_WINDOWS, STILL_UNLOCKED, diskSize, roomDiskLines } from '../../dist/app/session/room-disk.js';
import { liveRoomDisk } from '../../dist/app/features/settings/live-data.js';
const GB = 1e9;
const open = { state: 'open', off: null, cap: 10 * GB, used: 1.2 * GB, free: 8.8 * GB, onDisk: 3.4 * GB, hostFree: 90 * GB };
const closed = { ...open, state: 'closed', used: null, free: null };
const STOLEN = 'It’s encrypted, and a stolen disk can’t open it.';
const UNPROTECTED = 'It’s encrypted, but what opens it is on this same disk until you protect your vault.';
test('open, it says its size, what is in use and what it takes here, and why it never shrinks', () => {
    assert.deepEqual(roomDiskLines(open, 'hardware_key', false), [
        'It holds up to 10.0 GB, 1.2 GB of it in use, and takes 3.4 GB on this machine.',
        NO_SHRINK,
        STOLEN,
    ]);
    assert.equal(NO_SHRINK, 'Deleting files inside it doesn’t make it smaller on your disk. Giving space back would tell your disk which parts are in use, and hiding that is part of what the encryption is for.');
    assert.deepEqual(roomDiskLines(closed, 'hardware_key', false)[0], 'It holds up to 10.0 GB and takes 3.4 GB on this machine.', 'closed, nothing inside is read');
});
test('only a security key makes a stolen disk useless, and nothing else says so', () => {
    for (const tier of ['file', 'totp']) {
        for (const disk of [open, closed, { ...closed, state: 'off', off: 'not_opened' }]) {
            const said = roomDiskLines(disk, tier, false);
            assert.ok(!said.some(line => /stolen disk can|protected/u.test(line)), `${tier}: a protected vault was claimed on an unprotected install`);
            assert.ok(said.includes(UNPROTECTED), `${tier}: what opens it wasn't said`);
        }
    }
    assert.ok(!roomDiskLines(open, 'hardware_key', false).includes(UNPROTECTED));
});
test('a Windows install hears that Windows keeps the space too, and only it', () => {
    assert.ok(roomDiskLines(open, 'file', true).includes(ON_WINDOWS));
    assert.ok(!roomDiskLines(open, 'file', false).includes(ON_WINDOWS));
});
test('off, it says why in plain words, and its sizes when there are some', () => {
    assert.deepEqual(roomDiskLines({ ...open, state: 'off', off: 'not_here', cap: null, used: null, free: null, onDisk: null, hostFree: null }, 'file', false), ['It’s off on this install: it needs Hatchling running as a service, as root.']);
    const failed = roomDiskLines({ ...closed, state: 'off', off: 'not_opened' }, 'hardware_key', false);
    assert.equal(failed[0], 'It’s off: it couldn’t be opened when Hatchling started. hatchling-broker doctor, run as root, says why.');
    assert.ok(failed.includes(NO_SHRINK) && failed.includes(STOLEN), 'a disk that is there still takes its space');
    assert.deepEqual(roomDiskLines({ ...open, state: 'off', off: 'not_read', cap: null }, 'file', false), ['Hatchling couldn’t read how it is just now.']);
    assert.deepEqual(roomDiskLines({ ...open, state: 'off', off: null, onDisk: null }, 'file', false), ['Hatchling couldn’t read how it is just now.']);
});
test('unlocked but unused, it says so first, and is never said to be closed', () => {
    const said = roomDiskLines({ ...closed, state: 'mapped' }, 'hardware_key', false);
    assert.equal(said[0], STILL_UNLOCKED, 'a room disk still unlocked was said as if it were closed');
    assert.ok(said.includes(NO_SHRINK) && said.includes(STOLEN));
    assert.ok(!roomDiskLines(closed, 'hardware_key', false).includes(STILL_UNLOCKED));
});
test('none made yet says so, and nothing else', () => {
    assert.deepEqual(roomDiskLines({ ...open, state: 'absent' }, 'hardware_key', true), ['It hasn’t been made yet. Hatchling makes it when it starts.']);
});
test('sizes read as a person reads them', () => {
    assert.equal(diskSize(10 * GB), '10.0 GB');
    assert.equal(diskSize(GB), '1.0 GB');
    assert.equal(diskSize(300 * 1e6), '300 MB');
});
// ---- A minimal DOM, as in live-adapter.test.mjs.
class FakeNode {
    childNodes = [];
    append(...nodes) { this.childNodes.push(...nodes); }
}
class FakeText extends FakeNode {
    constructor(data) { super(); this.data = data; }
}
class FakeElement extends FakeNode {
    attributes = new Map();
    constructor(tag) { super(); this.tagName = tag; }
    setAttribute(name, value) { this.attributes.set(name, value); }
}
const text = node => node instanceof FakeText ? node.data : node.childNodes.map(text).join('|');
test('Settings shows the agent’s computer once it has been read, and nothing before', () => {
    globalThis.Node = FakeNode;
    globalThis.document = { createElement: tag => new FakeElement(tag), createTextNode: data => new FakeText(data) };
    const ctx = state => ({ setup: { current: () => state } });
    const read = (roomDisk, tier = 'totp') => ({ kind: 'read', setup: { tier, keys: 0, codes: 0, codesUnconfirmed: false, lastBackup: null, windowsSetup: true, roomDisk } });
    const [block] = liveRoomDisk(ctx(read(open)));
    const shown = text(block);
    assert.match(shown, /^Your agent’s computer\|/u);
    assert.ok(shown.includes(NO_SHRINK) && shown.includes(ON_WINDOWS) && shown.includes(UNPROTECTED));
    assert.ok(!shown.includes(STOLEN), 'a protected vault claimed on an unprotected install');
    for (const unknown of [{ kind: 'unread' }, { kind: 'failed', detail: 'x' }, read(null)])
        assert.deepEqual(liveRoomDisk(ctx(unknown)), [], 'said only of what has been read');
    assert.deepEqual(liveRoomDisk({}), []);
    delete globalThis.document;
    delete globalThis.Node;
});
