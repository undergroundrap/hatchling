// What the agent is given from the owner's memory, over a fake client: listed on
// connecting and after a decision or Hatchling's notice, forgotten from the
// Memory page, and every broker answer as a sentence.
import test from 'node:test';
import assert from 'node:assert/strict';
import { LiveMemory, entryOf, titleOf } from '../../dist/app/adapters/live/memory.js';
process.env.TZ = 'UTC';
/** A client that answers each request from a script, and records it. */
class FakeClient {
    sent = [];
    listeners = [];
    state = { kind: 'reconnecting', detail: '' };
    constructor(answers = []) {
        this.answers = answers;
    }
    subscribe(listener) { this.listeners.push(listener); return () => { }; }
    connection() { return this.state; }
    set(kind) {
        this.state = { kind, detail: '' };
        for (const listener of this.listeners)
            listener();
    }
    request(id, action) {
        this.sent.push(action);
        const next = this.answers.shift();
        assert.ok(next !== undefined, `no answer scripted for ${JSON.stringify(action)}`);
        return Promise.resolve(next);
    }
}
// Noon, 2026-11-03, UTC.
const AT = 1793707200;
const kept = (id, content, approved = true) => ({ id, content, kept_at: AT, approved });
const memory = outcome => ({ kind: 'answered', result: { result: 'memory', outcome } });
const listed = (...items) => memory({ result: 'listed', kept: items });
const flush = () => new Promise(resolve => setImmediate(resolve));
const surface = answers => {
    const client = new FakeClient(answers);
    let changes = 0;
    let serial = 0;
    const live = new LiveMemory(client, () => { changes++; }, () => `r${++serial}`);
    return { client, live, changes: () => changes };
};
test('a kept thing reads as a Memory page entry, titled by its first words', () => {
    assert.deepEqual(entryOf(kept('user/kept-1', 'Their cat is Biscuit.')), {
        id: 'user/kept-1', kind: 'memory', title: 'Their cat is Biscuit.', body: 'Their cat is Biscuit.', date: 'Approved by you, Nov 3, 2026', closeness: 0,
    });
    assert.equal(entryOf(kept('project/note', 'A note.', false)).date, 'Kept Nov 3, 2026');
    assert.equal(titleOf('First line\nsecond line'), 'First line');
    assert.equal(titleOf(`${'word '.repeat(20)}end`), `${'word '.repeat(12).trim()}…`);
    assert.equal(titleOf('x'.repeat(80)), `${'x'.repeat(60)}…`);
    assert.equal(titleOf(''), '');
});
test('what is kept is listed once each time the connection opens, and is empty until then', async () => {
    const { client, live, changes } = surface([listed(kept('user/a', 'A')), memory({ result: 'off' })]);
    assert.deepEqual(live.entries(), []);
    client.set('ready');
    client.set('ready');
    await flush();
    assert.deepEqual(client.sent, [{ action: 'manage_memory', op: { op: 'list' } }]);
    assert.deepEqual(live.entries().map(entry => entry.id), ['user/a']);
    assert.equal(changes(), 1);
    // A list that isn't one leaves it as it was.
    await live.refresh();
    assert.deepEqual(live.entries().map(entry => entry.id), ['user/a']);
});
test('a decision, or Hatchling\'s own word, reads it again; anything else does not', async () => {
    const { client, live } = surface([listed(), listed()]);
    const event = body => ({ seq: 1, ts: 't', body });
    live.observe(event({ kind: 'chat_message', author: 'agent', chat_id: 'local', message_id: 'm', text: 'hi' }));
    live.observe(event({ kind: 'confirmation', confirmation_id: 'c' }));
    assert.equal(client.sent.length, 0);
    live.observe(event({ kind: 'confirmation_decided', confirmation_id: 'c', revision: 1, decision: 'approve' }));
    live.observe(event({ kind: 'chat_message', author: 'system', chat_id: 'local', message_id: 'm', text: 'Remembered' }));
    await flush();
    assert.equal(client.sent.length, 2);
});
test('forgetting reads the list again, and every answer is said', async () => {
    const { client, live } = surface([memory({ result: 'forgotten' }), listed()]);
    assert.deepEqual(await live.forget('user/a'), { ok: true, message: 'Forgotten. The agent isn’t given it any more.' });
    assert.deepEqual(client.sent[0], { action: 'manage_memory', op: { op: 'forget', id: 'user/a' } });
    assert.equal(client.sent.length, 2);
    const cases = [
        [{ kind: 'failed', detail: 'Not connected.' }, /Not connected/, false],
        [{ kind: 'answered', result: { result: 'refused', reason: 'not_granted' } }, /may not change your memory/, false],
        [{ kind: 'answered', result: { result: 'goals', outcome: { result: 'off' } } }, /something else/, false],
        [listed(), /something else/, false],
        [memory({ result: 'not_found' }), /no longer kept/, true],
        [memory({ result: 'off' }), /HATCHLING_MEMORY_DIR/, false],
        [memory({ result: 'failed' }), /Its log says why/, false],
    ];
    for (const [reply, says, relists] of cases) {
        const { client, live } = surface(relists ? [reply, listed()] : [reply]);
        const result = await live.forget('user/a');
        assert.equal(result.ok, false);
        assert.match(result.message, says);
        assert.equal(client.sent.length, relists ? 2 : 1, JSON.stringify(reply));
    }
});
