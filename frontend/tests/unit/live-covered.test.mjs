// The requests the owner's standing grants covered (WO-036), over a fake
// client: listed on connecting and as runs go on, one request at a time,
// and shown in Activity as one thread, each with the address the agent asked
// for and the grant that covered it.
import test from 'node:test';
import assert from 'node:assert/strict';
import { LiveCovered } from '../../dist/app/adapters/live/covered.js';
process.env.TZ = 'UTC';
/** A client that answers each request from a script, held until released, and records it. */
class FakeClient {
    sent = [];
    listeners = [];
    held = [];
    state = { kind: 'reconnecting', detail: '' };
    subscribe(listener) { this.listeners.push(listener); return () => { }; }
    connection() { return this.state; }
    set(kind) {
        this.state = { kind, detail: '' };
        for (const listener of this.listeners)
            listener();
    }
    request(id, action) {
        this.sent.push(action);
        return new Promise(resolve => this.held.push(resolve));
    }
    answer(reply) {
        const next = this.held.shift();
        assert.ok(next !== undefined, 'nothing asked');
        next(reply);
    }
}
const flush = () => new Promise(resolve => setImmediate(resolve));
const covered = (...records) => ({ kind: 'answered', result: { result: 'covered', covered: records } });
const record = (count, extra = {}) => ({ key: 'web.visit:https://a.example', scope: 'always', url: `https://a.example/${count}?q=1`, via: 'fetch', task: 't-1', count, at: '2026-11-20T08:00:00Z', ...extra });
const surface = () => {
    const client = new FakeClient();
    let changes = 0;
    let serial = 0;
    const live = new LiveCovered(client, () => { changes++; }, () => `r${++serial}`);
    return { client, live, changes: () => changes };
};
test('covered requests: asked for on connecting, the broker\'s list, and nothing shown until something was covered', async () => {
    const { client, live, changes } = surface();
    assert.equal(live.thread(), undefined);
    client.set('ready');
    assert.deepEqual(client.sent, [{ action: 'list_covered' }]);
    client.set('ready');
    assert.equal(client.sent.length, 1, 'once a connection');
    client.answer(covered());
    await flush();
    assert.deepEqual(live.covered(), []);
    assert.equal(live.thread(), undefined, 'none covered');
    assert.equal(changes(), 1);
    // Reconnected: asked again.
    client.set('reconnecting');
    client.set('ready');
    assert.equal(client.sent.length, 2);
});
test('a run going on asks again, one request at a time, and once more if asked meanwhile', async () => {
    const { client, live } = surface();
    live.observe();
    live.observe();
    live.observe();
    assert.equal(client.sent.length, 1, 'one at a time');
    client.answer(covered(record(1)));
    await flush();
    assert.equal(client.sent.length, 2, 'once more, for what was asked meanwhile');
    client.answer(covered(record(2), record(1)));
    await flush();
    assert.equal(client.sent.length, 2);
    assert.equal(live.covered().length, 2);
    // An answer that isn't the list, or none, leaves the list as it was.
    live.observe();
    client.answer({ kind: 'answered', result: { result: 'refused', reason: 'not_granted' } });
    await flush();
    live.observe();
    client.answer({ kind: 'unsent', detail: 'offline' });
    await flush();
    assert.equal(live.covered().length, 2);
});
test('Activity shows each covered request with its address and the grant that covered it', async () => {
    const { client, live } = surface();
    live.observe();
    client.answer(covered(record(64, { url: null, scope: 'day', at: '2026-11-20T09:30:00Z' }), record(2, { via: 'browser', key: 'web.visit:https://b.example:8443', url: 'https://b.example:8443/x' }), record(1, { at: '2026-11-20T07:00:00Z' })));
    await flush();
    const thread = live.thread();
    assert.equal(thread.name, 'Allowed by your standing grants');
    assert.equal(thread.subtitle, '3 requests · newest 09:30');
    assert.equal(thread.startedAt, '2026-11-20T07:00:00Z', 'from the oldest');
    assert.match(thread.summary, /^3 requests your standing grants let through without asking/);
    const rows = thread.lanes[0].rows;
    assert.deepEqual(rows.map(row => row.title), ['64 requests to https://a.example by one task', 'https://b.example:8443/x', 'https://a.example/1?q=1']);
    assert.deepEqual(rows.map(row => row.subtitle), ['Allowed by your 24-hour grant for https://a.example', 'Allowed by your always grant for https://b.example:8443', 'Allowed by your always grant for https://a.example']);
    assert.match(rows[0].report, /had sent 64 requests under your 24-hour grant by then; past the first, they are counted/);
    assert.match(rows[1].report, /^Opened in the agent’s browser, the address as the agent asked for it\. You weren’t asked: web\.visit:https:\/\/b\.example:8443 covers it\./);
    assert.match(rows[2].report, /^Read, the address as the agent asked for it\./);
    assert.ok(rows.every(row => row.icon === 'globe' && row.status === 'success' && row.workId === 't-1'));
    // One request is said as one.
    live.observe();
    client.answer(covered(record(1)));
    await flush();
    assert.equal(live.thread().subtitle, '1 request · newest 08:00');
});
