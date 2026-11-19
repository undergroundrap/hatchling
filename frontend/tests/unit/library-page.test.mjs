// The files and folders the agent gave, on the Library page in the demo's
// own cards (WO-043, #523): grouped by conversation and reply, each a card
// with its type, whole name, Download and Remove; opened, what Hatchling
// knows of it; chosen in the page's Select mode and downloaded as one .zip;
// a .zip's progress counted in place. And the Library's few words in
// Settings, with the way to it. Over a minimal DOM that records listeners.
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
    dataset = {};
    constructor(tag) { super(); this.tagName = tag; }
    setAttribute(name, value) { this.attributes.set(name, value); }
    getAttribute(name) { return this.attributes.get(name) ?? null; }
    addEventListener(name, handler) { this.listeners.set(name, handler); }
    querySelector() { return null; }
    focus() { }
    fire(name) { this.listeners.get(name)?.({ preventDefault() { }, stopPropagation() { } }); }
    set textContent(value) { this.childNodes = [new FakeText(value)]; }
}
globalThis.Node = FakeNode;
globalThis.requestAnimationFrame = () => 0;
const focused = [];
const byId = new Map();
globalThis.document = { createElement: tag => new FakeElement(tag), createElementNS: (namespace, tag) => new FakeElement(tag), createTextNode: data => new FakeText(data), querySelectorAll: () => [], activeElement: null, body: null, getElementById: id => byId.get(id) ?? null };
const { liveLibrary, liveLibraryActions, libraryOverview, librarySummary, hasGivenItems, endChoosing } = await import('../../dist/app/features/library/live-library.js');
const text = (node) => node instanceof FakeText ? node.data : node.childNodes.map(text).join('');
const findAll = (node, predicate, out = []) => {
    if (node instanceof FakeElement && predicate(node))
        out.push(node);
    for (const child of node.childNodes)
        if (child instanceof FakeElement)
            findAll(child, predicate, out);
    return out;
};
const page = (nodes) => { const root = new FakeElement('section'); root.append(...nodes); return root; };
/** A button by what it says: a menu's item says its label beside its icon. */
const buttonNamed = (node, label) => findAll(node, element => element.tagName === 'button' && text(element) === label)[0];
const flush = () => new Promise(resolve => setImmediate(resolve));
const FILE = { id: 'f1', name: 'report.pdf', bytes: 2048, sha256: 'ab', keptAt: '2026-11-17T09:00:00Z', reply: 'run-1', conversation: 1 };
const NONE_LEFT = { links: 0, mounts: 0, others: 0 };
const FOLDER = { id: 'd'.repeat(64), name: 'site', files: 3, bytes: 4096, sha256: 'cd', leftOut: NONE_LEFT, keptAt: '2026-11-18T00:00:00Z', reply: 'run-2', conversation: 1 };
const STANDING = { files: 1, fileLimit: 64, bytes: 2048, byteLimit: 268435456, given: 1, givenLimit: 16, off: false, filesInAll: 1, filesInAllLimit: 4096, read: 2048, readLimit: 134217728, conversation: 1 };
const read = (files = [FILE], folders = [], unnamed = [], standing = {}) => ({ kind: 'read', files, folders, unnamed, standing: { ...STANDING, ...standing } });
/** A stand-in Library that records what it was asked. */
function stand(overrides = {}) {
    const asked = [];
    return {
        asked,
        current: () => read(),
        refresh: () => { asked.push(['refresh']); return Promise.resolve(); },
        offered: () => undefined,
        offeredFolder: () => undefined,
        givenIn: () => [],
        givenElsewhere: () => [],
        downloading: () => false,
        downloadProgress: () => null,
        download: id => { asked.push(['download', id]); return Promise.resolve({ ok: true, message: 'Gave your browser report.pdf.' }); },
        downloadMany: (ids, name) => { asked.push(['download many', [...ids], name]); return Promise.resolve({ ok: true, message: `Gave your browser ${name}.` }); },
        downloadAll: () => { asked.push(['download all']); return Promise.resolve({ ok: true, message: 'Gave your browser hatchling-library.zip.' }); },
        chosen: () => new Set(),
        choose: (id, on) => { asked.push(['choose', id, on]); },
        clearChosen: () => { asked.push(['clear']); },
        remove: id => { asked.push(['remove', id]); return Promise.resolve({ ok: true, message: 'Removed from Hatchling’s Library.' }); },
        ...overrides,
    };
}
/** An app context with `agentFiles`, recording what it was asked to show and do. */
function context(agentFiles, ui = {}) {
    const shown = { toasts: [], confirms: [], dialogs: [], went: [], drawn: 0, closed: 0 };
    return {
        shown,
        state: { mode: 'live' },
        agentFiles,
        ui: { libraryLayout: 'grid', selectionMode: false, libraryFilter: 'documents', ...ui },
        render: () => { shown.drawn++; },
        navigate: (route) => shown.went.push(route),
        toast: (message, error) => shown.toasts.push([message, error === true]),
        dialogs: {
            confirm: (title, description, action, options) => shown.confirms.push({ title, description, action, options }),
            open: (title, content) => { const ref = { title, content, closed: false, close: () => { ref.closed = true; } }; shown.dialogs.push(ref); return ref; },
            closeAll: () => { shown.closed++; },
        },
    };
}
/** The open buttons of the cards, by the card's name. */
const cardOpen = (root, name) => findAll(root, element => element.tagName === 'button' && element.attributes.get('class') === 'artifact-meta' && text(element).startsWith(name))[0];
test('each item is a card in the demo\'s style: its whole name, its type, and what opening it can do', () => {
    const files = [FILE, { ...FILE, id: 'f2', name: 'setup.exe' }, { ...FILE, id: 'f3', name: 'notes' }];
    const folder = { ...FOLDER, reply: 'run-1', leftOut: { links: 1, mounts: 0, others: 0 } };
    const root = page(liveLibrary(context(stand({ current: () => read(files, [folder]) })), stand({ current: () => read(files, [folder]) })));
    const words = text(root);
    assert.match(words, /1 of 64 items \(1 of 4096 files\), 2 KB of 256 MB\. The agent has given 1 of the 16 files or folders it can in this conversation\./u);
    const cards = findAll(root, element => element.tagName === 'article');
    assert.equal(cards.length, 4);
    for (const card of cards)
        assert.match(card.attributes.get('class'), /^artifact-card given/u);
    const named = (name) => cards.find(card => text(cardOpen(card, name) ?? new FakeElement('none')) !== '');
    assert.match(text(named('site')), /^3 filessiteFolder · 3 files · 4 KB · kept 2026-11-18Left out when it was given: 1 link\./u);
    assert.match(words, /PDFreport\.pdfPDF · 2 KB · kept 2026-11-17/u);
    // A program says so on its card; a name without an extension is a file.
    assert.match(words, /setup\.exeEXE · 2 KB · kept 2026-11-17Program: runs when opened/u);
    assert.match(words, /FILEnotesFILE · 2 KB/u);
    assert.doesNotMatch(text(cardOpen(root, 'report.pdf')), /runs when opened/u);
    // Each card's actions are the demo's menu: Download (a .zip for a folder) and Remove.
    assert.ok(buttonNamed(named('site'), 'Download .zip'));
    assert.ok(buttonNamed(named('report.pdf'), 'Download'));
    assert.ok(buttonNamed(named('report.pdf'), 'Remove'));
    // A name the broker never gives is no card.
    assert.doesNotMatch(text(page(liveLibrary(context(stand()), stand({ current: () => read([{ ...FILE, name: '../evil.exe' }]) })))), /evil/u);
});
test('cards are grouped by conversation, the open one first, and by reply, oldest first, apart from what came before replies', () => {
    const at = (hour) => `2026-11-18T0${hour}:00:00Z`;
    const files = [
        { ...FILE, id: 'a1', name: 'old.txt', conversation: 1, reply: 'run-x', keptAt: at(1) },
        { ...FILE, id: 'b2', name: 'second.txt', conversation: 2, reply: 'run-b', keptAt: at(4) },
        { ...FILE, id: 'b1', name: 'first.txt', conversation: 2, reply: 'run-a', keptAt: at(2) },
        { ...FILE, id: 'b0', name: 'before.txt', conversation: 2, reply: null, keptAt: at(0) },
    ];
    const folders = [{ ...FOLDER, files: 2, bytes: 9, leftOut: { links: 0, mounts: 0, others: 3 }, keptAt: at(3), reply: 'run-a', conversation: 2 }];
    const port = stand({ current: () => read(files, folders, [], { conversation: 2 }) });
    const words = text(page(liveLibrary(context(port), port)));
    const order = ['This conversation', '4 items', 'Given in one reply, 2026-11-18 02:00 UTC', 'first.txt', 'site', 'Left out when it was given: 3 special files.', 'Given in one reply, 2026-11-18 04:00 UTC', 'second.txt', 'Earlier, from before items were kept by reply', 'before.txt', 'Conversation 1', '1 item', 'old.txt'];
    let from = 0;
    for (const words_ of order) {
        const found = words.indexOf(words_, from);
        assert.ok(found >= from, `${words_} out of order`);
        from = found + words_.length;
    }
    // The page's layout: a grid, or a list.
    const listed = page(liveLibrary(context(port, { libraryLayout: 'list' }), port));
    assert.equal(findAll(listed, element => /artifact-grid/u.test(element.attributes.get('class') ?? '')).length, 0);
    assert.ok(findAll(listed, element => element.attributes.get('class') === 'artifact-list given-cards').length > 0);
});
test('opening a card shows what Hatchling knows of it, never what it holds, with Download and Remove', async () => {
    const port = stand({ current: () => read([{ ...FILE, name: 'run.exe' }], [FOLDER]) });
    const ctx = context(port);
    const root = page(liveLibrary(ctx, port));
    cardOpen(root, 'run.exe').fire('click');
    const [file] = ctx.shown.dialogs;
    assert.equal(file.title, 'A file the agent gave you');
    // Each card's buttons and menu are named for it, so focus finds its own again.
    const card = findAll(root, element => element.tagName === 'article' && element.attributes.get('data-given') === 'f1')[0];
    assert.equal(findAll(card, element => element.attributes.get('class')?.startsWith('artifact-preview'))[0].attributes.get('aria-label'), 'Open run.exe');
    assert.deepEqual(findAll(card, element => element.tagName === 'button' && element.attributes.has('id')).map(element => element.attributes.get('id')), ['given-preview-f1', 'given-open-f1']);
    assert.equal(findAll(card, element => element.tagName === 'summary')[0].attributes.get('aria-label'), 'Actions for run.exe');
    const said = text(file.content);
    assert.match(said, /^run\.exe2 KB, kept 2026-11-17 09:00 UTC\.A program: it runs when opened\. Open it only if you trust what the agent made\.SHA-256 ab/u);
    buttonNamed(file.content, 'Download').fire('click');
    await flush();
    assert.deepEqual(port.asked, [['download', 'f1']]);
    buttonNamed(file.content, 'Remove').fire('click');
    const [asked] = ctx.shown.confirms;
    assert.equal(asked.title, 'Remove run.exe from Hatchling’s Library?');
    assert.deepEqual(asked.options, { label: 'Remove', danger: true });
    asked.action();
    await flush();
    assert.deepEqual(port.asked.at(-1), ['remove', 'f1']);
    assert.equal(file.closed, true, 'the dialog closes once it is removed');
    assert.deepEqual(ctx.shown.toasts.at(-1), ['Removed from Hatchling’s Library.', false]);
    // A folder says its files, what was left out, and that its hash is its list's.
    cardOpen(root, 'site').fire('click');
    const folder = ctx.shown.dialogs[1];
    assert.equal(folder.title, 'A folder the agent gave you');
    assert.match(text(folder.content), /3 files, 4 KB, kept 2026-11-18 00:00 UTC\. It downloads as one \.zip, its folders as they were\.SHA-256 of its list cd/u);
    // While it downloads, its card says so, and Download asks nothing more.
    const busy = stand({ current: () => read(), downloading: () => true });
    const busyCtx = context(busy);
    const busyPage = page(liveLibrary(busyCtx, busy));
    assert.match(text(cardOpen(busyPage, 'report.pdf')), /Downloading…$/u);
    cardOpen(busyPage, 'report.pdf').fire('click');
    buttonNamed(busyCtx.shown.dialogs[0].content, 'Download').fire('click');
    await flush();
    assert.deepEqual(busy.asked, []);
    assert.deepEqual(busyCtx.shown.toasts, [['It is already downloading.', true]]);
    // An item gone meanwhile closes what says it.
    let kept = [FILE];
    const going = stand({ current: () => read(kept) });
    const goingCtx = context(going);
    cardOpen(page(liveLibrary(goingCtx, going)), 'report.pdf').fire('click');
    kept = [];
    buttonNamed(goingCtx.shown.dialogs[0].content, 'Download').fire('click');
    await flush();
    assert.deepEqual([going.asked, goingCtx.shown.dialogs[0].closed, goingCtx.shown.toasts], [[], true, [['It isn’t in Hatchling’s Library any more.', true]]]);
});
test('a removal that fails says why, and stops', async () => {
    const port = stand({ current: () => read([FILE, { ...FILE, id: 'f2', name: 'b.txt' }]), chosen: () => new Set(['f1', 'f2']), remove: id => { port.asked.push(['remove', id]); return Promise.resolve({ ok: false, message: 'Hatchling’s Library couldn’t be read.' }); } });
    const ctx = context(port, { selectionMode: true });
    buttonNamed(page(liveLibrary(ctx, port)), 'Remove').fire('click');
    ctx.shown.confirms[0].action();
    await flush();
    assert.deepEqual(port.asked, [['remove', 'f1']]);
    assert.deepEqual(ctx.shown.toasts, [['Hatchling’s Library couldn’t be read.', true]]);
});
test('Select chooses cards, the bar downloads the chosen as one .zip in the page\'s order, removes them, or is done', async () => {
    const files = [{ ...FILE, id: 'k1', name: 'a.txt', keptAt: '2026-11-18T01:00:00Z' }, { ...FILE, id: 'k2', name: 'b.txt', reply: 'run-2', keptAt: '2026-11-18T02:00:00Z' }];
    // The header offers Select and Download all, for more than one item.
    const idle = stand({ current: () => read(files) });
    const idleCtx = context(idle);
    const actions = page(liveLibraryActions(idleCtx, idle));
    assert.deepEqual(findAll(actions, element => element.tagName === 'button').map(text), ['Select', 'Download all as one .zip']);
    buttonNamed(actions, 'Download all as one .zip').fire('click');
    await flush();
    assert.deepEqual(idle.asked, [['download all']]);
    assert.deepEqual(idleCtx.shown.toasts, [['Gave your browser hatchling-library.zip.', false]]);
    const doneButton = { focus: () => focused.push('given-select-done') };
    byId.set('given-select-done', doneButton);
    buttonNamed(actions, 'Select').fire('click');
    assert.deepEqual([idleCtx.ui.selectionMode, idleCtx.shown.drawn, focused.at(-1)], [true, 1, 'given-select-done']);
    // While choosing, a card's click chooses it, and its press is said.
    const chosen = new Set(['k2', 'k1', 'gone']);
    const port = stand({ current: () => read(files), chosen: () => chosen });
    const ctx = context(port, { selectionMode: true });
    const root = page(liveLibrary(ctx, port));
    assert.deepEqual(liveLibraryActions(ctx, port), [], 'the header gives way to the bar');
    assert.equal(cardOpen(root, 'a.txt').attributes.get('aria-pressed'), 'true');
    assert.equal(findAll(root, element => element.attributes.get('id') === 'given-preview-k1')[0].attributes.get('aria-label'), 'Choose a.txt');
    cardOpen(root, 'a.txt').fire('click');
    assert.deepEqual(port.asked.at(-1), ['choose', 'k1', false]);
    assert.deepEqual(ctx.shown.dialogs, [], 'choosing opens nothing');
    assert.match(text(root), /2 chosen/u);
    buttonNamed(root, 'Download as one .zip').fire('click');
    await flush();
    assert.deepEqual(port.asked.at(-1), ['download many', ['k1', 'k2'], 'hatchling-chosen.zip']);
    buttonNamed(root, 'Remove').fire('click');
    assert.equal(ctx.shown.confirms[0].title, 'Remove these 2 items from Hatchling’s Library?');
    ctx.shown.confirms[0].action();
    await flush();
    assert.deepEqual(port.asked.slice(-3), [['remove', 'k1'], ['remove', 'k2'], ['clear']]);
    assert.deepEqual(ctx.shown.toasts.at(-1), ['Removed 2 items from Hatchling’s Library.', false]);
    byId.set('given-select', { focus: () => focused.push('given-select') });
    const drawnBefore = ctx.shown.drawn;
    buttonNamed(root, 'Done').fire('click');
    // Letting the chosen go draws the page; Done draws nothing more.
    assert.deepEqual([ctx.ui.selectionMode, port.asked.at(-1), ctx.shown.drawn - drawnBefore, focused.at(-1)], [false, ['clear'], 0, 'given-select']);
    // One chosen downloads as itself; none chosen offers nothing to do.
    const alone = stand({ current: () => read(files), chosen: () => new Set(['k2']) });
    const single = page(liveLibrary(context(alone, { selectionMode: true }), alone));
    assert.match(text(single), /1 chosen/u);
    buttonNamed(single, 'Download').fire('click');
    await flush();
    assert.deepEqual(alone.asked, [['download', 'k2']]);
    const none = page(liveLibrary(context(stand({ current: () => read(files) }), { selectionMode: true }), stand({ current: () => read(files) })));
    assert.equal(buttonNamed(none, 'Download as one .zip').attributes.get('disabled'), '');
    // One item: Select, but no Download all; none: nothing.
    assert.deepEqual(findAll(page(liveLibraryActions(context(stand()), stand())), element => element.tagName === 'button').map(text), ['Select']);
    assert.deepEqual(liveLibraryActions(context(stand()), stand({ current: () => read([]) })), []);
    assert.equal(hasGivenItems(stand({ current: () => read([]) })), false);
    assert.equal(hasGivenItems(stand({ current: () => ({ kind: 'unread' }) })), false);
});
test('a reply\'s items download together, by a name made from its run, only where it gave more than one and nothing is being chosen', async () => {
    const files = [
        { ...FILE, id: 'r1', name: 'a.txt', keptAt: '2026-11-18T01:00:00Z' },
        { ...FILE, id: 'r2', name: 'b.txt', keptAt: '2026-11-18T02:00:00Z' },
        { ...FILE, id: 'r3', name: 'c.txt', reply: 'run-2', keptAt: '2026-11-18T03:00:00Z' },
        { ...FILE, id: 'r4', name: 'd.txt', reply: null, keptAt: '2026-11-18T00:00:00Z' },
        { ...FILE, id: 'r5', name: 'e.txt', reply: null, keptAt: '2026-11-18T00:30:00Z' },
    ];
    const port = stand({ current: () => read(files) });
    const ctx = context(port);
    const root = page(liveLibrary(ctx, port));
    const together = findAll(root, element => element.tagName === 'button' && text(element).startsWith('Download these'));
    assert.deepEqual(together.map(text), ['Download these 2 as one .zip']);
    assert.equal(together[0].attributes.get('id'), 'given-reply-1-run-1');
    together[0].fire('click');
    await flush();
    assert.deepEqual(port.asked, [['download many', ['r1', 'r2'], 'hatchling-run-1.zip']]);
    assert.deepEqual(ctx.shown.toasts, [['Gave your browser hatchling-run-1.zip.', false]]);
    const choosing = page(liveLibrary(context(port, { selectionMode: true }), port));
    assert.equal(findAll(choosing, element => element.tagName === 'button' && text(element).startsWith('Download these')).length, 0);
});
test('while a .zip is made its progress is said in place, and nothing offers a second download', () => {
    const files = [FILE, { ...FILE, id: 'f2', name: 'b.txt' }];
    const port = stand({ current: () => read(files), downloadProgress: () => ({ done: 3, total: 9 }) });
    const ctx = context(port, { selectionMode: true });
    const root = page(liveLibrary(ctx, port));
    const [status] = findAll(root, element => element.attributes.get('role') === 'status');
    assert.equal(text(status), 'Downloading as one .zip… 3 of 9 files');
    // The count is hidden from a reader, so it isn't read out at each file.
    assert.equal(findAll(status, element => element.attributes.get('aria-hidden') === 'true').length, 1);
    assert.doesNotMatch(text(root), /chosen/u);
    assert.deepEqual(liveLibraryActions(context(port), port), []);
});
test('the page says the Library as read: not yet, why not with a retry, nothing yet with the way to the chat, and what it can\'t name', async () => {
    assert.match(text(page(liveLibrary(context(stand()), stand({ current: () => ({ kind: 'unread' }) })))), /^Reading Hatchling’s Library…$/u);
    const failing = stand({ current: () => ({ kind: 'failed', detail: 'Hatchling couldn’t read its Library.' }) });
    const failed = page(liveLibrary(context(failing), failing));
    assert.match(text(failed), /^Hatchling couldn’t read its Library\. Try again$/u);
    buttonNamed(failed, 'Try again').fire('click');
    assert.deepEqual(failing.asked, [['refresh']]);
    const empty = stand({ current: () => read([]) });
    const emptyCtx = context(empty);
    const nothing = page(liveLibrary(emptyCtx, empty));
    assert.match(text(nothing), /Nothing yet\.Files and folders the agent gives you in the chat show here\./u);
    buttonNamed(nothing, 'Go to the chat').fire('click');
    assert.deepEqual(emptyCtx.shown.went, ['chat']);
    buttonNamed(nothing, 'Refresh').fire('click');
    assert.deepEqual(empty.asked, [['refresh']]);
    // An item it can't name: its size, and Remove only when its id is one Hatchling gives.
    const unnamed = { id: 'c'.repeat(64), bytes: 10, removable: true };
    const stuck = { id: 'not-an-id', bytes: 5, removable: false };
    const odd = stand({ current: () => read([], [], [unnamed, stuck]) });
    const oddCtx = context(odd);
    const items = page(liveLibrary(oddCtx, odd));
    assert.doesNotMatch(text(items), /Nothing yet/u);
    const cards = findAll(items, element => element.tagName === 'article');
    assert.deepEqual(cards.map(card => findAll(card, element => element.tagName === 'button' && element.attributes.get('class')?.startsWith('artifact-')).length), [0, 0]);
    assert.deepEqual(cards.map(card => findAll(card, element => element.tagName === 'details').length), [1, 0]);
    assert.equal(findAll(cards[0], element => element.tagName === 'summary')[0].attributes.get('aria-label'), 'Actions for the item Hatchling can’t name, of 10 bytes');
    assert.match(text(cards[0]), /An item Hatchling can’t name10 bytes\. It isn’t one Hatchling gives/u);
    assert.match(text(cards[1]), /5 bytes\. .*Its id isn’t one Hatchling gives either, so it can’t be removed from here/u);
    assert.equal(buttonNamed(cards[1], 'Remove'), undefined);
    buttonNamed(cards[0], 'Remove').fire('click');
    assert.equal(oddCtx.shown.confirms[0].title, 'Remove this item from Hatchling’s Library?');
    oddCtx.shown.confirms[0].action();
    await flush();
    assert.deepEqual(odd.asked, [['remove', unnamed.id]]);
});
test('where the copies come from is said, and choosing ends where its cards aren\'t shown', async () => {
    assert.match(text(page(liveLibrary(context(stand()), stand()))), /These are the copies Hatchling made from the agent’s computer and checked for the secrets it keeps\. A download is always this copy, each file checked against its SHA-256 and saved, never opened/u);
    for (const state of [{ kind: 'unread' }, { kind: 'failed', detail: 'No.' }, read([])]) {
        const port = stand({ current: () => state });
        const ctx = context(port, { selectionMode: true });
        liveLibrary(ctx, port);
        assert.equal(ctx.ui.selectionMode, false);
        await flush();
        assert.deepEqual(port.asked, [['clear']], `${state.kind}: the chosen let go after the draw`);
    }
    const port = stand();
    const ctx = context(port);
    endChoosing(ctx, port);
    await flush();
    assert.deepEqual(port.asked, [], 'nothing to end');
});
test('the page says when the Library is full, giving is off, the conversation has given or read all it can', () => {
    const said = (standing) => text(page(liveLibrary(context(stand()), stand({ current: () => read([FILE], [], [], standing) }))));
    assert.match(said({ off: true }), /Giving is off for this conversation: something held a secret Hatchling keeps\./u);
    assert.match(said({ given: 16 }), /The agent has given all 16 files or folders it can in this conversation\./u);
    assert.match(said({ read: 134217728 }), /The agent has read all 128 MB it can out of its computer in this conversation\./u);
    assert.match(said({ files: 64 }), /The Library is full: remove something before the agent can give more\./u);
    assert.match(said({ bytes: 268435456 }), /The Library is full/u);
    assert.match(said({ filesInAll: 4096 }), /The Library is full/u);
});
test('Settings says the Library in a few words, kept up to date, and leads to it', () => {
    assert.equal(librarySummary(stand()), '1 of 64 items, 2 KB of 256 MB.');
    // Its items, not what this conversation gave.
    assert.equal(librarySummary(stand({ current: () => read([FILE], [], [], { files: 5, given: 3 }) })), '5 of 64 items, 2 KB of 256 MB.');
    assert.equal(librarySummary(stand({ current: () => ({ kind: 'unread' }) })), 'Reading Hatchling’s Library…');
    assert.equal(librarySummary(stand({ current: () => ({ kind: 'failed', detail: 'Hatchling couldn’t read its Library.' }) })), 'Hatchling couldn’t read its Library.');
    const ctx = context(stand());
    const [section] = libraryOverview(ctx);
    assert.match(text(section), /^Files and folders the agent gave you1 of 64 items, 2 KB of 256 MB\.Open the Library$/u);
    buttonNamed(section, 'Open the Library').fire('click');
    assert.deepEqual([ctx.shown.closed, ctx.ui.libraryFilter, ctx.shown.went], [1, 'all', ['library']]);
    assert.deepEqual(libraryOverview(context(undefined)), []);
});
