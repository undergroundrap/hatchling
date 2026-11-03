// The owner's goals over a fake client: listed on connecting and after each
// change, set, changed and closed, and every broker answer as a sentence.
import test from 'node:test';
import assert from 'node:assert/strict';
import { LiveGoals, goalOf } from '../../dist/app/adapters/live/goals.js';
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
// Noon, 2026-11-03 and 2026-11-05, UTC.
const SET = 1793707200;
const LATER = SET + 2 * 86400;
const view = (id, extra = {}) => ({ id, title: `Goal ${id}`, detail: 'Under an hour', state: 'open', created_at: SET, updated_at: SET, ...extra });
const goals = outcome => ({ kind: 'answered', result: { result: 'goals', outcome } });
const listed = (...items) => goals({ result: 'listed', goals: items });
const flush = () => new Promise(resolve => setImmediate(resolve));
const surface = answers => {
    const client = new FakeClient(answers);
    let changes = 0;
    let serial = 0;
    const live = new LiveGoals(client, () => { changes++; }, () => `r${++serial}`);
    return { client, live, changes: () => changes };
};
test('a goal reads as the Goals page shows it', () => {
    const open = goalOf(view('g1'));
    assert.deepEqual(open, {
        id: 'g1', kind: 'user', title: 'Goal g1', description: 'Under an hour', category: 'Something else', status: 'open',
        pulse: 'Under an hour', pulseKind: 'monitoring', attention: false, due: null, timeline: [{ date: 'Nov 3, 2026', text: 'Set' }],
    });
    const bare = goalOf(view('g2', { detail: '', updated_at: LATER }));
    assert.equal(bare.pulse, 'Set Nov 3, 2026');
    assert.deepEqual(bare.timeline.map(entry => entry.text), ['Set', 'Changed']);
    const reached = goalOf(view('g3', { state: 'achieved', updated_at: LATER }));
    assert.deepEqual([reached.status, reached.pulse, reached.pulseKind], ['closed', 'Reached Nov 5, 2026', 'milestone']);
    assert.deepEqual(reached.timeline.map(entry => entry.text), ['Set', 'Reached']);
    const aside = goalOf(view('g4', { state: 'abandoned', updated_at: LATER }));
    assert.deepEqual([aside.status, aside.pulse], ['closed', 'Set aside Nov 5, 2026']);
});
test('the goals are listed once each time the connection opens, and are empty until then', async () => {
    const { client, live, changes } = surface([listed(view('g1')), goals({ result: 'off' }), { kind: 'failed', detail: 'gone' }]);
    assert.deepEqual(live.goals(), []);
    client.set('ready');
    client.set('ready');
    await flush();
    assert.deepEqual(client.sent, [{ action: 'manage_goals', op: { op: 'list' } }], 'once while it stays open');
    assert.deepEqual(live.goals().map(goal => goal.id), ['g1']);
    assert.equal(changes(), 1);
    // A list that doesn't answer with goals leaves them as they were.
    client.set('offline');
    client.set('ready');
    await flush();
    await live.refresh();
    assert.deepEqual(live.goals().map(goal => goal.id), ['g1']);
    assert.equal(changes(), 1);
});
test('setting, changing and closing a goal reads the list again', async () => {
    const saved = goals({ result: 'saved', goal: view('g1') });
    const { client, live } = surface([saved, listed(view('g1')), saved, listed(view('g1')), saved, listed(), saved, listed()]);
    assert.deepEqual(await live.create('  Run a 10k ', ' Under an hour  '), { ok: true, message: 'Goal set.' });
    assert.deepEqual(client.sent[0], { action: 'manage_goals', op: { op: 'create', title: 'Run a 10k', detail: 'Under an hour' } });
    assert.deepEqual(await live.update('g1', 'Run a half', ''), { ok: true, message: 'Saved.' });
    assert.deepEqual(client.sent[2], { action: 'manage_goals', op: { op: 'update', id: 'g1', title: 'Run a half', detail: '' } });
    assert.deepEqual(await live.close('g1', true), { ok: true, message: 'Marked reached. It stays in Closed goals.' });
    assert.deepEqual(client.sent[4], { action: 'manage_goals', op: { op: 'close', id: 'g1', achieved: true } });
    assert.deepEqual(await live.close('g1', false), { ok: true, message: 'Set aside. It stays in Closed goals.' });
    assert.equal(client.sent.filter(action => action.op.op === 'list').length, 4);
});
test('every answer to a change is said, and a gone or closed goal reads the list again', async () => {
    const cases = [
        [{ kind: 'failed', detail: 'Not connected.' }, 'Not connected.', false],
        [{ kind: 'answered', result: { result: 'refused', reason: 'not_granted' } }, /may not change your goals/, false],
        [{ kind: 'answered', result: { result: 'sites', outcome: { result: 'off' } } }, /something else/, false],
        [listed(), /something else/, false],
        [goals({ result: 'not_found' }), /no longer there/, true],
        [goals({ result: 'already_closed' }), /is closed, so it can’t change/, true],
        [goals({ result: 'rejected' }), /title is one line/, false],
        [goals({ result: 'limit_reached' }), /as many goals as Hatchling keeps/, false],
        [goals({ result: 'off' }), /HATCHLING_GOALS_DIR/, false],
        [goals({ result: 'failed' }), /Its log says why/, false],
    ];
    for (const [reply, says, relists] of cases) {
        const { client, live } = surface(relists ? [reply, listed()] : [reply]);
        const result = await live.close('g1', true);
        assert.equal(result.ok, false);
        if (typeof says === 'string')
            assert.equal(result.message, says);
        else
            assert.match(result.message, says);
        assert.equal(client.sent.length, relists ? 2 : 1, JSON.stringify(reply));
    }
});
