// A file the agent gave the owner, as the chat and Settings show it (WO-041):
// beside the chat's notice, where it came from, what opening it can do and
// Download; and in Settings, the Library's use and its files, each with
// Download and Remove behind a confirm. Over a minimal DOM that records
// listeners, so buttons can be pressed.
import test from 'node:test';
import assert from 'node:assert/strict';
class FakeNode {
    childNodes = [];
    append(...nodes) { this.childNodes.push(...nodes); }
}
class FakeText extends FakeNode {
    constructor(data) { super(); this.data = data; }
}
class FakeElement extends FakeNode {
    attributes = new Map();
    listeners = new Map();
    constructor(tag) { super(); this.tagName = tag; }
    setAttribute(name, value) { this.attributes.set(name, value); }
    addEventListener(name, handler) { this.listeners.set(name, handler); }
    fire(name) { this.listeners.get(name)?.(); }
}
globalThis.Node = FakeNode;
globalThis.document = { createElement: tag => new FakeElement(tag), createElementNS: (namespace, tag) => new FakeElement(tag), createTextNode: data => new FakeText(data) };
const { offeredFileDetail } = await import('../../dist/app/features/chat/file-out.js');
const { liveAgentFiles } = await import('../../dist/app/features/settings/live-agent-files.js');
const text = (node) => node instanceof FakeText ? node.data : node.childNodes.map(text).join('');
const find = (node, predicate) => {
    if (node instanceof FakeElement && predicate(node))
        return node;
    for (const child of node.childNodes) {
        const found = child instanceof FakeElement ? find(child, predicate) : undefined;
        if (found)
            return found;
    }
    return undefined;
};
const buttonNamed = (node, label) => find(node, element => element.tagName === 'button' && text(element) === label);
const flush = () => new Promise(resolve => setImmediate(resolve));
const FILE = { id: 'f1', name: 'report.pdf', bytes: 2048, sha256: 'ab', keptAt: '2026-11-17T09:00:00Z' };
const STANDING = { files: 1, fileLimit: 64, bytes: 2048, byteLimit: 268435456, given: 1, givenLimit: 16, off: false };
/** A stand-in Library that records what it was asked. */
function stand(overrides = {}) {
    const asked = [];
    return {
        asked,
        current: () => ({ kind: 'read', files: [FILE], standing: STANDING }),
        refresh: () => { asked.push(['refresh']); return Promise.resolve(); },
        offered: id => id === 'file-a' ? { ...FILE, removed: false } : undefined,
        downloading: () => false,
        download: id => { asked.push(['download', id]); return Promise.resolve({ ok: true, message: 'Saved report.pdf, 2 KB.' }); },
        remove: id => { asked.push(['remove', id]); return Promise.resolve({ ok: true, message: 'Removed from Hatchling’s Library.' }); },
        ...overrides,
    };
}
/** An app context with `agentFiles`, recording what it was asked to show. */
function context(agentFiles) {
    const shown = { toasts: [], confirms: [] };
    return {
        shown,
        state: { mode: 'live' },
        agentFiles,
        toast: (message, error) => shown.toasts.push([message, error === true]),
        dialogs: { confirm: (title, description, action, options) => shown.confirms.push({ title, description, action, options }) },
    };
}
const notice = (id) => ({ id, kind: 'notice', text: 'Hatchling gave you a file: report.pdf, 2 KB.' });
test('the chat\'s notice of a file given says where it came from and offers its download', async () => {
    const port = stand();
    const ctx = context(port);
    const detail = offeredFileDetail(ctx, notice('file-a'));
    assert.equal(text(detail), 'From the agent’s computer. Made by the agent: open it only if you trust what it made. Download');
    buttonNamed(detail, 'Download').fire('click');
    await flush();
    assert.deepEqual(port.asked, [['download', 'f1']]);
    assert.deepEqual(ctx.shown.toasts, [['Saved report.pdf, 2 KB.', false]]);
    // A program is said to be one.
    const program = stand({ offered: () => ({ ...FILE, name: 'setup.exe', removed: false }) });
    assert.match(text(offeredFileDetail(context(program), notice('file-a'))), /A program: it runs when opened\./u);
    // One being downloaded offers no second download.
    const busy = stand({ downloading: () => true });
    const going = offeredFileDetail(context(busy), notice('file-a'));
    assert.equal(buttonNamed(going, 'Download'), undefined);
    assert.match(text(going), /Downloading…$/u);
    // A failed download is said as an error.
    const failing = stand({ download: () => Promise.resolve({ ok: false, message: 'The file didn’t arrive whole.' }) });
    const failed = context(failing);
    buttonNamed(offeredFileDetail(failed, notice('file-a')), 'Download').fire('click');
    await flush();
    assert.deepEqual(failed.shown.toasts, [['The file didn’t arrive whole.', true]]);
});
test('a file removed since, another message, or no Library shows nothing to download', () => {
    const removed = stand({ offered: () => ({ ...FILE, removed: true }) });
    assert.equal(text(offeredFileDetail(context(removed), notice('file-a'))), 'Removed from Hatchling’s Library since.');
    assert.equal(offeredFileDetail(context(stand()), notice('file-b')), null);
    assert.equal(offeredFileDetail(context(stand()), { id: 'file-a', kind: 'error', text: 'x' }), null);
    assert.equal(offeredFileDetail(context(undefined), notice('file-a')), null);
});
test('Settings lists the Library\'s files by their whole name, with its use, Download and Remove behind a confirm', async () => {
    const port = stand();
    const ctx = context(port);
    let drawn = 0;
    const [section] = liveAgentFiles(ctx, () => drawn++);
    const words = text(section);
    assert.match(words, /^Files the agent gave you/u);
    assert.match(words, /1 of 64 files, 2 KB of 256 MB\. The agent has given 1 of 16 files in this conversation\./u);
    assert.match(words, /report\.pdf2 KB · kept 2026-11-17\. Made by the agent: open it only if you trust what it made\.SHA-256 ab/u);
    buttonNamed(section, 'Download').fire('click');
    await flush();
    assert.deepEqual(port.asked, [['download', 'f1']]);
    buttonNamed(section, 'Remove').fire('click');
    const [asked] = ctx.shown.confirms;
    assert.equal(asked.title, 'Remove report.pdf from Hatchling’s Library?');
    assert.deepEqual(asked.options, { label: 'Remove file', danger: true });
    assert.deepEqual(port.asked, [['download', 'f1']], 'removed before the owner said yes');
    asked.action();
    await flush();
    assert.deepEqual(port.asked.at(-1), ['remove', 'f1']);
    assert.equal(drawn, 1);
    buttonNamed(section, 'Refresh').fire('click');
    await flush();
    assert.deepEqual(port.asked.at(-1), ['refresh']);
    assert.equal(drawn, 2);
});
test('Settings says when the Library is full, giving is off, or the conversation has given all it can', () => {
    const said = (standing, files = [FILE]) => text(liveAgentFiles(context(stand({ current: () => ({ kind: 'read', files, standing: { ...STANDING, ...standing } }) })), () => undefined)[0]);
    assert.match(said({ off: true }), /Giving files is off for this conversation: a file held a secret Hatchling keeps\./u);
    assert.match(said({ given: 16 }), /The agent has given all 16 files it can in this conversation\./u);
    assert.match(said({ files: 64 }), /The Library is full: remove a file before the agent can give another\./u);
    assert.match(said({ bytes: 268435456 }), /The Library is full/u);
    assert.match(said({}, []), /Nothing yet\. Files the agent gives you in the main chat show here\./u);
    // A name the broker never gives is no row.
    assert.doesNotMatch(said({}, [{ ...FILE, name: '../evil.exe' }]), /evil/u);
});
test('Settings says the Library as read: not yet, why not, or nothing on an install without one', () => {
    const shown = (state, overrides = {}) => text(liveAgentFiles(context(stand({ current: () => state, ...overrides })), () => undefined)[0]);
    assert.match(shown({ kind: 'unread' }), /Reading Hatchling’s Library…/u);
    assert.match(shown({ kind: 'failed', detail: 'Hatchling couldn’t read its Library.' }), /Hatchling couldn’t read its Library\./u);
    assert.match(shown({ kind: 'read', files: [FILE], standing: STANDING }, { downloading: () => true }), /Downloading…/u);
    assert.deepEqual(liveAgentFiles(context(undefined), () => undefined), []);
});
