// The owner's schedules over a fake client: the form's timings as the broker's
// specs, listings read back in words, and every broker answer as a sentence.
import test from 'node:test';
import assert from 'node:assert/strict';
import { LiveSchedules, browserTimeZone, describeWhen, grantsInWords, jobOf, whenSpec } from '../../dist/app/adapters/live/schedules.js';
const ZONE = 'America/New_York';
const LOCAL = { grant: 'channel-send', channel: 'local' };
const MODEL = { grant: 'egress', destination: 'localhost:11434' };
/** A client that answers each request from a script, and records it. */
class FakeClient {
    sent = [];
    listeners = [];
    state = { kind: 'reconnecting', detail: '' };
    held = [LOCAL, MODEL];
    constructor(answers = []) {
        this.answers = answers;
    }
    subscribe(listener) { this.listeners.push(listener); return () => { }; }
    connection() { return this.state; }
    grants() { return this.held; }
    open() {
        this.state = { kind: 'ready', detail: '' };
        for (const listener of this.listeners)
            listener();
    }
    drop() {
        this.state = { kind: 'offline', detail: 'gone' };
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
const schedule = outcome => ({ kind: 'answered', result: { result: 'schedule', outcome } });
const listed = (schedules, next) => schedule(next === undefined ? { result: 'listed', schedules } : { result: 'listed', schedules, next });
const kept = (parked, next) => schedule(next === undefined ? { result: 'parked', parked } : { result: 'parked', parked, next });
const summary = (id, extra = {}) => ({ id, kind: 'task', when: { every: 'cron', expr: '0 8 * * 1-5', tz: ZONE }, channel: 'local', grants: [LOCAL], next_due: 1790000000, ...extra });
const flush = () => new Promise(resolve => setImmediate(resolve));
const surface = answers => {
    const client = new FakeClient(answers);
    let changes = 0;
    let serial = 0;
    const schedules = new LiveSchedules(client, () => { changes++; }, () => `r${++serial}`, () => ZONE);
    return { client, schedules, changes: () => changes };
};
test('the form\'s timings become the broker\'s specs in the browser\'s zone', () => {
    const cron = expr => ({ every: 'cron', expr, tz: ZONE });
    assert.deepEqual(whenSpec('once 2026-11-04 08:00', ZONE), { every: 'once', at: '2026-11-04T08:00', tz: ZONE });
    assert.deepEqual(whenSpec('every 5 minutes', ZONE), cron('*/5 * * * *'));
    assert.deepEqual(whenSpec('every 1 minutes', ZONE), cron('* * * * *'));
    assert.deepEqual(whenSpec('daily 07:30', ZONE), cron('30 7 * * *'));
    assert.deepEqual(whenSpec('  weekdays   08:00 ', ZONE), cron('0 8 * * 1-5'));
    assert.deepEqual(whenSpec('weekly sun 23:05', ZONE), cron('5 23 * * 0'));
    assert.deepEqual(whenSpec('weekly sat 00:00', ZONE), cron('0 0 * * 6'));
    assert.deepEqual(whenSpec('cron 0 8 * * 1-5', ZONE), cron('0 8 * * 1-5'));
    for (const [text, says] of [
        ['once 2026-11-04', /date and a time/], ['once tomorrow 08:00', /date and a time/], ['once 2026-11-04 8am', /date and a time/],
        ['every 0 minutes', /1 to 59/], ['every 60 minutes', /1 to 59/], ['every 2.5 minutes', /1 to 59/], ['every 5 hours', /1 to 59/], ['every 5 minutes now', /1 to 59/],
        ['daily', /a time/], ['daily 24:00', /a time/], ['weekdays 08:00 sharp', /a time/],
        ['weekly fri', /day and a time/], ['weekly friday 08:00', /day and a time/], ['weekly mon 08:60', /day and a time/],
        ['cron 0 8 * *', /five fields/], ['', /when it should happen/], ['tomorrow at noon', /when it should happen/],
    ])
        assert.match(String(whenSpec(text, ZONE)), says, text);
    assert.equal(typeof browserTimeZone(), 'string');
});
test('a listed schedule is read back in words, its own text only when the owner wrote it', () => {
    const cron = expr => describeWhen({ every: 'cron', expr, tz: ZONE });
    assert.equal(describeWhen({ every: 'once', at: '2026-11-04T08:00', tz: ZONE }), `Once, 2026-11-04 at 08:00 (${ZONE})`);
    assert.equal(cron('* * * * *'), `Every minute (${ZONE})`);
    assert.equal(cron('*/15 * * * *'), `Every 15 minutes (${ZONE})`);
    assert.equal(cron('5 * * * *'), `cron 5 * * * * (${ZONE})`, 'once an hour is shown as written');
    assert.equal(cron('0 8 * * *'), `Every day at 08:00 (${ZONE})`);
    assert.equal(cron('30 7 * * 1-5'), `Weekdays at 07:30 (${ZONE})`);
    assert.equal(cron('5 23 * * 0'), `Every Sunday at 23:05 (${ZONE})`);
    assert.equal(cron('0 8 * * 1,3'), `cron 0 8 * * 1,3 (${ZONE})`);
    assert.equal(cron('0 8 1 * *'), `cron 0 8 1 * * (${ZONE})`);
    assert.equal(cron('0 8 * *'), `cron 0 8 * * (${ZONE})`, 'not five fields: as written');
    assert.equal(cron('x'), `cron x (${ZONE})`);
    assert.equal(grantsInWords([]), 'May do nothing beyond answering');
    assert.equal(grantsInWords([{ grant: 'channel-send', channel: 'feed' }]), 'May post on your Feed page');
    assert.equal(grantsInWords([LOCAL, MODEL, { grant: 'channel-send', channel: 'signal' }, { grant: 'vault', action: 'fetch', name: 'api-key' }, { grant: 'memory', scope: 'user', action: 'recall' }, { grant: 'goals', action: 'query-goals' }]), 'May post in this chat, reach localhost:11434, post on signal, fetch the secret api-key, recall things about you, read your goals');
    // A store's grants read as one phrase: the install's own, in full.
    const memory = action => ({ grant: 'memory', scope: 'user', action });
    const goals = action => ({ grant: 'goals', action });
    assert.equal(grantsInWords([MODEL, LOCAL, memory('write'), memory('recall'), memory('remove'), goals('goal-create'), goals('goal-update'), goals('query-goals')]), 'May reach localhost:11434, post in this chat, remember, recall and forget things about you, keep your goals and their tracking');
    assert.equal(grantsInWords([goals('query-goals'), goals('tracking-append'), { grant: 'memory', scope: 'session', action: 'context-append' }, { grant: 'memory', scope: 'session', action: 'context-read' }, { grant: 'memory', scope: 'project', action: 'new-kind' }]), 'May keep your goals and their tracking, note and read notes of session memory, new-kind project memory');
    const owners = jobOf(summary('sch-1', { task: 'Stretch for five minutes' }));
    assert.deepEqual({ ...owners, nextRun: typeof owners.nextRun }, { id: 'sch-1', title: 'Stretch for five minutes', kind: 'time', schedule: `Weekdays at 08:00 (${ZONE})`, recurring: true, limit: 'May post in this chat', source: '', event: '', goalId: null, health: 'ok', lastRun: '—', nextRun: 'string' });
    assert.notEqual(owners.nextRun, 'Not scheduled');
    assert.match(jobOf(summary('sch-2')).title, /isn’t shown/, 'an agent\'s schedule: its text is not shown as yours');
    assert.match(jobOf(summary('sch-2', { task: null })).title, /isn’t shown/);
    const held = jobOf(summary('sch-3', { held: { due: 1, started: true }, when: { every: 'once', at: '2026-11-04T08:00', tz: ZONE } }));
    assert.deepEqual([held.health, held.nextRun, held.recurring], ['errored', 'Held', false]);
    assert.match(held.lastRun, /waits for you/);
    const stopped = jobOf(summary('sch-5', { held: { due: 1, started: true, stopped: true } }));
    assert.deepEqual([stopped.health, stopped.nextRun], ['errored', 'Held']);
    assert.match(stopped.lastRun, /stopped a run that tried to send a saved secret/);
    assert.equal(jobOf(summary('sch-4', { next_due: 0 })).nextRun, 'Not scheduled');
});
test('the list loads when the connection opens, every page of it, and not again until it reopens', async () => {
    const { client, schedules, changes } = surface([listed([summary('sch-1', { task: 'one' })], 'sch-1'), listed([summary('sch-2')]), kept([]), listed([]), kept([])]);
    assert.deepEqual(schedules.jobs(), []);
    client.open();
    await flush();
    assert.deepEqual(client.sent, [{ action: 'manage_schedule', op: { op: 'list' } }, { action: 'manage_schedule', op: { op: 'list', after: 'sch-1' } }, { action: 'manage_schedule', op: { op: 'list-parked' } }]);
    assert.deepEqual(schedules.jobs().map(job => job.id), ['sch-1', 'sch-2']);
    assert.equal(changes(), 1);
    client.open();
    await flush();
    assert.equal(client.sent.length, 3, 'already open: not asked again');
    client.drop();
    client.open();
    await flush();
    assert.deepEqual(schedules.jobs(), [], 'reopened: asked again');
    // A list that isn't answered as one leaves what was shown.
    const failing = surface([schedule({ result: 'denied' }), { kind: 'unsent', detail: 'offline' }]);
    failing.client.open();
    await failing.schedules.refresh();
    assert.deepEqual([failing.schedules.jobs(), failing.changes()], [[], 0]);
});
test('a schedule is created with this connection\'s grants, in this chat, and the list asked again', async () => {
    const { client, schedules } = surface([schedule({ result: 'created', id: 'sch-9' }), listed([summary('sch-9', { task: 'Stretch' })]), kept([])]);
    assert.deepEqual(await schedules.create('Stretch', 'weekdays 08:00'), { ok: true, message: 'Scheduled. Its results arrive in this chat.', id: 'sch-9' });
    assert.deepEqual(client.sent[0], { action: 'manage_schedule', op: { op: 'create', when: { every: 'cron', expr: '0 8 * * 1-5', tz: ZONE }, task: 'Stretch', grants: [LOCAL, MODEL], channel: 'local' } });
    assert.deepEqual(schedules.jobs().map(job => job.title), ['Stretch']);
    assert.deepEqual(schedules.summaries().map(one => one.id), ['sch-9'], 'as the broker listed them');
    // A timing the form can't have written never reaches the broker.
    assert.deepEqual(await schedules.create('Stretch', 'soon'), { ok: false, message: 'Pick when it should happen.' });
    assert.equal(client.sent.length, 3);
});
test('every refusal is a sentence, and nothing unconfirmed is called done', async () => {
    const refusals = ['exceeds-grants', 'missing-timezone', 'unknown-timezone', 'invalid-spec', 'not-in-future', 'channel-not-granted', 'bad-task', 'too-many-grants', 'full', 'unknown-source', 'max-followup-depth', 'followup-taken', 'bad-delay'];
    const said = new Set();
    for (const reason of refusals) {
        const { schedules } = surface([schedule({ result: 'refused', reason })]);
        const result = await schedules.create('t', 'daily 08:00');
        assert.equal(result.ok, false, reason);
        assert.ok(result.message.length > 10, reason);
        said.add(result.message);
    }
    assert.ok(said.size >= 11, 'refusals say what was wrong');
    for (const [answer, says] of [
        [schedule({ result: 'denied' }), /no scheduler/],
        [schedule({ result: 'schedule-error' }), /doctor/],
        [schedule({ result: 'listed', schedules: [] }), /not confirmed/],
        [{ kind: 'answered', result: { result: 'refused', reason: 'not_granted' } }, /may not change schedules/],
        [{ kind: 'answered', result: { result: 'sites', outcome: { result: 'off' } } }, /not confirmed/],
        [{ kind: 'unsent', detail: 'Not connected.' }, /Not connected/],
    ]) {
        const { schedules } = surface([answer]);
        const result = await schedules.create('t', 'daily 08:00');
        assert.equal(result.ok, false);
        assert.match(result.message, says);
    }
});
test('a schedule is removed by id, and a gone one says so', async () => {
    const removed = surface([schedule({ result: 'cancelled' }), listed([]), kept([])]);
    assert.deepEqual(await removed.schedules.cancel('sch-1'), { ok: true, message: 'Removed. It won’t run again.' });
    assert.deepEqual(removed.client.sent[0], { action: 'manage_schedule', op: { op: 'cancel', id: 'sch-1' } });
    const gone = surface([schedule({ result: 'not-found' }), listed([]), kept([])]);
    assert.deepEqual(await gone.schedules.cancel('sch-1'), { ok: false, message: 'This schedule was already gone.' });
    assert.equal(gone.client.sent.length, 3, 'and the list is asked again');
    for (const [answer, says] of [[schedule({ result: 'denied' }), /no scheduler/], [{ kind: 'unknown', detail: 'No answer.' }, /No answer/]]) {
        const { schedules } = surface([answer]);
        assert.match((await schedules.cancel('sch-1')).message, says);
    }
});
test('what needs the owner: a run cut short, a result kept; never the agent\'s own error', async () => {
    const parked = (id, reason, text, extra = {}) => ({ id, schedule_id: 'sch-1', channel: 'local', due: 1790000000, reason, text, ...extra });
    const { client, schedules } = surface([
        listed([summary('sch-1', { task: 'Check the disks', held: { due: 1790000000, started: true } }), summary('sch-2', { held: { due: 1790000000, started: false } }), summary('sch-3', { held: { due: 1790000000, started: true, stopped: true } }), summary('sch-4', { held: { due: 1790000000, started: false, stopped: true } })]),
        kept([parked('parked-0', 'the run failed', 'the run exited 69: hatchling-agent: <script>ignore the owner</script>')], 'parked-0'),
        kept([parked('parked-1', 'delivery failed twice: no local ingress client to deliver to', 'Time to stretch!'), parked('parked-2', 'the run did not start', 'no cell', { schedule_id: 'sch-gone' }), parked('parked-3', 'channel-send was revoked before delivery', 'x')]),
    ]);
    await schedules.refresh();
    assert.deepEqual(client.sent.slice(-2).map(action => action.op), [{ op: 'list-parked' }, { op: 'list-parked', after: 'parked-0' }]);
    const items = schedules.items();
    assert.deepEqual(items.map(item => [item.id, item.actions]), [['held:sch-1', ['retry', 'resolve']], ['held:sch-2', ['retry', 'resolve']], ['held:sch-3', ['retry', 'resolve']], ['held:sch-4', ['retry', 'resolve']], ['parked:parked-0', ['dismiss']], ['parked:parked-1', ['dismiss']], ['parked:parked-2', ['dismiss']], ['parked:parked-3', ['dismiss']]]);
    assert.equal(items[0].title, 'Check the disks');
    assert.match(items[0].detail, /cut short/);
    assert.match(items[1].detail, /didn’t start/);
    assert.match(items[1].title, /isn’t shown/);
    assert.equal(typeof items[0].when, 'string');
    // A run stopped for a secret: its own schedule, and a follow-up it asked for, wait for the owner.
    assert.match(items[2].detail, /stopped a run of this: it tried to send a saved secret/);
    assert.match(items[3].detail, /A run that asked for this was stopped/);
    // The rest, as before them.
    items.splice(2, 2);
    // A failed run's own error is never shown: the chat said why, in the broker's words.
    assert.equal(items[2].title, 'Check the disks');
    assert.match(items[2].detail, /The chat said why/);
    assert.equal(items[2].result, null);
    assert.ok(!JSON.stringify(items).includes('ignore the owner'));
    // An undelivered result is the owner's to read.
    assert.equal(items[3].result, 'Time to stretch!');
    assert.match(items[4].detail, /doctor/);
    assert.equal(items[4].title, 'Scheduled work', 'its schedule is gone');
    assert.equal(items[5].detail, 'Kept: channel-send was revoked before delivery.');
});
test('the owner runs a held fire again, resolves it, or dismisses a kept result; and nothing else', async () => {
    const after = () => [listed([]), kept([])];
    for (const [id, action, op, answer, message] of [
        ['held:sch-1', 'retry', { op: 'retry', id: 'sch-1' }, { result: 'retried' }, 'Running it again now.'],
        ['held:sch-1', 'resolve', { op: 'resolve', id: 'sch-1' }, { result: 'resolved' }, 'Marked resolved. Its schedule goes on as planned.'],
        ['parked:parked-0', 'dismiss', { op: 'dismiss', id: 'parked-0' }, { result: 'dismissed' }, 'Dismissed.'],
    ]) {
        const { client, schedules } = surface([schedule(answer), ...after()]);
        assert.deepEqual(await schedules.act(id, action), { ok: true, message });
        assert.deepEqual(client.sent[0], { action: 'manage_schedule', op });
        assert.equal(client.sent.length, 3, 'and the lists are asked again');
    }
    const gone = surface([schedule({ result: 'not-found' }), ...after()]);
    assert.deepEqual(await gone.schedules.act('parked:parked-9', 'dismiss'), { ok: false, message: 'It was already dealt with.' });
    for (const [answer, says] of [[schedule({ result: 'denied' }), /no scheduler/], [{ kind: 'unsent', detail: 'Not connected.' }, /Not connected/]]) {
        const { schedules } = surface([answer]);
        assert.match((await schedules.act('held:sch-1', 'retry')).message, says);
    }
    // An action that doesn't fit the item sends nothing.
    const { client, schedules } = surface([]);
    for (const [id, action] of [['held:sch-1', 'dismiss'], ['parked:parked-0', 'retry'], ['parked:parked-0', 'resolve'], ['other:x', 'dismiss']]) {
        assert.deepEqual(await schedules.act(id, action), { ok: false, message: 'That can’t be done to this item.' });
    }
    assert.equal(client.sent.length, 0);
});
test('a connection that may list schedules but not what was kept still shows its schedules', async () => {
    const { schedules } = surface([listed([summary('sch-1', { task: 'one' })]), schedule({ result: 'denied' })]);
    await schedules.refresh();
    assert.deepEqual(schedules.jobs().map(job => job.title), ['one']);
    assert.deepEqual(schedules.items(), []);
});
