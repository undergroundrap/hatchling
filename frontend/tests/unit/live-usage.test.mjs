// The model usage the broker counted, over a fake client: read on connecting
// and after each answer, and every broker answer as a state the view can say.
import test from 'node:test';
import assert from 'node:assert/strict';
import { LiveUsage, usageOf } from '../../dist/app/adapters/live/usage.js';
import { compact, summary, weekday } from '../../dist/app/features/settings/usage.js';
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
const day = (date, calls = 2, extra = {}) => ({ date, calls, input_tokens: 1200, output_tokens: 300, unreported: 0, ...extra });
const REPORT = { counted: true, days: [day('2026-11-03'), day('2026-11-02', 0, { input_tokens: 0, output_tokens: 0 })], models: [{ model: 'qwen3.5:9b', calls: 2, input_tokens: 1200, output_tokens: 300 }] };
const usage = report => ({ kind: 'answered', result: { result: 'usage', usage: report } });
const flush = () => new Promise(resolve => setImmediate(resolve));
const surface = answers => {
    const client = new FakeClient(answers);
    let changes = 0;
    let serial = 0;
    const live = new LiveUsage(client, () => { changes++; }, () => `r${++serial}`);
    return { client, live, changes: () => changes };
};
test('the broker\'s report reads in the app\'s words', () => {
    assert.deepEqual(usageOf(REPORT), {
        counted: true,
        days: [
            { date: '2026-11-03', calls: 2, inputTokens: 1200, outputTokens: 300, unreported: 0 },
            { date: '2026-11-02', calls: 0, inputTokens: 0, outputTokens: 0, unreported: 0 },
        ],
        models: [{ model: 'qwen3.5:9b', calls: 2, inputTokens: 1200, outputTokens: 300 }],
    });
});
test('usage is read once each time the connection opens, and is unread until then', async () => {
    const { client, live, changes } = surface([usage(REPORT), usage({ ...REPORT, counted: false })]);
    assert.deepEqual(live.current(), { kind: 'unread' });
    client.set('ready');
    client.set('ready');
    await flush();
    assert.deepEqual(client.sent, [{ action: 'read_usage' }], 'once while it stays open');
    assert.equal(live.current().kind, 'read');
    assert.equal(live.current().usage.days[0].inputTokens, 1200);
    assert.equal(changes(), 1);
    client.set('offline');
    client.set('ready');
    await flush();
    assert.equal(client.sent.length, 2, 'again after it reopens');
    assert.equal(live.current().usage.counted, false);
});
test('an answer from the agent or Hatchling reads it again; the owner\'s own message does not', async () => {
    const { client, live } = surface([usage(REPORT), usage(REPORT)]);
    const message = author => ({ author, chat_id: 'local', message_id: 'm1', text: 'hi' });
    live.observe(message('user'));
    assert.equal(client.sent.length, 0);
    live.observe(message('agent'));
    live.observe(message('system'));
    await flush();
    assert.equal(client.sent.length, 2);
});
test('a read that fails says why', async () => {
    const { live } = surface([
        { kind: 'failed', detail: 'Not connected.' },
        { kind: 'answered', result: { result: 'refused', reason: 'not_granted' } },
        { kind: 'answered', result: { result: 'sites', outcome: { result: 'off' } } },
    ]);
    await live.refresh();
    assert.deepEqual(live.current(), { kind: 'failed', detail: 'Not connected.' });
    await live.refresh();
    assert.deepEqual(live.current(), { kind: 'failed', detail: 'This connection may not read the usage.' });
    await live.refresh();
    assert.deepEqual(live.current(), { kind: 'failed', detail: 'Hatchling answered with something else.' });
});
test('the view says counts short, days by weekday, and a span in one line', () => {
    assert.equal(compact(950), '950');
    assert.equal(compact(48200), '48.2K');
    assert.equal(compact(1250000), '1.3M');
    assert.equal(weekday('2026-11-03'), 'Tue');
    assert.equal(weekday('not a date'), 'not a date');
    const days = usageOf(REPORT).days;
    assert.equal(summary(days.slice(1)), 'No model calls.');
    assert.equal(summary(days), '2 model calls · 1.2K read · 300 written');
    assert.equal(summary([{ ...days[0], calls: 1, unreported: 1 }]), '1 model call · 1.2K read · 300 written · 1 call didn’t report usage');
});
