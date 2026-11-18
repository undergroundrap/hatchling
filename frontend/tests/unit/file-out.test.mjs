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
const { offeredFileDetail, offeredFolderDetail, givenInReply, givenElsewhere } = await import('../../dist/app/features/chat/file-out.js');
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
const STANDING = { files: 1, fileLimit: 64, bytes: 2048, byteLimit: 268435456, given: 1, givenLimit: 16, off: false, filesInAll: 1, filesInAllLimit: 4096, read: 2048, readLimit: 134217728 };
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
        downloadingAll: () => null,
        download: id => { asked.push(['download', id]); return Promise.resolve({ ok: true, message: 'Saved report.pdf, 2 KB.' }); },
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
test('Settings lists the Library\'s files by their whole name, with its use, Download and Remove behind a confirm', async () => {
    const port = stand();
    const ctx = context(port);
    let drawn = 0;
    const [section] = liveAgentFiles(ctx, () => drawn++);
    const words = text(section);
    assert.match(words, /^Files and folders the agent gave you/u);
    assert.match(words, /1 of 64 items \(1 of 4096 files\), 2 KB of 256 MB\. The agent has given 1 of the 16 files or folders it can in this conversation\./u);
    assert.match(words, /report\.pdf2 KB · kept 2026-11-17\. Made by the agent: open it only if you trust what it made\.SHA-256 ab/u);
    buttonNamed(section, 'Download').fire('click');
    await flush();
    assert.deepEqual(port.asked, [['download', 'f1']]);
    buttonNamed(section, 'Remove').fire('click');
    const [asked] = ctx.shown.confirms;
    assert.equal(asked.title, 'Remove report.pdf from Hatchling’s Library?');
    assert.deepEqual(asked.options, { label: 'Remove file', danger: true });
    // (A file keeps its own confirm; folders and unnamed items have theirs.)
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
    const said = (standing, files = [FILE]) => text(liveAgentFiles(context(stand({ current: () => ({ kind: 'read', files, folders: [], unnamed: [], standing: { ...STANDING, ...standing } }) })), () => undefined)[0]);
    assert.match(said({ off: true }), /Giving is off for this conversation: something held a secret Hatchling keeps\./u);
    assert.match(said({ given: 16 }), /The agent has given all 16 files or folders it can in this conversation\./u);
    assert.match(said({ files: 64 }), /The Library is full: remove something before the agent can give more\./u);
    assert.match(said({ bytes: 268435456 }), /The Library is full/u);
    assert.match(said({}, []), /Nothing yet\. Files and folders the agent gives you in the main chat show here\./u);
    // A name the broker never gives is no row.
    assert.doesNotMatch(said({}, [{ ...FILE, name: '../evil.exe' }]), /evil/u);
});
test('Settings says the Library as read: not yet, why not, or nothing on an install without one', () => {
    const shown = (state, overrides = {}) => text(liveAgentFiles(context(stand({ current: () => state, ...overrides })), () => undefined)[0]);
    assert.match(shown({ kind: 'unread' }), /Reading Hatchling’s Library…/u);
    assert.match(shown({ kind: 'failed', detail: 'Hatchling couldn’t read its Library.' }), /Hatchling couldn’t read its Library\./u);
    assert.match(shown({ kind: 'read', files: [FILE], folders: [], unnamed: [], standing: STANDING }, { downloading: () => true }), /Downloading…/u);
    assert.deepEqual(liveAgentFiles(context(undefined), () => undefined), []);
});
test('Settings offers every file as one .zip only where there is more than one', async () => {
    const two = { kind: 'read', files: [FILE, { ...FILE, id: 'f2', name: 'b.txt' }], folders: [], unnamed: [], standing: STANDING };
    const port = stand({ current: () => two });
    const ctx = context(port);
    const [section] = liveAgentFiles(ctx, () => undefined);
    buttonNamed(section, 'Download all as one .zip').fire('click');
    await flush();
    assert.deepEqual(port.asked, [['download all']]);
    assert.deepEqual(ctx.shown.toasts, [['Gave your browser hatchling-library.zip, 2 files, 4 KB, to save.', false]]);
    const busy = liveAgentFiles(context(stand({ current: () => two, downloadingAll: () => ({ done: 1, total: 2 }) })), () => undefined)[0];
    assert.equal(buttonNamed(busy, 'Download all as one .zip'), undefined);
    assert.match(text(busy), /Downloading all: 1 of 2 files…/u);
    assert.equal(buttonNamed(liveAgentFiles(context(stand()), () => undefined)[0], 'Download all as one .zip'), undefined, 'offered for one file');
    const failing = stand({ current: () => two, downloadAll: () => Promise.resolve({ ok: false, message: 'b.txt: The file didn’t arrive whole.' }) });
    const failed = context(failing);
    buttonNamed(liveAgentFiles(failed, () => undefined)[0], 'Download all as one .zip').fire('click');
    await flush();
    assert.deepEqual(failed.shown.toasts, [['b.txt: The file didn’t arrive whole.', true]]);
});
test('Settings lists a folder by its name, files and size, and an item it can\'t name by its size, each with Remove behind a confirm', async () => {
    const folder = { id: 'd'.repeat(64), name: 'site', files: 3, bytes: 4096, keptAt: '2026-11-18T00:00:00Z' };
    const one = { ...folder, id: 'e'.repeat(64), name: 'one', files: 1 };
    const unnamed = { id: 'c'.repeat(64), bytes: 10, removable: true };
    const stuck = { id: 'not-an-id', bytes: 5, removable: false };
    const port = stand({ current: () => ({ kind: 'read', files: [], folders: [folder, one], unnamed: [unnamed, stuck], standing: STANDING }) });
    const ctx = context(port);
    let drawn = 0;
    const [section] = liveAgentFiles(ctx, () => drawn++);
    const words = text(section);
    assert.match(words, /site.*A folder of 3 files, 4 KB · kept 2026-11-18\. Downloading a folder comes in a later version of Hatchling\./u);
    assert.match(words, /oneA folder of 1 file,/u);
    assert.match(words, /An item Hatchling can’t name10 bytes\. It isn’t one Hatchling gives/u);
    assert.match(words, /5 bytes\. .*Its id isn’t one Hatchling gives either, so it can’t be removed from here/u);
    assert.doesNotMatch(words, /Nothing yet/u);
    const all = (node, out = []) => {
        if (node instanceof FakeElement && node.tagName === 'button' && text(node) === 'Remove')
            out.push(node);
        for (const child of node.childNodes)
            if (child instanceof FakeElement)
                all(child, out);
        return out;
    };
    const removes = all(section);
    assert.equal(removes.length, 3);
    removes[0].fire('click');
    removes[2].fire('click');
    const [first, last] = ctx.shown.confirms;
    assert.equal(first.title, 'Remove the folder site and its 3 files from Hatchling’s Library?');
    assert.equal(last.title, 'Remove this item from Hatchling’s Library?');
    assert.deepEqual(first.options, { label: 'Remove', danger: true });
    assert.deepEqual(port.asked, [], 'removed before the owner said yes');
    first.action();
    last.action();
    await flush();
    assert.deepEqual(port.asked, [['remove', folder.id], ['remove', unnamed.id]]);
    assert.equal(drawn, 2);
});
test('Settings says when the conversation has read all it can, the Library holds all the files it can, and what Download all leaves out', () => {
    const said = (standing, extra = {}) => text(liveAgentFiles(context(stand({ current: () => ({ kind: 'read', files: [FILE], folders: [], unnamed: [], standing: { ...STANDING, ...standing }, ...extra }) })), () => undefined)[0]);
    assert.match(said({ read: 134217728 }), /The agent has read all 128 MB it can out of its computer in this conversation\./u);
    assert.match(said({ filesInAll: 4096 }), /The Library is full/u);
    const folder = { id: 'd'.repeat(64), name: 'site', files: 3, bytes: 4096, keptAt: '2026-11-18T00:00:00Z' };
    const two = [FILE, { ...FILE, id: 'f2', name: 'b.txt' }];
    assert.match(said({}, { files: two, folders: [folder] }), /Download all files as one \.zip \(not the folders\)/u);
    assert.doesNotMatch(said({}, { files: two }), /not the folders/u);
});
test('Settings says nothing yet only when the Library holds no file, folder or item at all', () => {
    const said = (state) => text(liveAgentFiles(context(stand({ current: () => ({ kind: 'read', files: [], folders: [], unnamed: [], standing: STANDING, ...state }) })), () => undefined)[0]);
    assert.match(said({}), /Nothing yet/u);
    assert.doesNotMatch(said({ folders: [{ id: 'd'.repeat(64), name: 'site', files: 1, bytes: 1, keptAt: '2026-11-18T00:00:00Z' }] }), /Nothing yet/u);
    assert.doesNotMatch(said({ unnamed: [{ id: 'c'.repeat(64), bytes: 1, removable: true }] }), /Nothing yet/u);
});
const folderOf = { id: 'd'.repeat(64), name: 'site', files: 3, bytes: 4096, keptAt: '2026-11-18T00:00:00Z', reply: 'run-1', conversation: 1 };
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
    assert.equal(text(shown), 'site · a folder of 3 files, 4 KB. From the agent’s computer. Downloading a folder comes in a later version of Hatchling.report.pdf · 2 KB. From the agent’s computer. Made by the agent: open it only if you trust what it made. Downloadgone.txt · Removed from Hatchling’s Library since.away · a folder of 1 file. Removed from Hatchling’s Library since.');
    buttonNamed(shown, 'Download').fire('click');
    await flush();
    assert.deepEqual(port.asked, [['download', 'f1']]);
    // Nothing under the owner's words, another answer, or with no Library.
    assert.equal(givenInReply(ctx, { ...answer, role: 'user' }), null);
    assert.equal(givenInReply(ctx, { ...answer, id: 'm2' }), null);
    assert.equal(givenInReply(context(undefined), answer), null);
});
test('what the conversation gave under no reply shown sits apart, under none', () => {
    const block = givenElsewhere(context(stand({ givenElsewhere: () => cards.slice(0, 1) })));
    assert.equal(text(block), 'Given in this conversation, not under a reply shown here.site · a folder of 3 files, 4 KB. From the agent’s computer. Downloading a folder comes in a later version of Hatchling.');
    assert.equal(givenElsewhere(context(stand())), null);
    assert.equal(givenElsewhere(context(undefined)), null);
});
test('the chat\'s notice of a folder given says its files, or that it was removed since', () => {
    const port = stand({ offeredFolder: id => id === 'fold-a' ? { ...folderOf, removed: false } : id === 'fold-b' ? { ...folderOf, removed: true } : undefined });
    const ctx = context(port);
    assert.equal(text(offeredFolderDetail(ctx, notice('fold-a'))), 'From the agent’s computer: 3 files. Downloading a folder comes in a later version of Hatchling.');
    assert.equal(text(offeredFolderDetail(ctx, notice('fold-b'))), 'Removed from Hatchling’s Library since.');
    assert.equal(offeredFolderDetail(ctx, notice('fold-c')), null);
    assert.equal(offeredFolderDetail(ctx, { id: 'fold-a', kind: 'error', text: 'x' }), null);
    assert.equal(offeredFolderDetail(context(undefined), notice('fold-a')), null);
});
