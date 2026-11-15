// The owner's Ideas page over a fake client and a fake schedule list: what the
// agent suggested, in its own words and its area's section; the owner starting,
// setting aside and bringing back an idea; and asking for ideas now as a one-off
// run for the page.
import test from 'node:test';
import assert from 'node:assert/strict';
import { IDEAS_PAGE, LiveIdeas, ideaOf, ordered } from '../../dist/app/adapters/live/ideas.js';
const ZONE = 'America/New_York';
const MODEL = { grant: 'egress', destination: 'localhost:11434' };
const LOCAL = { grant: 'channel-send', channel: 'local' };
const READ_GOALS = { grant: 'goals', action: 'query-goals' };
const RECALL = { grant: 'memory', scope: 'user', action: 'recall' };
const PROPOSE = { grant: 'memory', scope: 'user', action: 'propose' };
const SUGGEST = { grant: 'channel-send', channel: 'ideas' };
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
/** The schedule list, as the page sees it. */
class FakeSchedules {
    list = [];
    made = [];
    makes = [];
    summaries() { return this.list; }
    make(op) {
        this.made.push(op);
        return Promise.resolve(this.makes.shift() ?? { ok: true, message: '', id: `sch-${this.made.length}` });
    }
    cancel() { return Promise.resolve({ ok: true, message: '' }); }
}
const kept = (id, area, extra = {}) => ({ suggestion_id: id, title: `Idea ${id}`, summary: 'A week of short runs.', why: 'You set a goal to run 5k.', area, suggested_at: '2026-11-04T13:30:00Z', state: 'suggested', ...extra });
const answered = outcome => ({ kind: 'answered', result: { result: 'suggestions', outcome } });
const listed = suggestions => answered({ result: 'listed', suggestions });
const flush = () => new Promise(resolve => setImmediate(resolve));
const surface = (answers = []) => {
    const client = new FakeClient(answers);
    const schedules = new FakeSchedules();
    let changes = 0;
    let serial = 0;
    const live = new LiveIdeas(client, schedules, () => { changes++; }, () => `r${++serial}`, () => ZONE, () => new Date('2026-11-04T13:00:00Z'));
    return { client, schedules, live, changes: () => changes };
};
test('a suggestion reads as the agent\'s words, in its area\'s section, with its picture', () => {
    assert.deepEqual(ideaOf(kept('idea-1', 'health')), { id: 'idea-1', section: 'Health & fitness', title: 'Idea idea-1', summary: 'A week of short runs.', domain: 'Health & fitness', icon: 'run', rationale: 'You set a goal to run 5k.', signal: ideaOf(kept('idea-1', 'health')).signal, items: [], state: 'suggested', outcome: '', artifactId: null });
    assert.match(ideaOf(kept('i', 'career')).signal, /These are its words/);
    assert.equal(ideaOf(kept('i', 'career', { state: 'started' })).state, 'activated');
    assert.equal(ideaOf(kept('i', 'career', { state: 'dismissed' })).state, 'dismissed');
    const areas = ['career', 'devices', 'creation', 'family', 'finance', 'health', 'learning', 'productivity', 'relationships', 'shopping'];
    assert.equal(new Set(areas.map(area => ideaOf(kept('i', area)).domain)).size, areas.length, 'each area its own section');
    // By area, in the page's order; the newest first within each.
    assert.deepEqual(ordered([kept('a', 'shopping'), kept('b', 'health'), kept('c', 'productivity'), kept('d', 'health')]).map(one => one.suggestion_id), ['c', 'b', 'd', 'a']);
});
test('an idea says the goal it came from, and why it left For you when it did', () => {
    const goal = { kind: 'goal', id: '0a1b', title: 'Run 5k by June' };
    const plain = ideaOf(kept('idea-1', 'health'));
    const sourced = ideaOf(kept('idea-1', 'health', { source: goal, needs: ['room'], run: '3f9c' }));
    assert.equal(sourced.signal, `From your goal “Run 5k by June”. ${plain.signal}`);
    assert.doesNotMatch(plain.signal, /From your goal/);
    assert.equal(sourced.outcome, '');
    const stale = ideaOf(kept('i', 'career', { state: 'stale' }));
    assert.deepEqual([stale.state, stale.outcome], ['dismissed', 'It waited two weeks without being started. Restore it if you still want it.']);
    const gone = ideaOf(kept('i', 'career', { state: 'source_gone', source: goal }));
    assert.deepEqual([gone.state, gone.outcome], ['dismissed', 'The goal it came from is closed or gone.']);
    for (const [state, shown] of [['suggested', 'suggested'], ['started', 'activated'], ['dismissed', 'dismissed']])
        assert.deepEqual([ideaOf(kept('i', 'career', { state })).state, ideaOf(kept('i', 'career', { state })).outcome], [shown, ''], state);
});
test('the page is read each time the connection opens, and is unread until then', async () => {
    const { client, live, schedules, changes } = surface([listed([kept('idea-1', 'health')]), listed([])]);
    assert.deepEqual(live.current(), { kind: 'unread' });
    client.set('ready');
    client.set('ready');
    await flush();
    assert.deepEqual(client.sent, [{ action: 'manage_suggestions', op: { op: 'list' } }], 'once while it stays open');
    assert.deepEqual(live.ideas().map(idea => idea.id), ['idea-1']);
    assert.deepEqual(live.current(), { kind: 'read', suggesting: false });
    assert.equal(changes(), 1);
    // A one-off run for the page is out: ideas are being thought of. A daily one isn't that.
    schedules.list = [{ id: 'sch-1', channel: IDEAS_PAGE, when: { every: 'cron', expr: '0 9 * * 1', tz: ZONE } }, { id: 'sch-2', channel: 'local', when: { every: 'once', at: 'x', tz: ZONE } }];
    assert.equal(live.current().suggesting, false);
    schedules.list.push({ id: 'sch-3', channel: IDEAS_PAGE, when: { every: 'once', at: 'x', tz: ZONE } });
    assert.equal(live.current().suggesting, true);
    client.set('offline');
    client.set('ready');
    await flush();
    assert.equal(client.sent.length, 2, 'again after it reopens');
    assert.deepEqual(live.ideas(), []);
});
test('a run that ends reads the page again; one that only started does not', async () => {
    const { client, live } = surface([listed([]), listed([])]);
    live.observe({ outcome: 'fired', schedule_id: 'sch-1' });
    live.observe({ outcome: 'cancelled', schedule_id: 'sch-1' });
    assert.equal(client.sent.length, 0);
    live.observe({ outcome: 'completed', schedule_id: 'sch-1' });
    live.observe({ outcome: 'failed', schedule_id: 'sch-1', detail: '' });
    await flush();
    assert.equal(client.sent.length, 2);
});
test('a run that suggested an idea reads the page again, in a chat or not; a burst of them reads it once more; any other record does not', async () => {
    const { client, live } = surface([listed([kept('idea-1', 'health')]), listed([kept('idea-1', 'health'), kept('idea-2', 'career')]), listed([])]);
    const used = (tool, done) => ({ tool, done, asked: 0, refused: 0, failed: 0 });
    const summary = (chat, tools) => ({ event: 'run_summary', event_id: 'e', ts: 't', run_id: 'r', cell_id: 'c', chat, tools, model_calls: 2, model_calls_refused: 0, peak_input_tokens: 1, output_tokens: 1 });
    // A run that used something else, one whose idea wasn't kept, and a record that is no summary.
    live.observeRun(summary(true, [used('room', 3)]));
    live.observeRun(summary(true, [{ ...used('idea_suggest', 0), refused: 1 }]));
    live.observeRun({ event: 'tool_call', event_id: 'e', ts: 't', run_id: 'r', tool: 'idea_suggest' });
    await flush();
    assert.deepEqual(client.sent, []);
    // A chat's run that suggested four, then two more records before the page was read.
    live.observeRun(summary(true, [used('memory_recall', 2), used('idea_suggest', 4)]));
    live.observeRun(summary(false, [used('idea_suggest', 1)]));
    live.observeRun(summary(true, [used('idea_suggest', 1)]));
    assert.equal(client.sent.length, 1, 'one read at a time');
    await flush();
    assert.deepEqual(client.sent, [{ action: 'manage_suggestions', op: { op: 'list' } }, { action: 'manage_suggestions', op: { op: 'list' } }], 'once for the first, once for all that came meanwhile');
    assert.deepEqual(live.ideas().map(idea => idea.id), ['idea-2', 'idea-1']);
    // The burst is over: the next one reads again.
    live.observeRun(summary(true, [used('idea_suggest', 1)]));
    await flush();
    assert.equal(client.sent.length, 3);
    assert.deepEqual(live.ideas(), []);
});
test('a read that fails says why, and a later one recovers', async () => {
    const { live } = surface([
        { kind: 'unsent', detail: 'Not connected.' },
        { kind: 'answered', result: { result: 'refused', reason: 'not_granted' } },
        { kind: 'answered', result: { result: 'feed', posts: [] } },
        answered({ result: 'off' }),
        answered({ result: 'failed' }),
        answered({ result: 'not_found' }),
        listed([kept('idea-1', 'health')]),
    ]);
    const said = [];
    for (let read = 0; read < 6; read++) {
        await live.refresh();
        assert.equal(live.current().kind, 'failed');
        said.push(live.current().detail);
    }
    assert.deepEqual(said, ['Not connected.', 'This connection may not read the Ideas page.', 'Hatchling answered with something else.', 'Hatchling keeps no Ideas page here: scheduled work is off.', 'Hatchling couldn’t read or change the Ideas page. `hatchling-broker doctor`, run as root, checks the install.', 'Hatchling answered with something else.']);
    await live.refresh();
    assert.equal(live.current().kind, 'read');
});
test('the owner starts, sets aside and brings back an idea, and the page is read again', async () => {
    const changed = suggestion => answered({ result: 'changed', suggestion });
    const { client, live } = surface([
        changed(kept('idea-1', 'health', { state: 'started' })), listed([kept('idea-1', 'health', { state: 'started' })]),
        changed(kept('idea-1', 'health', { state: 'dismissed' })), listed([]),
        changed(kept('idea-1', 'health')), listed([kept('idea-1', 'health')]),
        answered({ result: 'not_found' }),
        answered({ result: 'failed' }),
        { kind: 'unknown', detail: 'No answer came.' },
    ]);
    assert.deepEqual(await live.act('idea-1', 'start'), { ok: true, message: 'It’s in your chat box: read it, change it, and send it when you’re ready.' });
    assert.deepEqual(client.sent[0], { action: 'manage_suggestions', op: { op: 'start', id: 'idea-1' } });
    assert.equal(live.ideas()[0].state, 'activated');
    assert.deepEqual(await live.act('idea-1', 'dismiss'), { ok: true, message: 'Set aside.' });
    assert.deepEqual(await live.act('idea-1', 'restore'), { ok: true, message: 'It’s back in For you.' });
    assert.deepEqual(await live.act('idea-9', 'start'), { ok: false, message: 'This idea is gone.' });
    assert.match((await live.act('idea-1', 'start')).message, /couldn’t read or change the Ideas page/);
    assert.deepEqual(await live.act('idea-1', 'start'), { ok: false, message: 'No answer came.' });
});
test('an idea whose goal is still closed stays as it is when brought back, and the owner is told', async () => {
    const gone = kept('idea-1', 'health', { state: 'source_gone', source: { kind: 'goal', id: '0a1b', title: 'Run 5k by June' } });
    const back = kept('idea-2', 'health', { state: 'suggested' });
    const { client, live } = surface([answered({ result: 'changed', suggestion: gone }), listed([gone, back]), answered({ result: 'changed', suggestion: back }), listed([gone, back])]);
    assert.deepEqual(await live.act('idea-1', 'restore'), { ok: false, message: 'Its goal is still closed, so it stays here.' });
    assert.deepEqual(live.ideas().map(idea => [idea.id, idea.state]), [['idea-1', 'dismissed'], ['idea-2', 'suggested']], 'the page was read again all the same');
    // One that did come back says so; and setting aside one that stays gone isn't taken for a restore.
    assert.deepEqual(await live.act('idea-2', 'restore'), { ok: true, message: 'It’s back in For you.' });
    assert.deepEqual(client.sent.map(action => action.op.op), ['restore', 'list', 'restore', 'list']);
    const other = surface([answered({ result: 'changed', suggestion: gone }), listed([gone])]);
    assert.deepEqual(await other.live.act('idea-1', 'dismiss'), { ok: true, message: 'Set aside.' });
});
test('asking for ideas now is a one-off run for the page, a few seconds out; it reads and suggests, nothing else', async () => {
    const { live, schedules, changes } = surface();
    assert.deepEqual(await live.suggestNow(), { ok: true, message: 'Thinking of ideas for you. They appear here when they’re ready, in a minute or two.' });
    assert.deepEqual(schedules.made, [{ op: 'create', when: { every: 'once', at: '2026-11-04T08:00:15', tz: ZONE }, task: 'Ideas for me, from my goals and what I’ve asked you to remember.', grants: [MODEL, READ_GOALS, RECALL, SUGGEST], channel: 'ideas' }]);
    assert.equal(changes(), 1);
    schedules.makes.push({ ok: false, message: 'Hatchling keeps as many schedules as it can. Remove one first.' });
    assert.deepEqual(await live.suggestNow(), { ok: false, message: 'Hatchling keeps as many schedules as it can. Remove one first.' });
    assert.equal(changes(), 1, 'nothing changed');
});
test('the browser\'s own zone and clock, unless a test says', async () => {
    const schedules = new FakeSchedules();
    const live = new LiveIdeas(new FakeClient(), schedules, () => { }, () => 'r1');
    assert.equal((await live.suggestNow()).ok, true);
    assert.equal(schedules.made[0].when.tz, Intl.DateTimeFormat().resolvedOptions().timeZone);
    assert.match(schedules.made[0].when.at, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}$/);
});
