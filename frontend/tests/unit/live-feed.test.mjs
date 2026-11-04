// The owner's Feed page over a fake client and a fake schedule list: the
// prompt and time as a schedule that posts on the page, "write one now" as a
// one-off of it, and every broker answer as a state or a sentence.
import test from 'node:test';
import assert from 'node:assert/strict';
import { FEED_PAGE, LiveFeed, feedGrants, localDateTime, postOf, setupOf } from '../../dist/app/adapters/live/feed.js';
const ZONE = 'America/New_York';
const MODEL = { grant: 'egress', destination: 'localhost:11434' };
const LOCAL = { grant: 'channel-send', channel: 'local' };
const READ_GOALS = { grant: 'goals', action: 'query-goals' };
const RECALL = { grant: 'memory', scope: 'user', action: 'recall' };
const PROPOSE = { grant: 'memory', scope: 'user', action: 'propose' };
const POST = { grant: 'channel-send', channel: 'feed' };
/** A client that answers each request from a script, and records it. */
class FakeClient {
    sent = [];
    listeners = [];
    state = { kind: 'reconnecting', detail: '' };
    held = [MODEL, LOCAL, READ_GOALS, RECALL, PROPOSE];
    constructor(answers = []) {
        this.answers = answers;
    }
    subscribe(listener) { this.listeners.push(listener); return () => { }; }
    connection() { return this.state; }
    grants() { return this.held; }
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
/** The schedule list, as the page sees it: what it made and cancelled, and what each answers. */
class FakeSchedules {
    list = [];
    made = [];
    cancelled = [];
    makes = [];
    cancels = [];
    summaries() { return this.list; }
    make(op) {
        this.made.push(op);
        return Promise.resolve(this.makes.shift() ?? { ok: true, message: '', id: `sch-${this.made.length}` });
    }
    cancel(id) {
        this.cancelled.push(id);
        return Promise.resolve(this.cancels.shift() ?? { ok: true, message: 'Removed.' });
    }
}
const daily = (id, expr = '30 8 * * *', extra = {}) => ({ id, kind: 'task', when: { every: 'cron', expr, tz: ZONE }, channel: FEED_PAGE, grants: [MODEL, READ_GOALS, POST], next_due: 1790000000, task: 'What moved on my goals.', ...extra });
const oneOff = id => ({ ...daily(id), when: { every: 'once', at: '2026-11-04T08:00:15', tz: ZONE } });
const feed = posts => ({ kind: 'answered', result: { result: 'feed', posts } });
const kept = (id, text, at = '2026-11-04T13:30:00Z') => ({ post_id: id, written_at: at, text });
const flush = () => new Promise(resolve => setImmediate(resolve));
const surface = (answers = []) => {
    const client = new FakeClient(answers);
    const schedules = new FakeSchedules();
    let changes = 0;
    let serial = 0;
    const live = new LiveFeed(client, schedules, () => { changes++; }, () => `r${++serial}`, () => ZONE, () => new Date('2026-11-04T13:00:00Z'));
    return { client, schedules, live, changes: () => changes };
};
test('a kept post reads as a title and a post, the first line the title', () => {
    const post = postOf(kept('post-1', '## Your Tuesday\n\nTwo goals moved.\n- Run 5k'));
    assert.deepEqual({ ...post, date: typeof post.date }, { id: 'post-1', title: 'Your Tuesday', body: 'Two goals moved.\n- Run 5k', kicker: 'Your feed', category: 'Written for you', icon: 'feed', reason: post.reason, date: 'string', reacted: false });
    assert.match(post.reason, /feed prompt/);
    assert.match(post.reason, /wasn’t sent anywhere/);
    assert.equal(postOf(kept('p', '**Bold title**\nBody')).title, 'Bold title');
    // One line is the post, under a plain title.
    assert.deepEqual([postOf(kept('p', '  Just this.  ')).title, postOf(kept('p', 'Just this.')).body], ['Your post', 'Just this.']);
    assert.equal(postOf(kept('p', 'x', 'not a time')).date, 'Undated');
    assert.notEqual(postOf(kept('p', 'x')).date, 'Undated');
});
test('a post\'s run reads what this connection may read, and posts on the page; never writes, nor the chat', () => {
    assert.deepEqual(feedGrants([MODEL, LOCAL, READ_GOALS, { grant: 'goals', action: 'goal-create' }, RECALL, PROPOSE, { grant: 'memory', scope: 'user', action: 'write' }, { grant: 'vault', action: 'fetch', name: 'k' }]), [MODEL, READ_GOALS, RECALL, POST]);
    assert.deepEqual(feedGrants([]), [POST]);
});
test('a one-off\'s time is written on its zone\'s clock, to the second', () => {
    assert.equal(localDateTime(new Date('2026-11-04T13:00:15Z'), ZONE), '2026-11-04T08:00:15');
    assert.equal(localDateTime(new Date('2026-11-04T23:59:59Z'), 'UTC'), '2026-11-04T23:59:59');
    assert.equal(localDateTime(new Date('2026-11-05T00:00:00Z'), 'UTC'), '2026-11-05T00:00:00', 'midnight is 00, not 24');
});
test('a daily schedule reads as the prompt, its time and when, in words', () => {
    assert.deepEqual(setupOf(daily('sch-1')), { prompt: 'What moved on my goals.', time: '08:30', when: `Every day at 08:30 (${ZONE})` });
    assert.equal(setupOf(daily('sch-1', '5 7 * * *')).time, '07:05');
    assert.equal(setupOf(daily('sch-1', '0 8 * * 1-5')).time, '', 'not a daily time: none');
    assert.equal(setupOf(oneOff('sch-2')).time, '');
    assert.equal(setupOf(daily('sch-1', '30 8 * * *', { task: null })).prompt, '');
});
test('the page is read each time the connection opens, and is unread until then', async () => {
    const { client, live, schedules, changes } = surface([feed([kept('post-2', 'Newer\nB'), kept('post-1', 'Older\nA')]), feed([])]);
    assert.deepEqual(live.current(), { kind: 'unread' });
    assert.deepEqual(live.posts(), []);
    client.set('ready');
    client.set('ready');
    await flush();
    assert.deepEqual(client.sent, [{ action: 'read_feed' }], 'once while it stays open');
    assert.deepEqual(live.posts().map(post => post.title), ['Newer', 'Older']);
    assert.deepEqual(live.current(), { kind: 'read', setup: null, writing: false });
    assert.equal(changes(), 1);
    // The daily schedule that stands is the newest; a one-off is being written.
    schedules.list = [daily('sch-1', '0 7 * * *'), { ...daily('sch-x'), channel: 'local' }, daily('sch-2'), oneOff('sch-3')];
    assert.deepEqual(live.current(), { kind: 'read', setup: setupOf(daily('sch-2')), writing: true });
    client.set('offline');
    client.set('ready');
    await flush();
    assert.equal(client.sent.length, 2, 'again after it reopens');
    assert.deepEqual(live.posts(), []);
});
test('a run that ends reads the page again; one that only started does not', async () => {
    const { client, live } = surface([feed([]), feed([])]);
    live.observe({ outcome: 'fired', schedule_id: 'sch-1' });
    live.observe({ outcome: 'cancelled', schedule_id: 'sch-1' });
    assert.equal(client.sent.length, 0);
    live.observe({ outcome: 'completed', schedule_id: 'sch-1' });
    live.observe({ outcome: 'failed', schedule_id: 'sch-1', detail: '' });
    await flush();
    assert.equal(client.sent.length, 2);
});
test('a read that fails says why, and a later one recovers', async () => {
    const { live } = surface([
        { kind: 'unsent', detail: 'Not connected.' },
        { kind: 'answered', result: { result: 'refused', reason: 'not_granted' } },
        { kind: 'answered', result: { result: 'usage', usage: { counted: false, days: [], models: [] } } },
        feed([kept('post-1', 'Back\nAgain')]),
    ]);
    const said = [];
    for (let read = 0; read < 3; read++) {
        await live.refresh();
        assert.equal(live.current().kind, 'failed');
        said.push(live.current().detail);
    }
    assert.deepEqual(said, ['Not connected.', 'This connection may not read the Feed page.', 'Hatchling answered with something else.']);
    await live.refresh();
    assert.equal(live.current().kind, 'read');
    assert.equal(live.posts()[0].title, 'Back');
});
test('saving the prompt makes a daily schedule that posts on the page, then removes the earlier one', async () => {
    const { live, schedules } = surface();
    schedules.list = [daily('sch-old'), { ...daily('sch-chat'), channel: 'local' }];
    assert.deepEqual(await live.save('  What moved on my goals.  ', '07:05'), { ok: true, message: 'Saved. Hatchling writes your post every day at 07:05.' });
    assert.deepEqual(schedules.made, [{ op: 'create', when: { every: 'cron', expr: '5 7 * * *', tz: ZONE }, task: 'What moved on my goals.', grants: [MODEL, READ_GOALS, RECALL, POST], channel: 'feed' }]);
    assert.deepEqual(schedules.cancelled, ['sch-old'], 'the chat\'s schedules are left alone');
    // Nothing reaches the broker without a prompt and a time.
    assert.deepEqual(await live.save('   ', '07:05'), { ok: false, message: 'Write your feed prompt first: it says what each post is about.' });
    for (const time of ['7:05', '24:00', '07:60', ''])
        assert.deepEqual(await live.save('x', time), { ok: false, message: 'Pick a time.' });
    // A refused schedule leaves the earlier one standing.
    schedules.makes.push({ ok: false, message: 'Hatchling keeps as many schedules as it can. Remove one first.' });
    assert.deepEqual(await live.save('x', '08:00'), { ok: false, message: 'Hatchling keeps as many schedules as it can. Remove one first.' });
    assert.equal(schedules.made.length, 2);
    assert.deepEqual(schedules.cancelled, ['sch-old']);
});
test('a post asked for now is a one-off of the daily schedule, a few seconds out on its clock', async () => {
    const { live, schedules } = surface();
    assert.deepEqual(await live.writeNow(), { ok: false, message: 'Write your feed prompt first: it says what each post is about.' });
    schedules.list = [daily('sch-1', '30 8 * * *', { task: null })];
    assert.deepEqual(await live.writeNow(), { ok: false, message: 'Write your feed prompt first: it says what each post is about.' }, 'a schedule whose prompt isn\'t shown');
    assert.equal(schedules.made.length, 0);
    schedules.list = [daily('sch-1')];
    assert.deepEqual(await live.writeNow(), { ok: true, message: 'Writing your post now. It appears here when it’s done, in a minute or two.' });
    assert.deepEqual(schedules.made, [{ op: 'create', when: { every: 'once', at: '2026-11-04T08:00:15', tz: ZONE }, task: 'What moved on my goals.', grants: [MODEL, READ_GOALS, POST], channel: 'feed' }]);
    schedules.makes.push({ ok: false, message: 'That time has passed.' });
    assert.deepEqual(await live.writeNow(), { ok: false, message: 'That time has passed.' });
});
test('stopping removes the daily post; the posts stay, and a failed removal says so', async () => {
    const { live, schedules } = surface();
    assert.deepEqual(await live.stop(), { ok: false, message: 'There’s no daily post to stop.' });
    schedules.list = [daily('sch-1'), oneOff('sch-2'), daily('sch-3')];
    assert.deepEqual(await live.stop(), { ok: true, message: 'Stopped. Your posts stay here.' });
    assert.deepEqual(schedules.cancelled, ['sch-1', 'sch-3'], 'a post being written now still arrives');
    schedules.cancels.push({ ok: false, message: 'This schedule was already gone.' });
    assert.deepEqual(await live.stop(), { ok: false, message: 'This schedule was already gone.' });
    assert.deepEqual(schedules.cancelled, ['sch-1', 'sch-3', 'sch-1'], 'it stops at the first that fails');
});
test('the browser\'s own zone and clock, unless a test says', async () => {
    const client = new FakeClient();
    const schedules = new FakeSchedules();
    const live = new LiveFeed(client, schedules, () => { }, () => 'r1');
    assert.equal((await live.save('x', '08:00')).ok, true);
    assert.equal(schedules.made[0].when.tz, Intl.DateTimeFormat().resolvedOptions().timeZone);
    schedules.list = [daily('sch-1')];
    const before = Date.now();
    assert.equal((await live.writeNow()).ok, true);
    const at = schedules.made[1].when.at;
    assert.match(at, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}$/);
    assert.ok(at >= localDateTime(new Date(before), ZONE), 'a few seconds from now');
});
