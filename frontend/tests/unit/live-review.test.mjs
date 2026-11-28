// What background work may not apply on its own, over a fake client: listed on
// connecting and when the Memory page opens, decided one at a time, and every
// broker answer as a sentence.
import test from 'node:test';
import assert from 'node:assert/strict';
import { LiveReview, entryOf } from '../../dist/app/adapters/live/review.js';
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
const item = (item_id, state = 'waiting', kind = 'replace') => ({
    item_id, proposed_by: 'memory', kind, target: 'pref/1', change: 'Say they prefer tea.', why: 'They said so twice.',
    evidence: ['chat/1'], proposed_at: '2026-11-26T12:00:00Z', state, decided_at: null,
});
const review = outcome => ({ kind: 'answered', result: { result: 'review', outcome } });
const listed = (...items) => review({ result: 'listed', items });
const flush = () => new Promise(resolve => setImmediate(resolve));
const surface = answers => {
    const client = new FakeClient(answers);
    let changes = 0;
    let serial = 0;
    const live = new LiveReview(client, () => { changes++; }, () => `r${++serial}`);
    return { client, live, changes: () => changes };
};
test('a proposal reads as the page shows it, each kind and state in the page\'s words', () => {
    assert.deepEqual(entryOf(item('a')), {
        id: 'a', kind: 'Would change a preference, boundary or commitment', change: 'Say they prefer tea.', why: 'They said so twice.',
        date: 'Proposed Nov 26, 2026', state: 'waiting', outcome: '', atOnce: false,
    });
    // Approving a memory keeps it at once; anything else waits for its work.
    assert.equal(entryOf(item('a', 'waiting', 'remember')).atOnce, true);
    for (const [kind, says] of [['remove', /remove/], ['identity', /who you are/], ['unresolved', /settle/], ['guidance', /guidance/], ['remember', /remember about you/]])
        assert.match(entryOf(item('a', 'waiting', kind)).kind, says);
    // A memory approved is kept as it's approved (WO-057), not applied later.
    assert.equal(entryOf(item('a', 'approved', 'remember')).outcome, 'You approved it, so it’s remembered: the agent is given it from now on.');
    assert.match(entryOf(item('a', 'rejected', 'remember')).outcome, /Nothing changed/);
    assert.match(entryOf(item('a', 'approved')).outcome, /approved it/);
    assert.match(entryOf(item('a', 'rejected')).outcome, /Nothing changed/);
    assert.match(entryOf(item('a', 'stale')).outcome, /won’t be applied/);
    assert.equal(entryOf({ ...item('a'), proposed_at: 'not a time' }).date, '');
});
test('the list is read once each time the connection opens, and again as the Memory page opens', async () => {
    const { client, live, changes } = surface([listed(item('b'), item('a', 'approved')), listed()]);
    assert.deepEqual(live.current(), { kind: 'unread' });
    // Not connected, opening the page reads nothing.
    live.opened();
    assert.equal(client.sent.length, 0);
    client.set('ready');
    client.set('ready');
    await flush();
    assert.deepEqual(client.sent, [{ action: 'manage_review', op: { op: 'list' } }]);
    const list = live.current();
    assert.equal(list.kind, 'read');
    assert.deepEqual(list.entries.map(entry => entry.id), ['b', 'a']);
    assert.equal(changes(), 1);
    live.opened();
    await flush();
    assert.equal(client.sent.length, 2);
    assert.deepEqual(live.current(), { kind: 'read', entries: [] });
});
test('with scheduled work off nothing waits; any other answer is said instead of a list', async () => {
    const { live } = surface([review({ result: 'off' })]);
    await live.refresh();
    assert.deepEqual(live.current(), { kind: 'read', entries: [] });
    const cases = [
        [{ kind: 'failed', detail: 'Not connected.' }, /Not connected/],
        [{ kind: 'answered', result: { result: 'refused', reason: 'not_granted' } }, /may not read or decide/],
        [{ kind: 'answered', result: { result: 'memory', outcome: { result: 'off' } } }, /something else/],
        [review({ result: 'failed' }), /hatchling-broker doctor/],
        [review({ result: 'not_found' }), /no longer waiting/],
    ];
    for (const [reply, says] of cases) {
        const { live } = surface([reply]);
        await live.refresh();
        const list = live.current();
        assert.equal(list.kind, 'failed', JSON.stringify(reply));
        assert.match(list.detail, says);
    }
});
test('deciding sends the owner\'s choice and reads the list again, and every answer is said', async () => {
    const { client, live } = surface([review({ result: 'decided', item: item('a', 'approved') }), listed(item('a', 'approved'))]);
    assert.deepEqual(await live.decide('a', true), { ok: true, message: 'Approved. It’s applied the next time that work runs.' });
    const memory = surface([review({ result: 'decided', item: item('m', 'approved', 'remember') }), listed()]);
    assert.deepEqual(await memory.live.decide('m', true), { ok: true, message: 'Approved. It’s remembered.' });
    assert.deepEqual(client.sent[0], { action: 'manage_review', op: { op: 'decide', item_id: 'a', approve: true } });
    assert.equal(client.sent.length, 2);
    assert.equal(live.current().entries[0].state, 'approved');
    const rejected = surface([review({ result: 'decided', item: item('a', 'rejected') }), listed()]);
    assert.deepEqual(await rejected.live.decide('a', false), { ok: true, message: 'Rejected. Nothing changes.' });
    const cases = [
        [{ kind: 'unknown', detail: 'No answer came.' }, /No answer came/, false],
        [{ kind: 'answered', result: { result: 'refused', reason: 'not_granted' } }, /may not read or decide/, false],
        [listed(), /something else/, false],
        [review({ result: 'not_found' }), /no longer waiting/, true],
        [review({ result: 'not_waiting', state: 'approved' }), /already decided/, true],
        [review({ result: 'off' }), /scheduled work is off/, false],
        [review({ result: 'failed' }), /hatchling-broker doctor/, false],
    ];
    for (const [reply, says, relists] of cases) {
        const { client, live } = surface(relists ? [reply, listed()] : [reply]);
        const result = await live.decide('a', true);
        assert.equal(result.ok, false);
        assert.match(result.message, says);
        assert.equal(client.sent.length, relists ? 2 : 1, JSON.stringify(reply));
    }
});
