// What the agent is given from the owner's memory, over a fake client: listed on
// connecting and after a decision or Hatchling's notice, forgotten from the
// Memory page, and every broker answer as a sentence.
import test from 'node:test';
import assert from 'node:assert/strict';
import { LiveMemory, detailOf, entryOf, titleOf } from '../../dist/app/adapters/live/memory.js';
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
const kept = (id, content, approved = true, extra = {}) => ({ id, content, kept_at: AT, approved, from: { from: 'not_recorded' }, ...extra });
const memory = outcome => ({ kind: 'answered', result: { result: 'memory', outcome } });
const listed = (...items) => memory({ result: 'listed', kept: items });
const moded = mode => memory({ result: 'mode', mode });
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
    const { client, live, changes } = surface([listed(kept('user/a', 'A')), moded('automatic'), memory({ result: 'off' })]);
    assert.deepEqual(live.entries(), []);
    client.set('ready');
    client.set('ready');
    await flush();
    // Then whether memory is automatic (WO-057), so the page says it from the start.
    assert.deepEqual(client.sent, [{ action: 'manage_memory', op: { op: 'list' } }, { action: 'manage_memory', op: { op: 'mode' } }]);
    assert.deepEqual(live.entries().map(entry => entry.id), ['user/a']);
    assert.equal(live.mode(), 'automatic');
    assert.equal(changes(), 2);
    // A list that isn't one leaves it as it was.
    await live.refresh();
    assert.deepEqual(live.entries().map(entry => entry.id), ['user/a']);
});
test('a decision, or Hatchling\'s own word, reads it again; anything else does not', async () => {
    const { client, live } = surface([listed(), listed()]);
    const event = body => ({ seq: 1, ts: 't', body });
    live.observe(event({ kind: 'chat_message', author: 'agent', chat_id: 'local', message_id: 'm', text: 'hi' }));
    live.observe(event({ kind: 'confirmation', grant_key: null, scopes: ['once'], confirmation_id: 'c' }));
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
test('a kept memory\'s details read in the page\'s words', () => {
    assert.deepEqual(detailOf(kept('user/a', 'Use metric units.', true, {
        from: { from: 'chat', chat: 'local' }, kind: 'guidance', why: 'They live in Europe.', how_to_apply: 'Any measurement.',
        confirmed_at: AT + 86400, check_by: '2000-01-01', due: true, replaces: 'They use miles.',
    })), {
        kind: 'How you want it to work', why: 'They live in Europe.', howToApply: 'Any measurement.', absence: false,
        origin: 'Asked to remember in your chat', confirmed: 'Last confirmed Nov 4, 2026', due: true, replaces: 'They use miles.', automatic: false,
    });
    const plain = detailOf(kept('user/b', 'x'));
    assert.deepEqual(plain, { kind: null, why: null, howToApply: null, absence: false, origin: 'Where it came from isn’t recorded', confirmed: null, due: false, replaces: null, automatic: false });
    // Kept automatically (WO-057): labelled so, on its line and in its details.
    const automatic = kept('user/i', 'Pepper is a cat.', false, { kept_by: 'automatic', from: { from: 'chat', chat: 'local' } });
    assert.equal(detailOf(automatic).automatic, true);
    assert.equal(entryOf(automatic).date, 'Kept automatically, Nov 3, 2026');
    assert.equal(detailOf(kept('user/j', 'x', true, { kept_by: 'owner' })).automatic, false);
    assert.equal(entryOf(kept('user/j', 'x', true, { kept_by: 'owner' })).date, 'Approved by you, Nov 3, 2026');
    for (const [kind, words] of [['owner', 'About you'], ['work', 'About your work'], ['pointer', 'Where something is'], ['other', null]])
        assert.equal(detailOf(kept('user/c', 'x', true, { kind })).kind, words, kind);
    for (const [from, words] of [[{ from: 'chat', chat: 'side-1' }, 'Asked to remember in a chat'], [{ from: 'run' }, 'Asked to remember by a scheduled run'], [{ from: 'approval' }, 'Approved by you']])
        assert.equal(detailOf(kept('user/d', 'x', true, { from })).origin, words);
    // Confirmed the day it was kept says nothing more.
    assert.equal(detailOf(kept('user/e', 'x', true, { confirmed_at: AT })).confirmed, null);
    assert.equal(detailOf(kept('user/f', 'x', true, { confirmed_at: null })).confirmed, null);
    assert.equal(detailOf(kept('user/g', 'x', true, { absence: true })).absence, true);
    // From an older broker, saying nothing of where it came from.
    const { from: _unsaid, ...older } = kept('user/h', 'x');
    assert.equal(detailOf(older).origin, 'Where it came from isn’t recorded');
});
test('a listed memory\'s details are kept beside it, and confirming reads the list again', async () => {
    const { client, live } = surface([listed(kept('user/a', 'A', true, { due: true, kind: 'owner' })), moded('automatic')]);
    client.set('ready');
    await flush();
    assert.equal(live.of('user/a')?.due, true);
    assert.equal(live.of('user/none'), undefined);
    const confirming = surface([memory({ result: 'confirmed' }), listed(kept('user/a', 'A'))]);
    assert.deepEqual(await confirming.live.confirm('user/a'), { ok: true, message: 'Confirmed: it’s still so, as of today.' });
    assert.deepEqual(confirming.client.sent[0], { action: 'manage_memory', op: { op: 'confirm', id: 'user/a' } });
    assert.equal(confirming.client.sent.length, 2);
    assert.equal(confirming.live.of('user/a')?.due, false);
    const cases = [
        [{ kind: 'failed', detail: 'Not connected.' }, /Not connected/, false],
        [{ kind: 'answered', result: { result: 'refused', reason: 'not_granted' } }, /may not change your memory/, false],
        [memory({ result: 'not_found' }), /no longer kept/, true],
        [memory({ result: 'forgotten' }), /something else/, false],
        [memory({ result: 'off' }), /HATCHLING_MEMORY_DIR/, false],
    ];
    for (const [reply, says, relists] of cases) {
        const { client, live } = surface(relists ? [reply, listed()] : [reply]);
        const result = await live.confirm('user/a');
        assert.equal(result.ok, false);
        assert.match(result.message, says);
        assert.equal(client.sent.length, relists ? 2 : 1, JSON.stringify(reply));
    }
    // Forgetting answered with a confirmation is something else.
    const odd = surface([memory({ result: 'confirmed' })]);
    assert.match((await odd.live.forget('user/a')).message, /something else/);
});
test('whether memory is automatic is read as a page that says it opens, and set only by the owner', async () => {
    const { client, live, changes } = surface([listed(), moded('automatic'), moded('automatic'), moded('ask'), memory({ result: 'off' })]);
    assert.equal(live.mode(), 'unread');
    // Not connected, opening reads nothing.
    await live.opened();
    assert.equal(client.sent.length, 0);
    client.set('ready');
    await flush();
    assert.equal(live.mode(), 'automatic');
    // Opened again, it's read again: unchanged, nothing is drawn again.
    const read = changes();
    await live.opened();
    assert.deepEqual(client.sent.at(-1), { action: 'manage_memory', op: { op: 'mode' } });
    assert.equal(client.sent.length, 3);
    assert.equal(changes(), read);
    const drawn = changes();
    assert.deepEqual(await live.setMode(false), { ok: true, message: 'The agent asks you first now: nothing is kept until you approve its words.' });
    assert.deepEqual(client.sent.at(-1), { action: 'manage_memory', op: { op: 'set_mode', mode: 'ask' } });
    assert.equal(live.mode(), 'ask');
    assert.equal(changes(), drawn + 1);
    // Memory off on this install: no mode, and the setting isn't shown.
    await live.opened();
    assert.equal(live.mode(), 'off');
});
test('setting the mode says what it is now, or why it is not set', async () => {
    const { client, live } = surface([listed(), moded('ask'), moded('automatic'), memory({ result: 'failed' }), { kind: 'answered', result: { result: 'refused', reason: 'not_granted' } }, memory({ result: 'forgotten' })]);
    client.set('ready');
    await flush();
    assert.equal(live.mode(), 'ask');
    assert.deepEqual(await live.setMode(true), { ok: true, message: 'Memory is automatic: what you tell the agent about yourself is kept at once, unless the conversation read something from outside.' });
    assert.deepEqual(client.sent.at(-1), { action: 'manage_memory', op: { op: 'set_mode', mode: 'automatic' } });
    assert.deepEqual(await live.setMode(false), { ok: false, message: 'Hatchling couldn’t change your memory. Its log says why.' });
    assert.equal(live.mode(), 'automatic');
    assert.equal((await live.setMode(false)).ok, false);
    assert.match((await live.setMode(false)).message, /something else/);
    assert.equal(live.mode(), 'automatic');
});
test('a mode that does not read leaves what was read', async () => {
    const { client, live } = surface([listed(), moded('ask'), memory({ result: 'failed' })]);
    client.set('ready');
    await flush();
    await live.opened();
    assert.equal(client.sent.length, 3);
    assert.equal(live.mode(), 'ask');
});
