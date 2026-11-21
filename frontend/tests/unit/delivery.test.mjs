import test from 'node:test';
import assert from 'node:assert/strict';
import { ChatDelivery } from '../../dist/app/features/chat/delivery.js';
import { chatGroups, channelStatus } from '../../dist/app/features/chat/connector-chats.js';
import { ScriptedChatSession } from '../../dist/app/adapters/chat-session.js';
import { parseRecovery } from '../../dist/app/adapters/chat-recovery.js';
import { createFixtures } from '../../dist/app/adapters/fixtures.js';
const tick = (ms = 0) => new Promise(resolve => setTimeout(resolve, ms));
function harness(options = {}) {
    const state = createFixtures();
    const listeners = new Set();
    const calls = [];
    let session = { connection: { kind: options.connection ?? 'ready', detail: '' }, histories: options.histories ?? {} };
    let saved = options.recovery ?? null;
    let changes = 0;
    let serial = 0;
    const accept = request => {
        const id = `accepted-${++serial}`;
        const command = request.command;
        state.messages.push({ id, chatId: command.chatId, time: '', source: null, reaction: null, role: 'user', ...(command.type === 'chat.send' ? { kind: 'text', text: command.text } : { kind: 'attachment', text: '', artifactId: command.artifactId }) });
        return { state: 'accepted', messageId: id };
    };
    const port = {
        getSnapshot: () => session,
        subscribe: fn => { listeners.add(fn); return () => listeners.delete(fn); },
        send: async (request) => { calls.push(request); return options.send ? options.send(request, accept, calls.length) : accept(request); },
        checkDelivery: async (id) => options.check ? options.check(id, accept, calls) : { state: 'unknown', detail: 'Still unknown.' },
        retryHistory: async (chatId) => {
            session.histories[chatId] = { phase: 'ready', hasCache: true, detail: '' };
            for (const listener of listeners)
                listener();
        },
        reconnect: async () => {
            session = { ...session, connection: { kind: 'ready', detail: '' } };
            for (const listener of listeners)
                listener();
        },
        loadRecovery: () => saved,
        saveRecovery: value => { saved = structuredClone(value); return !options.storageDenied; },
    };
    const adapter = { mode: 'scripted', getSnapshot: () => state, execute: async () => ({ ok: true, message: '' }), subscribe: () => () => { }, dispose() { }, chatSession: port };
    let streams = 0;
    const delivery = new ChatDelivery(adapter, () => changes++, options.timeout ?? 1000, options.streamed ? () => streams++ : undefined);
    const emit = (kind, histories) => {
        session = { connection: { kind, detail: '' }, histories: histories ?? session.histories };
        for (const listener of listeners)
            listener();
    };
    return { delivery, state, calls, accept, emit, port, adapter, saved: () => saved, changes: () => changes, streams: () => streams };
}
function cleanup(t, h) { t.after(() => h.delivery.dispose()); return h; }
test('offline messages are visible, never dispatched, then delivered exactly once in order', async (t) => {
    const h = cleanup(t, harness({ connection: 'offline' }));
    assert.equal(h.delivery.enqueue('main', 'first', []), true);
    assert.equal(h.delivery.enqueue('main', 'second', []), true);
    await tick();
    assert.equal(h.calls.length, 0);
    assert.equal(h.delivery.count('main'), 2);
    h.emit('ready');
    await tick();
    await tick();
    assert.deepEqual(h.calls.map(call => call.command.text), ['first', 'second']);
    assert.equal(h.delivery.visibleEntries('main').length, 0);
    h.emit('ready');
    await tick();
    assert.equal(h.calls.length, 2);
});
test('explicit rejection blocks later messages; retry retains the same request ID', async (t) => {
    const h = cleanup(t, harness({ send: (request, accept, count) => count === 1 ? { state: 'rejected', detail: 'Not accepted.' } : accept(request) }));
    h.delivery.enqueue('main', 'first', []);
    h.delivery.enqueue('main', 'second', []);
    await tick();
    assert.equal(h.calls.length, 1);
    assert.equal(h.delivery.entries('main')[0].phase, 'failed');
    const id = h.calls[0].id;
    h.delivery.retry(id);
    h.delivery.retry(id);
    await tick();
    await tick();
    assert.deepEqual(h.calls.map(call => call.command.text), ['first', 'first', 'second']);
    assert.equal(h.calls[1].id, id);
});
test('unknown deliveries cannot be retried, edited, removed, or automatically replayed', async (t) => {
    const h = cleanup(t, harness({ send: () => ({ state: 'unknown', detail: 'No receipt.' }) }));
    h.delivery.enqueue('main', 'one request', []);
    await tick();
    const item = h.delivery.entries('main')[0];
    assert.equal(item.phase, 'unconfirmed');
    h.delivery.retry(item.id);
    assert.equal(h.delivery.remove(item.id), null);
    h.emit('offline');
    h.emit('ready');
    await tick();
    assert.equal(h.calls.length, 1);
});
test('a thrown transport exception is uncertain, not a safe rejection', async (t) => {
    const h = cleanup(t, harness({ send: () => { throw new Error('Connection reset'); } }));
    h.delivery.enqueue('main', 'Keep this', []);
    await tick();
    assert.equal(h.delivery.entries('main')[0].phase, 'unconfirmed');
    assert.equal(h.delivery.hasUnsent, true);
});
test('a send deadline changes to uncertain and a late acceptance settles without a resend', async (t) => {
    let finish;
    const h = cleanup(t, harness({ timeout: 15, send: (request, accept) => new Promise(resolve => { finish = () => resolve(accept(request)); }) }));
    h.delivery.enqueue('main', 'Slow request', []);
    await tick(30);
    assert.equal(h.delivery.entries('main')[0].phase, 'unconfirmed');
    finish();
    await tick();
    assert.equal(h.delivery.entries('main')[0].phase, 'accepted');
    assert.equal(h.calls.length, 1);
});
test('checking a receipt reconciles the optimistic bubble to one authoritative message', async (t) => {
    const h = cleanup(t, harness({ send: () => ({ state: 'unknown', detail: 'No receipt.' }), check: (_id, accept, calls) => accept(calls[0]) }));
    h.delivery.enqueue('main', 'Only once', []);
    await tick();
    const id = h.delivery.entries('main')[0].id;
    await h.delivery.check(id);
    assert.equal(h.calls.length, 1);
    assert.equal(h.delivery.visibleEntries('main').length, 0);
    assert.equal(h.state.messages.filter(message => message.text === 'Only once').length, 1);
});
test('a late rejection cannot overwrite acceptance verified by status lookup', async (t) => {
    let rejectLate;
    const h = cleanup(t, harness({ timeout: 10, send: () => new Promise(resolve => { rejectLate = () => resolve({ state: 'rejected', detail: 'Stale response' }); }), check: (_id, accept, calls) => accept(calls[0]) }));
    h.delivery.enqueue('main', 'Race-safe', []);
    await tick(25);
    const id = h.delivery.entries('main')[0].id;
    await h.delivery.check(id);
    rejectLate();
    await tick();
    assert.equal(h.delivery.entries('main')[0].phase, 'accepted');
});
test('partial attachment failure retries the failed file without resending accepted text', async (t) => {
    let failed = false;
    const h = cleanup(t, harness({ send: (request, accept) => {
            if (request.command.type === 'chat.attach' && !failed) {
                failed = true;
                return { state: 'rejected', detail: 'File rejected.' };
            }
            return accept(request);
        } }));
    h.delivery.enqueue('main', 'Here are the notes', ['backlog']);
    await tick();
    await tick();
    const item = h.delivery.entries('main').find(entry => entry.phase === 'failed');
    assert.ok(item);
    h.delivery.retry(item.id);
    await tick();
    assert.deepEqual(h.calls.map(call => call.command.type), ['chat.send', 'chat.attach', 'chat.attach']);
});
test('a blocked conversation never blocks sending in another chat', async (t) => {
    const h = cleanup(t, harness({ send: (request, accept) => request.command.chatId === 'main' ? { state: 'unknown', detail: 'Unknown.' } : accept(request) }));
    h.delivery.enqueue('main', 'Unknown', []);
    h.delivery.enqueue('reference', 'Separate', []);
    await tick();
    assert.equal(h.delivery.entries('main')[0].phase, 'unconfirmed');
    assert.equal(h.delivery.entries('reference')[0].phase, 'accepted');
});
test('a queue recovered after reload waits for explicit review', async (t) => {
    const recovery = { version: 1, drafts: [], outbox: [{ id: 'saved-id', batchId: 'batch', command: { type: 'chat.send', chatId: 'main', text: 'Saved' }, phase: 'queued', detail: '' }] };
    const h = cleanup(t, harness({ recovery }));
    h.delivery.sync();
    await tick();
    assert.equal(h.calls.length, 0);
    assert.equal(h.delivery.restoredQueue, true);
    h.delivery.resumeRecovered();
    await tick();
    assert.equal(h.calls.length, 1);
});
test('a request interrupted by reload is uncertain and never auto-replayed', async (t) => {
    const recovery = { version: 1, drafts: [], outbox: [{ id: 'saved-id', batchId: 'batch', command: { type: 'chat.send', chatId: 'main', text: 'Saved' }, phase: 'sending', detail: '' }] };
    const h = cleanup(t, harness({ recovery }));
    h.delivery.sync();
    h.delivery.resumeRecovered();
    await tick();
    assert.equal(h.calls.length, 0);
    assert.equal(h.delivery.entries('main')[0].phase, 'unconfirmed');
});
test('draft persistence keeps each chat separate and records clearing', t => {
    const h = cleanup(t, harness());
    h.delivery.saveDraft('main', 'First', ['backlog']);
    h.delivery.saveDraft('reference', 'Second', []);
    assert.equal(h.saved().drafts.length, 2);
    assert.equal(h.delivery.hasDrafts, true);
    h.delivery.saveDraft('main', '', []);
    assert.deepEqual(h.saved().drafts.map(draft => draft.chatId), ['reference']);
});
test('storage denied is reported honestly while in-memory drafts remain usable', t => {
    const h = cleanup(t, harness({ storageDenied: true }));
    h.delivery.saveDraft('main', 'Keep in memory', []);
    assert.equal(h.delivery.recoveryAvailable, false);
    assert.equal(h.delivery.hasDrafts, true);
});
test('channel, archived, deleted, and incomplete-history conversations cannot send', t => {
    const h = cleanup(t, harness({ histories: { main: { phase: 'error', hasCache: false, detail: 'Missing.' } } }));
    assert.equal(h.delivery.enqueue('chat-whatsapp', 'Blocked', []), false);
    assert.equal(h.delivery.enqueue('missing-chat', 'Blocked', []), false);
    assert.equal(h.delivery.enqueue('main', 'Blocked', []), false);
    h.state.chats.find(chat => chat.id === 'reference').archived = true;
    assert.equal(h.delivery.enqueue('reference', 'Blocked', []), false);
});
test('history retry restores send availability without touching saved drafts', async (t) => {
    const h = cleanup(t, harness({ histories: { main: { phase: 'error', hasCache: false, detail: '' } } }));
    h.delivery.saveDraft('main', 'Untouched', []);
    await h.delivery.retryHistory('main');
    assert.equal(h.delivery.history('main').phase, 'ready');
    assert.equal(h.saved().drafts[0].text, 'Untouched');
});
test('a history loader has a finite deadline, including the initial load', async (t) => {
    const h = cleanup(t, harness({ timeout: 10, histories: { main: { phase: 'loading', hasCache: false, detail: '' } } }));
    await tick(25);
    assert.equal(h.delivery.history('main').phase, 'error');
    h.emit('ready', { main: { phase: 'ready', hasCache: true, detail: '' } });
    assert.equal(h.delivery.history('main').phase, 'ready');
});
test('empty and duplicate attachments cannot create accidental additional sends', async (t) => {
    const h = cleanup(t, harness());
    assert.equal(h.delivery.enqueue('main', '  ', []), false);
    h.delivery.enqueue('main', '', ['backlog', 'backlog']);
    await tick();
    assert.equal(h.calls.length, 1);
});
test('the outbox has a hard capacity and does not drop an oversized batch', t => {
    const h = cleanup(t, harness({ connection: 'offline' }));
    for (let i = 0; i < 29; i++)
        assert.equal(h.delivery.enqueue('main', `Message ${i}`, []), true);
    assert.equal(h.delivery.enqueue('main', 'text', ['backlog']), false);
    assert.equal(h.delivery.count('main'), 29);
});
test('browser offline overrides adapter readiness without changing dispatched uncertainty', async (t) => {
    const h = cleanup(t, harness());
    h.delivery.setBrowserOffline(true);
    h.delivery.enqueue('main', 'Queued', []);
    await tick();
    assert.equal(h.calls.length, 0);
    h.delivery.setBrowserOffline(false);
    await tick();
    assert.equal(h.calls.length, 1);
});
test('disposing cancels delivery deadlines and subscription callbacks', async (t) => {
    const h = cleanup(t, harness({ timeout: 10, send: () => new Promise(() => { }) }));
    h.delivery.enqueue('main', 'Wait', []);
    await tick();
    h.delivery.dispose();
    const changes = h.changes();
    h.emit('offline');
    await tick(25);
    assert.equal(h.changes(), changes);
});
test('what the session hears goes to the stream listener, which may wait to draw; what the owner does is told at once (#528)', async (t) => {
    const h = cleanup(t, harness({ streamed: true }));
    h.emit('ready');
    h.emit('ready');
    assert.equal(h.streams(), 2, 'each event is heard');
    assert.equal(h.changes(), 0, 'none of them asks to draw at once');
    assert.equal(h.delivery.enqueue('main', 'Hello', []), true);
    assert.ok(h.changes() > 0, 'the owner sending draws at once');
    assert.equal(h.streams(), 2);
    await tick();
    assert.equal(h.calls.length, 1, 'sent as before');
    // With no stream listener, what the session hears is told as a change.
    const plain = cleanup(t, harness());
    plain.emit('ready');
    assert.equal(plain.changes(), 1);
    assert.equal(plain.streams(), 0);
});
test('connector chats cannot appear inside side chats, including archived views', () => {
    const state = createFixtures();
    const groups = chatGroups(state.chats, false);
    assert.ok(groups.side.every(chat => chat.channelId === null));
    assert.deepEqual(groups.connectors.map(chat => chat.title), ['WhatsApp', 'Messenger']);
    state.chats.find(chat => chat.channelId === 'whatsapp').archived = true;
    assert.equal(chatGroups(state.chats, false).connectors.length, 1);
    assert.equal(chatGroups(state.chats, true).connectors[0].channelId, 'whatsapp');
});
test('connector status distinguishes linked-but-paused from listening', () => {
    const state = createFixtures();
    assert.equal(channelStatus(state.channels.find(channel => channel.id === 'whatsapp')).label, 'Listening');
    assert.equal(channelStatus(state.channels.find(channel => channel.id === 'messenger')).label, 'Updates paused');
    assert.equal(channelStatus(undefined).active, false);
});
test('recovery parser rejects arbitrary commands and duplicate IDs', () => {
    const item = { id: 'same', batchId: 'batch', command: { type: 'chat.send', chatId: 'main', text: 'Hello' }, phase: 'sending', detail: '' };
    const parsed = parseRecovery(JSON.stringify({ version: 1, drafts: [], outbox: [item, item, { ...item, id: 'bad', command: { type: 'purchase.submit', id: 'purchase' } }] }));
    assert.equal(parsed.outbox.length, 1);
    assert.equal(parsed.outbox[0].phase, 'unconfirmed');
    assert.equal(parseRecovery('{broken'), null);
    assert.equal(parseRecovery(JSON.stringify({ version: 2, drafts: [], outbox: [] })), null);
});
test('legacy live adapters cannot turn a generic failure into a safe retry', async (t) => {
    const state = createFixtures();
    const adapter = { mode: 'live', getSnapshot: () => state, execute: async () => ({ ok: false, message: 'Timeout' }), subscribe: () => () => { }, dispose() { } };
    const delivery = new ChatDelivery(adapter, () => { });
    t.after(() => delivery.dispose());
    delivery.enqueue('main', 'Live request', []);
    await tick();
    assert.equal(delivery.entries('main')[0].phase, 'unconfirmed');
});
test('editing failed text holds the remaining files instead of sending them without the text', async (t) => {
    const h = cleanup(t, harness({ send: () => ({ state: 'rejected', detail: 'Not accepted.' }) }));
    h.delivery.enqueue('main', 'Edit this with its file', ['backlog']);
    await tick();
    const parts = h.delivery.edit(h.delivery.entries('main')[0].id);
    assert.deepEqual(parts.map(part => part.command.type), ['chat.send', 'chat.attach']);
    await tick();
    assert.equal(h.calls.length, 1);
    assert.equal(h.delivery.count('main'), 0);
});
test('editing a failed attachment never copies already accepted text back into the draft', async (t) => {
    const h = cleanup(t, harness({ send: (request, accept) => request.command.type === 'chat.attach' ? { state: 'rejected', detail: 'Not accepted.' } : accept(request) }));
    h.delivery.enqueue('main', 'Already accepted', ['backlog']);
    await tick();
    await tick();
    const parts = h.delivery.edit(h.delivery.entries('main').find(part => part.phase === 'failed').id);
    assert.deepEqual(parts.map(part => part.command.type), ['chat.attach']);
    assert.equal(h.state.messages.filter(message => message.text === 'Already accepted').length, 1);
});
test('explicit dismissal releases later messages without pretending the uncertain one was cancelled or sent', async (t) => {
    const h = cleanup(t, harness({ send: (request, accept, count) => count === 1 ? { state: 'unknown', detail: 'No receipt.' } : accept(request) }));
    h.delivery.enqueue('main', 'Uncertain request', []);
    h.delivery.enqueue('main', 'Later message', []);
    await tick();
    assert.equal(h.calls.length, 1);
    const id = h.delivery.entries('main')[0].id;
    assert.equal(h.delivery.dismissUnconfirmed(id), true);
    await tick();
    assert.deepEqual(h.calls.map(request => request.command.text), ['Uncertain request', 'Later message']);
    assert.equal(h.state.messages.filter(message => message.text === 'Uncertain request').length, 0);
    assert.equal(h.delivery.receipt(id), false);
    assert.equal(h.delivery.dismissUnconfirmed(id), false);
});
test('recovery preserves a large draft and all staged attachment references without silent truncation', () => {
    const draft = { chatId: 'main', text: 'x'.repeat(150000), attachments: Array.from({ length: 40 }, (_, i) => `file-${i}`) };
    const restored = parseRecovery(JSON.stringify({ version: 1, drafts: [draft], outbox: [] }));
    assert.deepEqual(restored.drafts[0], draft);
});
test('the adapter never labels a recovery snapshot saved if its own reader cannot restore it', t => {
    const data = new Map();
    const storage = { getItem: key => data.get(key) ?? null, setItem: (key, value) => data.set(key, value), removeItem: key => data.delete(key) };
    const session = new ScriptedChatSession(async () => ({ ok: true, message: '' }), createFixtures, storage);
    t.after(() => session.dispose());
    const draft = { chatId: 'main', text: 'A recoverable draft', attachments: [] };
    assert.equal(session.saveRecovery({ version: 1, drafts: [draft], outbox: [] }), true);
    assert.equal(session.loadRecovery().drafts[0].text, draft.text);
    assert.equal(session.saveRecovery({ version: 1, drafts: [{ ...draft, text: 'x'.repeat(1000001) }], outbox: [] }), false);
    assert.equal(session.loadRecovery(), null);
    assert.equal(session.saveRecovery({ version: 1, drafts: Array.from({ length: 201 }, (_, i) => ({ ...draft, chatId: `chat-${i}` })), outbox: [] }), false);
});
