// Giving the agent a file, as the chat and Settings show it (WO-041): the menu
// item, each send's row and what it offers, the owner's name beside the
// chat's notice (inert, isolated, hidden characters shown), Remove behind a
// confirm, and the inbox's list in Settings. Over a minimal DOM that records
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
const made = [];
class FakeElement extends FakeNode {
    attributes = new Map();
    listeners = new Map();
    files = null;
    clicked = 0;
    constructor(tag) { super(); this.tagName = tag; made.push(this); }
    setAttribute(name, value) { this.attributes.set(name, value); }
    addEventListener(name, handler) { this.listeners.set(name, handler); }
    click() { this.clicked++; }
    fire(name) { this.listeners.get(name)?.(); }
}
globalThis.Node = FakeNode;
globalThis.document = { createElement: tag => new FakeElement(tag), createElementNS: (namespace, tag) => new FakeElement(tag), createTextNode: data => new FakeText(data) };
const { fileSendRows, giveFile, givenFileDetail, offersFiles, takesFiles } = await import('../../dist/app/features/chat/file-in.js');
const { liveInbox } = await import('../../dist/app/features/settings/live-inbox.js');
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
const buttonNamed = (node, label) => find(node, element => element.tagName === 'button' && (text(element) === label || element.attributes.get('aria-label') === label));
const flush = () => new Promise(resolve => setImmediate(resolve));
/** A stand-in inbox that records what it was asked. */
function stand(overrides = {}) {
    const asked = [];
    return {
        asked,
        takes: chatId => chatId === 'main',
        give: (chatId, file) => asked.push(['give', chatId, file.name]),
        sends: () => [],
        retry: id => asked.push(['retry', id]),
        dismiss: id => asked.push(['dismiss', id]),
        given: () => undefined,
        current: () => ({ kind: 'unread' }),
        refresh: () => { asked.push(['refresh']); return Promise.resolve(); },
        remove: name => { asked.push(['remove', name]); return Promise.resolve({ ok: true, message: 'Removed from the agent’s inbox.' }); },
        ...overrides,
    };
}
/** An app context with `inbox`, recording what it was asked to show. */
function context(inbox, mode = 'live') {
    const shown = { toasts: [], confirms: [], settings: [] };
    return {
        shown,
        state: { mode },
        ui: { chatId: 'main' },
        inbox,
        toast: (message, error) => shown.toasts.push([message, error === true]),
        openSettings: tab => shown.settings.push(tab),
        dialogs: { confirm: (title, description, action, options) => shown.confirms.push({ title, description, action, options }) },
    };
}
test('only a live install offers a file, and only in the main chat', () => {
    const inbox = stand();
    const open = (chatId, mode = 'live', port = inbox) => {
        const ctx = context(port, mode);
        ctx.ui.chatId = chatId;
        return offersFiles(ctx);
    };
    assert.deepEqual([open('main'), open('chat-1'), open('chat-1', 'scripted'), open('main', 'live', null)], [true, false, true, false], 'the composer\'s + where a file can go, and the preview\'s own everywhere');
    assert.equal(takesFiles(context(inbox), 'main'), true);
    assert.equal(takesFiles(context(inbox), 'chat-1'), false);
    assert.equal(takesFiles(context(inbox, 'scripted'), 'main'), false);
    assert.equal(takesFiles(context(undefined), 'main'), false);
});
test('a picked file is given in the chat it was picked in, whatever chat is open by then', () => {
    const inbox = stand();
    const ctx = context(inbox);
    giveFile(ctx);
    const input = made.at(-1);
    assert.equal(input.tagName, 'input');
    assert.equal(input.attributes.get('type'), 'file');
    assert.equal(input.attributes.has('accept'), false, 'any kind of file');
    assert.equal(input.clicked, 1);
    ctx.ui.chatId = 'chat-1';
    input.fire('change');
    assert.deepEqual(inbox.asked, [], 'nothing picked, nothing given');
    input.files = [new File(['x'], 'notes.txt')];
    input.fire('change');
    assert.deepEqual(inbox.asked, [['give', 'main', 'notes.txt']]);
    const before = made.length;
    giveFile(context(undefined));
    assert.equal(made.length, before, 'no inbox, no picker');
});
test('each send\'s row says how it goes, shows the name inert, and offers only what can work', async () => {
    const name = '‮gpj.exe';
    const sends = [
        { id: 's1', chatId: 'main', name, bytes: 2048, state: 'waiting' },
        { id: 's2', chatId: 'main', name: 'b.txt', bytes: 1, state: 'sending', sent: 3, parts: 12 },
        { id: 's3', chatId: 'main', name: 'c.txt', bytes: 5, state: 'refused', detail: 'It didn’t arrive whole. Nothing was put.', retry: true },
        { id: 's4', chatId: 'main', name: 'd.txt', bytes: 5, state: 'refused', detail: 'This file is larger than Hatchling takes: 8 MB at most.', retry: false },
        { id: 's5', chatId: 'main', name: 'e.txt', bytes: 5, state: 'unknown', detail: 'Not confirmed.' },
        { id: 's6', chatId: 'main', name: 'f.txt', bytes: 5, state: 'sending', sent: 2, parts: 2 },
    ];
    const inbox = stand({ sends: () => sends });
    const ctx = context(inbox);
    const [waiting, sending, again, never, unknown, ending] = fileSendRows(ctx);
    assert.match(text(ending), /Waiting for the agent’s computer to take it/u, 'every part in, the put may wait its turn');
    assert.match(text(waiting), /Waiting its turn to go to Hatchling/u);
    assert.ok(find(waiting, element => element.tagName === 'bdi'), 'the name is isolated');
    assert.match(text(waiting), /⟨U\+202E⟩/u, 'a hidden character is shown');
    assert.equal(buttonNamed(waiting, 'Dismiss'), undefined, 'a send at work isn\'t dismissed');
    const count = find(sending, element => element.attributes.get('class') === 'given-file-count');
    assert.deepEqual([text(count), count.attributes.get('aria-hidden')], ['3 of 12 parts', 'true']);
    assert.match(text(sending), /Giving it to Hatchling/u);
    assert.match(text(again), /Not given\. It didn’t arrive whole/u);
    buttonNamed(again, 'Try again').fire('click');
    buttonNamed(again, 'Dismiss').fire('click');
    assert.equal(buttonNamed(never, 'Try again'), undefined, 'trying again can\'t work');
    buttonNamed(unknown, 'Check status').fire('click');
    assert.deepEqual(inbox.asked, [['retry', 's3'], ['dismiss', 's3'], ['refresh']]);
    assert.deepEqual(ctx.shown.settings, [], 'the list was shown before it was read again');
    await flush();
    assert.deepEqual(ctx.shown.settings, ['data']);
    assert.match(text(unknown), /Not confirmed\./u);
    assert.deepEqual(fileSendRows(context(undefined)), []);
});
test('beside the chat\'s notice: the owner\'s name, inert, and Remove behind a confirm', async () => {
    const files = {
        'file-1': { name: '‮txt.exe <b>', placedAs: '0003-txt.exe_b', bytes: 3, holdsSecret: true, removed: false },
        'file-2': { name: 'gone.txt', placedAs: '0004-gone.txt', bytes: 3, holdsSecret: false, removed: true },
        'file-3': { name: 'odd.txt', placedAs: '../odd', bytes: 3, holdsSecret: true, removed: false },
    };
    const inbox = stand({ given: id => files[id] });
    const ctx = context(inbox);
    const notice = givenFileDetail(ctx, { id: 'file-1', kind: 'notice', text: 'You gave Hatchling a file.' });
    assert.match(text(notice), /^Your name for it: /u);
    assert.ok(find(notice, element => element.tagName === 'bdi'));
    assert.match(text(notice), /⟨U\+202E⟩txt\.exe <b>/u, 'the name is text, its hidden character shown');
    buttonNamed(notice, 'Remove').fire('click');
    const [asked] = ctx.shown.confirms;
    assert.equal(asked.title, 'Remove 0003-txt.exe_b from the agent’s inbox?');
    assert.match(asked.description, /already read or copied/u);
    assert.deepEqual(asked.options, { label: 'Remove file', danger: true });
    assert.deepEqual(inbox.asked, [], 'nothing removed before the confirm');
    asked.action();
    await flush();
    assert.deepEqual(inbox.asked, [['remove', '0003-txt.exe_b']]);
    assert.deepEqual(ctx.shown.toasts, [['Removed from the agent’s inbox.', false]]);
    const warning = givenFileDetail(ctx, { id: 'file-1:secret', kind: 'error', text: 'This file holds a secret…' });
    assert.ok(buttonNamed(warning, 'Remove it now'));
    assert.doesNotMatch(text(warning), /Your name for it/u);
    assert.equal(text(givenFileDetail(ctx, { id: 'file-2', kind: 'notice', text: '' })), 'Removed from the agent’s inbox since.');
    // A name the inbox never gives: the owner's name shown, nothing offered to remove.
    const odd = givenFileDetail(ctx, { id: 'file-3', kind: 'notice', text: '' });
    assert.equal(text(odd), 'Your name for it: odd.txt');
    assert.equal(buttonNamed(odd, 'Remove'), undefined);
    assert.equal(givenFileDetail(ctx, { id: 'file-3:secret', kind: 'error', text: '' }), null);
    for (const message of [
        { id: 'file-9', kind: 'notice', text: '' },
        { id: 'file-9:secret', kind: 'error', text: '' },
        { id: 'file-1', kind: 'error', text: '' },
        { id: 'file-1', kind: 'text', role: 'agent', text: '' },
    ])
        assert.equal(givenFileDetail(ctx, message), null, JSON.stringify(message));
    assert.equal(givenFileDetail(context(undefined), { id: 'file-1', kind: 'notice', text: '' }), null);
});
test('Settings lists the inbox\'s files newest first, by the inbox\'s names, each removed behind a confirm', async () => {
    const section = (state, overrides = {}) => {
        const inbox = stand({ current: () => state, ...overrides });
        const ctx = context(inbox);
        let refreshed = 0;
        const [root] = liveInbox(ctx, () => { refreshed++; });
        return { root, inbox, ctx, refreshed: () => refreshed };
    };
    assert.deepEqual(liveInbox(context(undefined), () => undefined), []);
    assert.match(text(section({ kind: 'unread' }).root), /Reading the agent’s inbox/u);
    assert.match(text(section({ kind: 'failed', detail: 'The agent’s computer isn’t on, so its inbox can’t be read.' }).root), /isn’t on/u);
    assert.match(text(section({ kind: 'read', files: [] }).root), /Nothing yet/u);
    const listed = section({ kind: 'read', files: [{ name: '0001-a.txt', bytes: 3 }, { name: '../bad', bytes: 1 }, { name: '0002-b.txt', bytes: 2048 }] });
    const rows = [];
    find(listed.root, element => {
        if (element.attributes.get('class') === 'reference-setting-row given-file')
            rows.push(text(element));
        return false;
    });
    assert.deepEqual(rows, ['0002-b.txt2 KBRemove', '0001-a.txt3 bytesRemove'], 'newest first, a name the inbox doesn\'t give left out');
    buttonNamed(listed.root, 'Remove').fire('click');
    const [asked] = listed.ctx.shown.confirms;
    assert.equal(asked.title, 'Remove 0002-b.txt from the agent’s inbox?');
    asked.action();
    await flush();
    assert.deepEqual(listed.inbox.asked, [['remove', '0002-b.txt']]);
    assert.equal(listed.refreshed(), 1);
    assert.deepEqual(listed.ctx.shown.toasts, [['Removed from the agent’s inbox.', false]]);
    buttonNamed(listed.root, 'Refresh').fire('click');
    await flush();
    assert.deepEqual(listed.inbox.asked.at(-1), ['refresh']);
    assert.equal(listed.refreshed(), 2);
});
