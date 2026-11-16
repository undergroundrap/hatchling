// Files the owner gives the agent (WO-041), end to end over a fake transport:
// sent in parts, in order, one file at a time, each step answered before the
// next; how each send ends, before its end and after it; the stream's word
// settling one not confirmed; and the inbox's list and removal.
import test from 'node:test';
import assert from 'node:assert/strict';
import { LiveAdapter } from '../../dist/app/adapters/live/adapter.js';
import { LiveClient } from '../../dist/app/adapters/live/client.js';
import { FILE_IN_BYTES, LiveInbox, base64 } from '../../dist/app/adapters/live/inbox.js';
import { HOLDS_SECRET } from '../../dist/app/adapters/live/chat.js';
import { SAMPLE_WELCOME, sampleEvent } from '../../dist/app/adapters/live/fixtures.js';
import { isInboxName, sizeWords } from '../../dist/app/session/inbox.js';
class FakeTransport {
    sent = [];
    handlers = [];
    respond = () => undefined;
    connect(handlers) { this.handlers.push(handlers); }
    send(text) {
        const frame = JSON.parse(text);
        this.sent.push(frame);
        if (frame.type !== 'action')
            return;
        const result = this.respond(frame.action, frame.request_id);
        if (result !== undefined)
            queueMicrotask(() => this.deliver({ type: 'action_result', request_id: frame.request_id, result }));
    }
    close() { }
    deliver(frame) { this.handlers.at(-1).received(JSON.stringify(frame)); }
    open() { this.handlers.at(-1).opened(); }
    drop() { this.handlers.at(-1).closed('The network went away.'); }
    actions(name) { return this.sent.filter(frame => frame.type === 'action' && frame.action.action === name).map(frame => frame.action); }
}
const flush = () => new Promise(resolve => setImmediate(resolve));
const settle = async () => {
    for (let i = 0; i < 60; i++)
        await flush();
};
let seq = 500;
const fileIn = (fields) => sampleEvent(++seq, 30, { kind: 'file_in', chat_id: 'local', message_id: 'file-1', name: 'notes.txt', placed_as: '0001-notes.txt', bytes: 3, holds_secret: false, ...fields });
const placed = (uploadId, fields = {}) => ({ result: 'file_in_placed', upload_id: uploadId, file: { chat_id: 'local', message_id: uploadId, name: 'notes.txt', placed_as: '0001-notes.txt', bytes: 3, holds_secret: false, ...fields } });
const fileOf = (bytes, name = 'notes.txt') => new File([bytes], name);
/** A broker that takes each file whole: begun with `partBytes`, each part taken, its end placed. */
function taking(partBytes = 4, held = ['0001-notes.txt', '0003-txt.exe_x_javascript_alert_1_b']) {
    // It lists what it put, as the inbox does.
    return (action) => {
        switch (action.action) {
            case 'begin_file_in': return { result: 'file_in_begun', upload_id: 'file-1', part_bytes: partBytes };
            case 'file_in_part': return { result: 'file_in_part_taken', upload_id: action.upload_id, seq: action.seq };
            case 'end_file_in': return placed(action.upload_id);
            case 'list_inbox': return { result: 'inbox', files: held.map(name => ({ name, bytes: 3 })) };
            default: return undefined;
        }
    };
}
/** An adapter connected through a transport whose broker answers as `respond` does. */
function connected(respond = taking()) {
    const transport = new FakeTransport();
    transport.respond = respond;
    const adapter = new LiveAdapter(transport, 'test-ui');
    transport.open();
    transport.deliver(SAMPLE_WELCOME);
    return { transport, adapter, inbox: adapter.inbox };
}
/** The bytes the parts sent carried, together. */
const carried = (transport) => Buffer.concat(transport.actions('file_in_part').map(part => Buffer.from(part.data, 'base64')));
test('a file is given in order, a part at a time, and the chat shows it once it is put', async () => {
    const { transport, adapter, inbox } = connected(taking(4));
    const changes = [];
    adapter.subscribe(() => changes.push(inbox.sends('main').map(send => send.state).join(',')));
    assert.equal(inbox.takes('main'), true);
    assert.equal(inbox.takes('chat-1'), false);
    inbox.give('main', fileOf('hello world'));
    assert.deepEqual(inbox.sends('main').map(send => [send.name, send.bytes]), [['notes.txt', 11]]);
    assert.equal('file' in inbox.sends('main')[0], false, 'the file itself is never handed out');
    await settle();
    const [begin] = transport.actions('begin_file_in');
    assert.deepEqual(begin, { action: 'begin_file_in', chat_id: 'local', name: 'notes.txt', bytes: 11 });
    assert.deepEqual(transport.actions('file_in_part').map(part => part.seq), [0, 1, 2]);
    assert.equal(carried(transport).toString(), 'hello world');
    assert.deepEqual(transport.actions('end_file_in'), [{ action: 'end_file_in', upload_id: 'file-1' }]);
    assert.deepEqual(inbox.sends('main'), [], 'a file put is shown by the chat, not the row');
    assert.ok(changes.includes('sending'), 'its sending was shown');
    assert.deepEqual(inbox.given('file-1'), { name: 'notes.txt', placedAs: '0001-notes.txt', bytes: 3, holdsSecret: false, removed: false });
    assert.ok(transport.actions('list_inbox').length >= 2, 'the list is read again after a file is put');
    // An empty file has no parts: its end follows its beginning.
    inbox.give('main', fileOf(''));
    await settle();
    assert.equal(transport.actions('file_in_part').length, 3);
    assert.equal(transport.actions('end_file_in').length, 2);
});
test('many parts redraw the app a bounded number of times', async () => {
    const { adapter, inbox } = connected(taking(1));
    let draws = 0;
    adapter.subscribe(() => { draws++; });
    inbox.give('main', fileOf('x'.repeat(100)));
    await settle();
    await settle();
    assert.ok(draws < 40, `${draws} redraws for 100 parts`);
});
test('one file at a time: a second waits until the first has ended', async () => {
    let release;
    const { transport, inbox } = connected((action) => {
        if (action.action === 'end_file_in' && release === undefined) {
            release = action.upload_id;
            return undefined;
        }
        return taking(64)(action);
    });
    inbox.give('main', fileOf('one', 'one.txt'));
    inbox.give('main', fileOf('two', 'two.txt'));
    await settle();
    assert.deepEqual(transport.actions('begin_file_in').map(begin => begin.name), ['one.txt'], 'the second began before the first ended');
    assert.deepEqual(inbox.sends('main').map(send => [send.name, send.state]), [['one.txt', 'sending'], ['two.txt', 'waiting']]);
    const end = transport.sent.filter(frame => frame.type === 'action' && frame.action.action === 'end_file_in').at(-1);
    transport.deliver({ type: 'action_result', request_id: end.request_id, result: placed('file-1') });
    await settle();
    assert.deepEqual(transport.actions('begin_file_in').map(begin => begin.name), ['one.txt', 'two.txt']);
    assert.deepEqual(inbox.sends('main'), []);
});
test('a file past the bound, or given outside the main chat, is refused before anything is sent', async () => {
    const { transport, inbox } = connected();
    inbox.give('main', new File([new Uint8Array(FILE_IN_BYTES + 1)], 'big.bin'));
    inbox.give('chat-1', fileOf('x'));
    await settle();
    assert.deepEqual(transport.actions('begin_file_in'), []);
    const [big] = inbox.sends('main');
    assert.deepEqual([big.state, big.retry], ['refused', false]);
    assert.match(big.detail, /8 MB at most/u);
    const [other] = inbox.sends('chat-1');
    assert.deepEqual([other.state, other.retry, other.detail], ['refused', false, 'Hatchling takes a file only in the main chat.']);
    // Nothing that can't work is sent again; a dismissed send is gone.
    inbox.retry(big.id);
    await settle();
    assert.deepEqual(transport.actions('begin_file_in'), []);
    inbox.dismiss(big.id);
    inbox.dismiss(other.id);
    assert.deepEqual([inbox.sends('main'), inbox.sends('chat-1')], [[], []]);
});
/** How a send ends when its beginning is answered with `result`. */
async function begunAs(result) {
    const { inbox } = connected((action) => action.action === 'begin_file_in' ? result : taking()(action));
    inbox.give('main', fileOf('abc'));
    await settle();
    const [send] = inbox.sends('main');
    return [send.state, send.detail, send.retry];
}
test('each refusal of a file is said in the owner\'s words, and says whether trying again can work', async () => {
    const said = (refusal) => begunAs({ result: 'file_in_refused', reason: refusal });
    assert.deepEqual(await said({ refusal: 'too_large', limit: FILE_IN_BYTES }), ['refused', 'This file is larger than Hatchling takes: 8 MB at most.', false]);
    assert.deepEqual((await said({ refusal: 'busy' })).slice(2), [true]);
    assert.match((await said({ refusal: 'busy' }))[1], /Another file/u);
    assert.deepEqual(await said({ refusal: 'no_room' }), ['refused', 'The agent’s computer isn’t on, so it can’t take a file.', true]);
    for (const refusal of ['unknown_upload', 'out_of_step'])
        assert.deepEqual(await said({ refusal }), ['refused', 'It didn’t arrive whole. Nothing was put.', true]);
    assert.deepEqual(await said({ refusal: 'refused', said: 'the Room’s inbox is full: it holds 64 files' }), ['refused', 'The agent’s inbox didn’t take it: the Room’s inbox is full: it holds 64 files. Nothing was put.', true]);
    assert.deepEqual((await said({ refusal: 'refused', said: 'the agent’s inbox couldn’t be reached: Hatchling’s log says why.' }))[1], 'The agent’s inbox didn’t take it: the agent’s inbox couldn’t be reached: Hatchling’s log says why. Nothing was put.', 'a sentence ended once');
    assert.match((await said({ refusal: 'unknown' }))[1], /^Not confirmed/u);
    const refused = (reason) => begunAs({ result: 'refused', reason });
    assert.deepEqual(await refused('not_granted'), ['refused', 'This connection may not give Hatchling files.', false]);
    assert.deepEqual((await refused('bad_request')).slice(2), [false]);
    assert.deepEqual((await refused('unavailable')).slice(2), [true]);
    assert.deepEqual((await refused('not_connected')).slice(2), [true]);
    assert.deepEqual(await begunAs({ result: 'inbox', files: [] }), ['refused', 'Hatchling answered with something else. Nothing was put.', false]);
    assert.deepEqual(await begunAs({ result: 'file_in_begun', upload_id: 'file-1', part_bytes: 0 }), ['refused', 'Hatchling answered with something else. Nothing was put.', false]);
});
test('a part answered out of step, or refused, ends the file as not put', async () => {
    const parted = async (answer) => {
        const { transport, inbox } = connected((action) => action.action === 'file_in_part' ? answer(action) : taking()(action));
        inbox.give('main', fileOf('abcdefgh'));
        await settle();
        assert.deepEqual(transport.actions('end_file_in'), [], 'an end followed a part that wasn\'t taken');
        const [send] = inbox.sends('main');
        return [send.state, send.detail];
    };
    for (const wrong of [
        (action) => ({ result: 'file_in_part_taken', upload_id: action.upload_id, seq: action.seq + 1 }),
        (action) => ({ result: 'file_in_part_taken', upload_id: 'file-9', seq: action.seq }),
        () => ({ result: 'inbox', files: [] }),
    ])
        assert.deepEqual(await parted(wrong), ['refused', 'Hatchling answered with something else. Nothing was put.']);
    assert.deepEqual(await parted(() => ({ result: 'file_in_refused', reason: { refusal: 'out_of_step' } })), ['refused', 'It didn’t arrive whole. Nothing was put.']);
    assert.deepEqual(await parted(() => ({ result: 'refused', reason: 'not_granted' })), ['refused', 'This connection may not give Hatchling files.']);
});
test('a file that can\'t be read ends as not put, and may be given again', async () => {
    const { transport, inbox } = connected();
    const broken = { name: 'moved.txt', size: 3, slice: () => ({ arrayBuffer: () => Promise.reject(new Error('gone')) }) };
    inbox.give('main', broken);
    await settle();
    const [send] = inbox.sends('main');
    assert.deepEqual([send.state, send.retry], ['refused', true]);
    assert.match(send.detail, /couldn’t be read/u);
    assert.deepEqual(transport.actions('file_in_part'), []);
});
test('a connection lost before the end puts nothing; after it, the file isn\'t known either way', async () => {
    // Lost on a part: nothing was put, and it may be sent again.
    let drop;
    const { transport, inbox } = connected((action) => {
        if (action.action === 'file_in_part' && drop !== undefined) {
            queueMicrotask(drop);
            return undefined;
        }
        return taking()(action);
    });
    drop = () => transport.drop();
    inbox.give('main', fileOf('abc'));
    await settle();
    let [send] = inbox.sends('main');
    assert.deepEqual([send.state, send.detail, send.retry], ['refused', 'The connection was lost. Nothing was put.', true]);
    // Given again, unconnected: never sent.
    inbox.retry(send.id);
    await settle();
    [send] = inbox.sends('main');
    assert.deepEqual([send.state, send.detail], ['refused', 'Not connected. Nothing was put.']);
    // Lost on the end: not known.
    const after = connected((action) => {
        if (action.action === 'end_file_in') {
            queueMicrotask(() => after.transport.drop());
            return undefined;
        }
        return taking()(action);
    });
    after.inbox.give('main', fileOf('abc'));
    await settle();
    [send] = after.inbox.sends('main');
    assert.deepEqual([send.state, 'retry' in send], ['unknown', false]);
    assert.match(send.detail, /^Not confirmed/u);
    // The stream's word that it was put settles it, by its upload id.
    void after.adapter.chatSession.reconnect();
    after.transport.open();
    after.transport.deliver(SAMPLE_WELCOME);
    after.transport.deliver(fileIn({ message_id: 'file-1' }));
    await settle();
    assert.deepEqual(after.inbox.sends('main'), []);
    // An end never sent, the connection gone after the last part, put nothing.
    const unsent = connected((action) => {
        if (action.action === 'file_in_part') {
            queueMicrotask(() => {
                unsent.transport.deliver({ type: 'action_result', request_id: unsent.transport.sent.at(-1).request_id, result: { result: 'file_in_part_taken', upload_id: action.upload_id, seq: action.seq } });
                unsent.transport.drop();
            });
            return undefined;
        }
        return taking()(action);
    });
    unsent.inbox.give('main', fileOf('abc'));
    await settle();
    [send] = unsent.inbox.sends('main');
    assert.deepEqual([send.state, send.detail, send.retry], ['refused', 'Not connected. Nothing was put.', true]);
});
test('an end the broker says it can\'t know, or answers otherwise, is said as it is', async () => {
    const ended = async (answer) => {
        const { inbox } = connected((action) => action.action === 'end_file_in' ? answer(action) : taking()(action));
        inbox.give('main', fileOf('abc'));
        await settle();
        const [send] = inbox.sends('main');
        return send === undefined ? 'placed' : [send.state, send.detail];
    };
    assert.match((await ended(() => ({ result: 'file_in_refused', reason: { refusal: 'unknown' } })))[1], /^Not confirmed/u);
    assert.deepEqual(await ended(() => ({ result: 'file_in_refused', reason: { refusal: 'refused', said: 'the Room’s disk is full: the file wasn’t put in its inbox' } })), ['refused', 'The agent’s inbox didn’t take it: the Room’s disk is full: the file wasn’t put in its inbox. Nothing was put.']);
    assert.deepEqual(await ended(() => ({ result: 'refused', reason: 'unavailable' })), ['refused', 'Hatchling couldn’t take the file. Nothing was put.']);
    // Placed under another id, or any other answer: not known.
    assert.equal((await ended(() => placed('file-9')))[0], 'unknown');
    assert.equal(await ended((action) => placed(action.upload_id)), 'placed');
});
test('a step not answered in time ends the file: before its end as not put, after it as not known', async () => {
    const transport = new FakeTransport();
    const client = new LiveClient(transport, 'test-ui', () => undefined);
    let serial = 0;
    const inbox = new LiveInbox(client, () => undefined, () => `request-${++serial}`, { step: 5, end: 5 });
    client.connect();
    transport.open();
    transport.respond = (action) => action.action === 'list_inbox' ? { result: 'inbox', files: [] } : undefined;
    transport.deliver(SAMPLE_WELCOME);
    inbox.give('main', fileOf('abc'));
    await new Promise(resolve => setTimeout(resolve, 30));
    let [send] = inbox.sends('main');
    assert.deepEqual([send.state, send.detail, send.retry], ['refused', 'Hatchling didn’t answer in time. Nothing was put.', true]);
    inbox.dismiss(send.id);
    transport.respond = (action) => action.action === 'end_file_in' ? undefined : taking()(action);
    inbox.give('main', fileOf('abc'));
    await new Promise(resolve => setTimeout(resolve, 40));
    [send] = inbox.sends('main');
    assert.equal(send.state, 'unknown');
    // An answer that came in time ends nothing early.
    transport.respond = taking();
    inbox.dismiss(send.id);
    inbox.give('main', fileOf('abc'));
    await new Promise(resolve => setTimeout(resolve, 20));
    assert.deepEqual(inbox.sends('main'), []);
});
test('the stream\'s word of a file put is the chat\'s, by the inbox\'s name alone; one holding a secret is followed by a warning', async () => {
    const { transport, adapter, inbox } = connected();
    transport.deliver(fileIn({ message_id: 'file-1', name: '‮txt.exe [x](javascript:alert(1)) <b>', placed_as: '0003-txt.exe_x_javascript_alert_1_b', bytes: 2048, holds_secret: true }));
    transport.deliver(fileIn({ message_id: 'file-2', placed_as: '../not-a-name', bytes: 1 }));
    // Seen again in a replay, nothing doubles.
    transport.deliver(fileIn({ message_id: 'file-1', name: '‮txt.exe [x](javascript:alert(1)) <b>', placed_as: '0003-txt.exe_x_javascript_alert_1_b', bytes: 2048, holds_secret: true }));
    await settle();
    const messages = adapter.getSnapshot().messages.filter(message => message.id.startsWith('file-'));
    assert.deepEqual(messages.map(message => [message.id, message.kind, message.role, message.chatId]), [
        ['file-1', 'notice', 'agent', 'main'],
        ['file-1:secret', 'error', 'agent', 'main'],
        ['file-2', 'notice', 'agent', 'main'],
    ]);
    assert.equal(messages[0].text, 'You gave Hatchling a file. It was put in its inbox as 0003-txt.exe_x_javascript_alert_1_b, 2 KB.');
    assert.equal(messages[1].text, HOLDS_SECRET);
    assert.equal(messages[2].text, 'You gave Hatchling a file.');
    for (const message of messages)
        assert.doesNotMatch(message.text, /‮|<|\(|\[/u, 'the owner\'s name reached the chat\'s text');
    assert.deepEqual(inbox.given('file-1'), { name: '‮txt.exe [x](javascript:alert(1)) <b>', placedAs: '0003-txt.exe_x_javascript_alert_1_b', bytes: 2048, holdsSecret: true, removed: false });
    assert.equal(inbox.given('file-9'), undefined);
});
test('the inbox is read when the connection opens, and said as the broker answers', async () => {
    const listed = async (answer) => {
        const { inbox } = connected((action) => action.action === 'list_inbox' ? answer : undefined);
        await settle();
        return inbox.current();
    };
    const { inbox } = connected(() => undefined);
    assert.deepEqual(inbox.current(), { kind: 'unread' });
    assert.deepEqual(await listed({ result: 'inbox', files: [{ name: '0001-a.txt', bytes: 3 }, { name: '../evil', bytes: 1 }, { name: '.numbered', bytes: 1 }] }), { kind: 'read', files: [{ name: '0001-a.txt', bytes: 3 }] });
    assert.deepEqual(await listed({ result: 'file_in_refused', reason: { refusal: 'no_room' } }), { kind: 'failed', detail: 'The agent’s computer isn’t on, so its inbox can’t be read.' });
    assert.deepEqual(await listed({ result: 'refused', reason: 'not_granted' }), { kind: 'failed', detail: 'This connection may not read the agent’s inbox.' });
    assert.deepEqual(await listed({ result: 'file_in_refused', reason: { refusal: 'busy' } }), { kind: 'failed', detail: 'Hatchling answered with something else.' });
    // Unconnected: said so.
    const transport = new FakeTransport();
    const client = new LiveClient(transport, 'test-ui', () => undefined);
    const alone = new LiveInbox(client, () => undefined, () => 'request-1');
    await alone.refresh();
    assert.equal(alone.current().kind, 'failed');
    // Read again each time the connection opens, once.
    const again = connected(() => undefined);
    const reads = () => again.transport.actions('list_inbox').length;
    const before = reads();
    again.transport.deliver({ type: 'event', seq: 1, ts: '2026-11-16T00:00:00Z', body: { kind: 'agent_status', state: 'idle', detail: '' } });
    assert.equal(reads(), before, 'read again without the connection opening again');
});
test('a file is removed by the inbox\'s name alone, and the chat says it was', async () => {
    const removing = async (answer) => {
        const { transport, inbox } = connected((action) => action.action === 'remove_inbox_file' ? answer : taking()(action));
        transport.deliver(fileIn({ message_id: 'file-1' }));
        await settle();
        const result = await inbox.remove('0001-notes.txt');
        await settle();
        return [result, inbox.given('file-1').removed, transport.actions('remove_inbox_file')];
    };
    const [removed, gone, asked] = await removing({ result: 'inbox_file_removed', name: '0001-notes.txt' });
    assert.deepEqual([removed, gone, asked], [{ ok: true, message: 'Removed from the agent’s inbox.' }, true, [{ action: 'remove_inbox_file', name: '0001-notes.txt' }]]);
    assert.deepEqual((await removing({ result: 'file_in_refused', reason: { refusal: 'refused', said: 'no file of that name is in the Room’s inbox' } }))[0], { ok: false, message: 'The agent’s inbox didn’t remove it: no file of that name is in the Room’s inbox.' });
    assert.deepEqual((await removing({ result: 'file_in_refused', reason: { refusal: 'no_room' } }))[0], { ok: false, message: 'The agent’s computer isn’t on, so nothing can be removed from its inbox.' });
    assert.deepEqual((await removing({ result: 'refused', reason: 'not_granted' }))[0], { ok: false, message: 'This connection may not remove files from the agent’s inbox.' });
    assert.deepEqual((await removing({ result: 'inbox', files: [] }))[0], { ok: false, message: 'Hatchling answered with something else.' });
    // A name the inbox doesn't give is never sent.
    const { transport, inbox } = connected();
    for (const name of ['../0001-a', '.numbered', '0001-.hidden', '0001-a/b', ''])
        assert.deepEqual(await inbox.remove(name), { ok: false, message: 'That isn’t a file in the agent’s inbox.' });
    assert.deepEqual(transport.actions('remove_inbox_file'), []);
    // Unconnected: said so.
    transport.drop();
    assert.equal((await inbox.remove('0001-a')).ok, false);
});
test('a part is never larger than this app sends, whatever the broker says', async () => {
    const { transport, inbox } = connected(taking(1 << 20));
    inbox.give('main', fileOf('x'.repeat(40000)));
    await settle();
    assert.deepEqual(transport.actions('file_in_part').map(part => Buffer.from(part.data, 'base64').length), [32768, 7232]);
});
test('a send that throws ends as not put, and the next goes on', async () => {
    const { transport, inbox } = connected();
    const send = transport.send.bind(transport);
    let thrown = false;
    transport.send = (text) => {
        if (!thrown && text.includes('begin_file_in')) {
            thrown = true;
            throw new Error('the socket refused it');
        }
        send(text);
    };
    inbox.give('main', fileOf('abc', 'first.txt'));
    inbox.give('main', fileOf('abc', 'second.txt'));
    await settle();
    const [first] = inbox.sends('main');
    assert.deepEqual([first.name, first.state, first.detail, first.retry], ['first.txt', 'refused', 'Hatchling answered with something else. Nothing was put.', true]);
    assert.equal(inbox.sends('main').length, 1, 'the second went on and was put');
});
test('the list is read once at a time, however often it is asked for', async () => {
    let answer;
    const { transport, inbox } = connected((action) => {
        if (action.action === 'list_inbox' && answer === undefined)
            return { result: 'inbox', files: [] };
        return undefined;
    });
    await settle();
    const reads = () => transport.actions('list_inbox').length;
    const before = reads();
    answer = 'held';
    const asked = [inbox.refresh(), inbox.refresh(), inbox.refresh()];
    assert.equal(reads(), before + 1, 'three asks, one read under way');
    assert.ok(asked[0] === asked[1] && asked[1] === asked[2]);
    const waiting = transport.sent.filter(frame => frame.type === 'action' && frame.action.action === 'list_inbox').at(-1);
    transport.deliver({ type: 'action_result', request_id: waiting.request_id, result: { result: 'inbox', files: [] } });
    await settle();
    assert.equal(reads(), before + 2, 'once more after it, not once each');
});
test('a file the list no longer holds reads as removed, after a reload too; one given since the list was read doesn\'t', async () => {
    let held = ['0001-notes.txt'];
    const { transport, inbox } = connected((action) => action.action === 'list_inbox' ? { result: 'inbox', files: held.map(name => ({ name, bytes: 3 })) } : undefined);
    await settle();
    transport.deliver(fileIn({ message_id: 'file-1' }));
    await settle();
    assert.equal(inbox.given('file-1').removed, false);
    // Taken out elsewhere: the next read says so.
    held = [];
    await inbox.refresh();
    assert.equal(inbox.given('file-1').removed, true);
    // Given after the list was read: not taken for removed before a read that could hold it.
    transport.respond = () => undefined;
    transport.deliver(fileIn({ message_id: 'file-2', placed_as: '0002-new.txt' }));
    assert.equal(inbox.given('file-2').removed, false);
});
test('a removal answers once the list is read again', async () => {
    let held = ['0001-notes.txt'];
    const { transport, inbox } = connected((action) => {
        if (action.action === 'remove_inbox_file') {
            held = [];
            return { result: 'inbox_file_removed', name: action.name };
        }
        return action.action === 'list_inbox' ? { result: 'inbox', files: held.map(name => ({ name, bytes: 3 })) } : undefined;
    });
    await settle();
    assert.deepEqual(inbox.current().files.map(file => file.name), ['0001-notes.txt']);
    assert.equal((await inbox.remove('0001-notes.txt')).ok, true);
    assert.deepEqual(inbox.current(), { kind: 'read', files: [] }, 'the list said before the removal answered');
    assert.equal(transport.actions('remove_inbox_file').length, 1);
});
test('a file\'s bytes go as base64, a piece at a time', () => {
    const bytes = new Uint8Array(20000).map((_, index) => index % 251);
    assert.equal(base64(bytes), Buffer.from(bytes).toString('base64'));
    assert.equal(base64(new Uint8Array()), '');
    assert.equal(isInboxName('0001-a.txt'), true);
    assert.equal(isInboxName('0001-.a'), false);
    assert.deepEqual([sizeWords(1), sizeWords(2), sizeWords(2048), sizeWords(3 * 1048576)], ['1 byte', '2 bytes', '2 KB', '3 MB']);
});
