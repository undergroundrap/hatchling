// The model usage the broker counted, over a fake client: read on connecting
// and after each answer, and every broker answer as a state the view can say.
import test from 'node:test';
import assert from 'node:assert/strict';
import { LiveUsage, usageOf } from '../../dist/app/adapters/live/usage.js';
import { compact, fitNote, monthName, parseTokens, summary, tokensText, weekday } from '../../dist/app/features/settings/usage.js';
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
const REPORT = { counted: true, days: [day('2026-11-03'), day('2026-11-02', 0, { input_tokens: 0, output_tokens: 0 })], models: [{ model: 'qwen3.5:9b', calls: 2, input_tokens: 1200, output_tokens: 300 }], limits: { daily_tokens: 500000 }, spent: { today: 1500, month: 9000, month_of: '2026-11' } };
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
        limits: { daily: 500000, monthly: null },
        spent: { today: 1500, month: 9000, monthOf: '2026-11' },
    });
    assert.deepEqual(usageOf({ ...REPORT, limits: { daily_tokens: null, monthly_tokens: 7 } }).limits, { daily: null, monthly: 7 });
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
test('limits are set as the broker words them, and answered with the usage', async () => {
    const answered = { ...REPORT, limits: { monthly_tokens: 2000000 } };
    const { client, live, changes } = surface([usage(answered), usage(REPORT)]);
    const set = await live.setLimits({ daily: null, monthly: 2000000 });
    assert.deepEqual(client.sent, [{ action: 'set_usage_limits', limits: { monthly_tokens: 2000000 } }]);
    assert.deepEqual(set, { ok: true, message: 'Limits saved. Hatchling checks them before every model call.' });
    assert.deepEqual(live.current().usage.limits, { daily: null, monthly: 2000000 });
    assert.equal(changes(), 1);
    const cleared = await live.setLimits({ daily: null, monthly: null });
    assert.deepEqual(client.sent[1], { action: 'set_usage_limits', limits: {} });
    assert.deepEqual(cleared, { ok: true, message: 'No limits now.' });
    const both = surface([usage(REPORT)]);
    await both.live.setLimits({ daily: 5, monthly: 9 });
    assert.deepEqual(both.client.sent, [{ action: 'set_usage_limits', limits: { daily_tokens: 5, monthly_tokens: 9 } }]);
});
test('limits that weren\'t set say why, and leave the usage as it was', async () => {
    const refused = reason => ({ kind: 'answered', result: { result: 'refused', reason } });
    const { live, changes } = surface([
        { kind: 'failed', detail: 'Not connected.' },
        refused('bad_request'),
        refused('unavailable'),
        refused('not_granted'),
        refused('not_connected'),
        { kind: 'answered', result: { result: 'sites', outcome: { result: 'off' } } },
    ]);
    const said = [];
    for (let attempt = 0; attempt < 6; attempt++) {
        const result = await live.setLimits({ daily: 1, monthly: null });
        assert.equal(result.ok, false);
        said.push(result.message);
    }
    assert.deepEqual(said, [
        'Not connected.',
        'Hatchling didn’t take those limits: each is a whole number of tokens, from 1 to a quadrillion.',
        'Hatchling couldn’t keep the limits. It counts usage only when scheduling is on; its log says if a write failed.',
        'This connection may not set the limits.',
        'Not connected.',
        'Hatchling answered with something else.',
    ]);
    assert.deepEqual(live.current(), { kind: 'unread' });
    assert.equal(changes(), 0);
});
test('the context is a chat answer\'s largest prompt: background work and runs without a model call don\'t move it', () => {
    const { live, changes } = surface([]);
    const run = (chat, calls, peak, window) => ({ event: 'run_summary', event_id: 'e', ts: 't', run_id: 'r', cell_id: 'c', chat, tools: [], model_calls: calls, model_calls_refused: 0, peak_input_tokens: peak, output_tokens: 1, context_window: window });
    assert.equal(live.context(), null);
    live.observeRun({ event: 'api_call', event_id: 'e', ts: 't', run_id: 'api', caller: 'ui', capabilities: [], request: 'hello', outcome: 'served' });
    live.observeRun(run(false, 3, 9000, null));
    live.observeRun(run(true, 0, 0, null));
    assert.equal(live.context(), null);
    assert.equal(changes(), 0);
    live.observeRun(run(true, 2, 5500, 16384));
    assert.deepEqual(live.context(), { tokens: 5500, window: 16384, onCpu: null, memoryLow: false });
    live.observeRun({ ...run(true, 1, 700, null), context_window: undefined });
    assert.deepEqual(live.context(), { tokens: 700, window: null, onCpu: null, memoryLow: false });
    live.observeRun({ ...run(true, 1, 700, 131072), model_on_cpu: 900000000, memory_low: true });
    assert.deepEqual(live.context(), { tokens: 700, window: 131072, onCpu: 900000000, memoryLow: true });
    assert.equal(changes(), 3);
});
test('the owner is told when the model ran partly on the CPU, or the computer was low on memory', () => {
    const context = (onCpu, memoryLow) => ({ tokens: 5000, window: 32768, onCpu, memoryLow });
    assert.equal(fitNote(context(null, false)), null);
    assert.equal(fitNote(context(0, false)), null);
    assert.equal(fitNote(context(99999999, false)), null, 'too little to feel');
    assert.equal(fitNote(context(100000000, false)), 'Part of the model (0.1 GB) ran on the CPU, so answers are slower. Something else may be using the GPU’s memory, or the model’s window is large for it.');
    assert.equal(fitNote(context(null, true)), 'This computer was low on memory: closing other programs keeps Hatchling quick.');
    assert.equal(fitNote(context(900000000, true)), 'Part of the model (0.9 GB) ran on the CPU, so answers are slower. Something else may be using the GPU’s memory, or the model’s window is large for it. This computer was low on memory: closing other programs keeps Hatchling quick.');
});
test('a limit reads as the owner writes it, and shows short', () => {
    assert.equal(parseTokens(''), null);
    assert.equal(parseTokens('   '), null);
    assert.equal(parseTokens('500000'), 500000);
    assert.equal(parseTokens('500,000'), 500000);
    assert.equal(parseTokens('500K'), 500000);
    assert.equal(parseTokens(' 1.5m '), 1500000);
    assert.equal(parseTokens('2B'), 2000000000);
    assert.equal(parseTokens('1000000000000000'), 1e15);
    for (const not of ['0', '-5', 'abc', '5 tokens', '1e3', '1000000000000001', '2KB', '.5'])
        assert.equal(parseTokens(not), undefined, not);
    assert.equal(tokensText(null), '');
    assert.equal(tokensText(500000), '500K');
    assert.equal(tokensText(2000000), '2M');
    assert.equal(tokensText(3000000000), '3B');
    assert.equal(tokensText(1500000), '1500K');
    assert.equal(tokensText(1234), '1234');
    for (const limit of [1, 999, 1000, 1500000, 2000000, 1e15])
        assert.equal(parseTokens(tokensText(limit)), limit, String(limit));
    assert.equal(monthName('2026-11'), 'November');
    assert.equal(monthName('not a month'), 'This month');
});
