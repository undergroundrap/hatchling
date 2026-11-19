// A file the agent gave the owner, as the chat shows it (WO-041): beside the
// chat's notice, where it came from, what opening it can do and Download.
// The Library page's own tests are in library-page.test.mjs. Over a minimal
// DOM that records listeners, so buttons can be pressed.
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
const { offeredFileDetail, offeredFolderDetail, givenInReply, givenElsewhere } = await import('../../dist/app/features/chat/file-out.js');
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
const FILE = { id: 'f1', name: 'report.pdf', bytes: 2048, sha256: 'ab', keptAt: '2026-11-17T09:00:00Z', reply: 'run-1', conversation: 1 };
const NONE_LEFT = { links: 0, mounts: 0, others: 0 };
const STANDING = { files: 1, fileLimit: 64, bytes: 2048, byteLimit: 268435456, given: 1, givenLimit: 16, off: false, filesInAll: 1, filesInAllLimit: 4096, read: 2048, readLimit: 134217728, conversation: 1 };
/** A stand-in Library that records what it was asked. */
function stand(overrides = {}) {
    const asked = [];
    return {
        asked,
        current: () => ({ kind: 'read', files: [FILE], folders: [], unnamed: [], standing: STANDING }),
        refresh: () => { asked.push(['refresh']); return Promise.resolve(); },
        offered: id => id === 'file-a' ? { ...FILE, removed: false } : undefined,
        offeredFolder: () => undefined,
        givenIn: () => [],
        givenElsewhere: () => [],
        downloading: () => false,
        downloadProgress: () => null,
        download: id => { asked.push(['download', id]); return Promise.resolve({ ok: true, message: 'Saved report.pdf, 2 KB.' }); },
        downloadMany: (ids, name) => { asked.push(['download many', [...ids], name]); return Promise.resolve({ ok: true, message: `Gave your browser ${name}.` }); },
        chosen: () => new Set(),
        choose: (id, on) => { asked.push(['choose', id, on]); },
        downloadAll: () => { asked.push(['download all']); return Promise.resolve({ ok: true, message: 'Gave your browser hatchling-library.zip, 2 files, 4 KB, to save.' }); },
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
const folderOf = { id: 'd'.repeat(64), name: 'site', files: 3, bytes: 4096, sha256: 'cd', leftOut: NONE_LEFT, keptAt: '2026-11-18T00:00:00Z', reply: 'run-1', conversation: 1 };
const cards = [
    { kind: 'folder', folder: folderOf },
    { kind: 'file', file: { ...FILE, reply: 'run-1', conversation: 1 } },
    { kind: 'removed', removed: { id: 'e'.repeat(64), name: 'gone.txt', folder: false, files: 1, bytes: 2, reply: 'run-1' } },
    { kind: 'removed', removed: { id: 'c'.repeat(64), name: 'away', folder: true, files: 1, bytes: 9, reply: 'run-1' } },
];
test('an answer shows what its run gave: a file with Download, a folder by its files, and one removed since', async () => {
    const port = stand({ givenIn: id => id === 'm1' ? cards : [] });
    const ctx = context(port);
    const answer = { id: 'm1', kind: 'text', role: 'agent', text: 'Here.' };
    const shown = givenInReply(ctx, answer);
    assert.equal(text(shown), 'site · a folder. From the agent’s computer: 3 files, 4 KB. Download as .zipreport.pdf · 2 KB. From the agent’s computer. Made by the agent: open it only if you trust what it made. Downloadgone.txt · Removed from Hatchling’s Library since.away · a folder of 1 file. Removed from Hatchling’s Library since.');
    buttonNamed(shown, 'Download').fire('click');
    buttonNamed(shown, 'Download as .zip').fire('click');
    await flush();
    assert.deepEqual(port.asked, [['download', 'f1'], ['download', folderOf.id]]);
    // A folder being downloaded offers no second download.
    assert.equal(buttonNamed(givenInReply(context(stand({ givenIn: () => cards.slice(0, 1), downloading: () => true })), answer), 'Download as .zip'), undefined);
    // Nothing under the owner's words, another answer, or with no Library.
    assert.equal(givenInReply(ctx, { ...answer, role: 'user' }), null);
    assert.equal(givenInReply(ctx, { ...answer, id: 'm2' }), null);
    assert.equal(givenInReply(context(undefined), answer), null);
});
test('what the conversation gave under no reply shown sits apart, under none', () => {
    const block = givenElsewhere(context(stand({ givenElsewhere: () => cards.slice(0, 1) })));
    assert.equal(text(block), 'Given in this conversation, not under a reply shown here.site · a folder. From the agent’s computer: 3 files, 4 KB. Download as .zip');
    assert.equal(givenElsewhere(context(stand())), null);
    assert.equal(givenElsewhere(context(undefined)), null);
});
test('the chat\'s notice of a folder given says its files, or that it was removed since', () => {
    const port = stand({ offeredFolder: id => id === 'fold-a' ? { ...folderOf, removed: false } : id === 'fold-b' ? { ...folderOf, removed: true } : undefined });
    const ctx = context(port);
    const offered = offeredFolderDetail(ctx, notice('fold-a'));
    assert.equal(text(offered), 'From the agent’s computer: 3 files, 4 KB. Download as .zip');
    buttonNamed(offered, 'Download as .zip').fire('click');
    assert.deepEqual(port.asked, [['download', folderOf.id]]);
    // What its walk left out is said.
    const leaving = stand({ offeredFolder: () => ({ ...folderOf, leftOut: { links: 2, mounts: 1, others: 0 }, removed: false }) });
    assert.equal(text(offeredFolderDetail(context(leaving), notice('fold-a'))), 'From the agent’s computer: 3 files, 4 KB. Left out when it was given: 2 links, 1 other drive. Download as .zip');
    assert.equal(text(offeredFolderDetail(ctx, notice('fold-b'))), 'Removed from Hatchling’s Library since.');
    assert.equal(offeredFolderDetail(ctx, notice('fold-c')), null);
    assert.equal(offeredFolderDetail(ctx, { id: 'fold-a', kind: 'error', text: 'x' }), null);
    assert.equal(offeredFolderDetail(context(undefined), notice('fold-a')), null);
});
/** Every element of `node` that `predicate` holds for, in order. */
const findAll = (node, predicate, out = []) => {
    if (node instanceof FakeElement && predicate(node))
        out.push(node);
    for (const child of node.childNodes)
        if (child instanceof FakeElement)
            findAll(child, predicate, out);
    return out;
};
