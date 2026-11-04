// the live adapter, end to end over a fake transport, with the typed
// fixtures in src/adapters/live/fixtures.ts. Untrusted rendering goes through
// the real markdown renderer on a minimal DOM.
import test from 'node:test';
import assert from 'node:assert/strict';
import { LiveAdapter } from '../../dist/app/adapters/live/adapter.js';
import { LiveClient } from '../../dist/app/adapters/live/client.js';
import { HOSTILE_TEXT, SAMPLE_EVENTS, SAMPLE_RUN, SAMPLE_WELCOME, sampleActivity, sampleEvent, sampleTime } from '../../dist/app/adapters/live/fixtures.js';
import { conforms } from '../../dist/app/adapters/live/protocol/conform.js';
import { decodeResponse, encodeRequest, isUiRequest, isUiResponse } from '../../dist/app/adapters/live/protocol/decode.js';
import { UI_SCHEMA_VERSION } from '../../dist/app/adapters/live/protocol/generated.js';
import { between, clockTime, count, inertLinks, quoted } from '../../dist/app/adapters/live/text.js';
import { stepOutcome, timelineGroups } from '../../dist/app/features/activity/timeline.js';
import { ApprovalReview } from '../../dist/app/features/approvals/review.js';
import { markdown } from '../../dist/app/components/markdown.js';
import { validThreadCopy } from '../../dist/app/protocol/rules.js';
// The app shows times on the person's clock; these tests read the fixtures'
// UTC times on a UTC clock, wherever they run.
process.env.TZ = 'UTC';
/** The transport a host would supply: frames are JSON text both ways. */
class FakeTransport {
    sent = [];
    connects = 0;
    closes = 0;
    handlers = [];
    connect(handlers) { this.connects++; this.handlers.push(handlers); }
    send(text) { this.sent.push(JSON.parse(text)); }
    close() { this.closes++; }
    get current() { return this.handlers.at(-1); }
    open() { this.current.opened(); }
    deliver(frame) { this.current.received(typeof frame === 'string' ? frame : JSON.stringify(frame)); }
    drop(detail = 'The network went away.') { this.current.closed(detail); }
    last() { return this.sent.at(-1); }
}
const connected = () => {
    const transport = new FakeTransport();
    const adapter = new LiveAdapter(transport, 'test-ui');
    transport.open();
    transport.deliver(SAMPLE_WELCOME);
    return { transport, adapter };
};
const replay = (transport, frames = SAMPLE_EVENTS) => {
    for (const frame of frames)
        transport.deliver(frame);
};
const answer = (transport, request_id, result) => transport.deliver({ type: 'action_result', request_id, result });
const flush = () => new Promise(resolve => setImmediate(resolve));
// The actions the app sent besides what it reads each time it connects: allowed and signed-in sites, schedules, goals, and usage.
const sentActions = transport => transport.sent.filter(frame => frame.type === 'action' && !['read_usage', 'read_feed'].includes(frame.action.action) && !(['manage_suggestions', 'manage_sites', 'manage_profiles', 'manage_sign_ins', 'manage_schedule', 'manage_goals', 'manage_memory'].includes(frame.action.action) && frame.action.op.op === 'list') && frame.action.action !== 'list_decisions');
const thread = adapter => adapter.getSnapshot().threads[0];
const lane = (adapter, id) => thread(adapter).lanes.find(item => item.id === id);
const confirmation = (seq, revision, summary = 'Post the sample summary?') => sampleEvent(seq, seq, { kind: 'confirmation', confirmation_id: 'confirm-1', revision, chat_id: 'main', summary, expires_at: null, digest: `digest-${revision}`, fields: [{ label: 'Operation', value: summary, source: 'broker' }, { label: 'Text', value: 'Sample text.', source: 'agent' }] });
const decidedEvent = (seq, decision, revision = 2) => sampleEvent(seq, seq, { kind: 'confirmation_decided', confirmation_id: 'confirm-1', revision, decision });
// ---- A minimal DOM: enough for h() and markdown(), and nothing that parses HTML.
class FakeNode {
    childNodes = [];
    append(...nodes) { this.childNodes.push(...nodes); }
}
class FakeText extends FakeNode {
    constructor(data) { super(); this.data = data; }
}
class FakeElement extends FakeNode {
    attributes = new Map();
    constructor(tag) { super(); this.tagName = tag; }
    setAttribute(name, value) { this.attributes.set(name, value); }
    addEventListener() { }
}
globalThis.Node = FakeNode;
globalThis.document = { createElement: tag => new FakeElement(tag), createTextNode: data => new FakeText(data), createDocumentFragment: () => new FakeNode() };
const walk = (node, out = { tags: [], text: '', hrefs: [] }) => {
    if (node instanceof FakeText)
        out.text += node.data;
    if (node instanceof FakeElement) {
        out.tags.push(node.tagName);
        if (node.attributes.has('href'))
            out.hrefs.push(node.attributes.get('href'));
    }
    for (const child of node.childNodes)
        walk(child, out);
    return out;
};
/** What the renderer may build from text: formatting, never controls or embeds. */
const FORMATTING = new Set(['div', 'p', 'blockquote', 'strong', 'em', 'code', 'pre', 'ul', 'ol', 'li', 'table', 'thead', 'tbody', 'tr', 'th', 'td', 'h1', 'h2', 'h3']);
test('the handshake says hello, then subscribes from the start, then the connection is ready', () => {
    const transport = new FakeTransport();
    const adapter = new LiveAdapter(transport, 'test-ui');
    assert.equal(transport.connects, 1);
    assert.equal(adapter.chatSession.getSnapshot().connection.kind, 'reconnecting');
    transport.open();
    assert.deepEqual(transport.last(), { type: 'hello', schema_version: UI_SCHEMA_VERSION, client: 'test-ui' });
    transport.deliver(SAMPLE_WELCOME);
    assert.deepEqual(transport.last(), { type: 'subscribe', after_seq: null });
    assert.deepEqual(adapter.chatSession.getSnapshot(), { connection: { kind: 'ready', detail: '' }, histories: {} });
    assert.equal(adapter.mode, 'live');
    assert.equal(new LiveAdapter(new FakeTransport()).getSnapshot().mode, 'live');
});
test('version skew or a stranger on the line ends the connection with a sentence a person can act on', () => {
    const cases = [
        [{ ...SAMPLE_WELCOME, schema_version: UI_SCHEMA_VERSION + 1 }, `broker v${UI_SCHEMA_VERSION + 1} needs frontend ≥ v${UI_SCHEMA_VERSION + 1}`],
        [{ type: 'incompatible', frontend: UI_SCHEMA_VERSION, backend: 0, message: 'restart the broker' }, 'restart the broker'],
        [SAMPLE_EVENTS[0], 'before the handshake'],
    ];
    for (const [frame, words] of cases) {
        const transport = new FakeTransport();
        const adapter = new LiveAdapter(transport);
        transport.open();
        transport.deliver(frame);
        const connection = adapter.chatSession.getSnapshot().connection;
        assert.equal(connection.kind, 'unavailable');
        assert.match(connection.detail, new RegExp(words));
        assert.equal(transport.closes, 1);
        assert.equal(transport.sent.length, 1, 'nothing after hello');
        assert.equal(adapter.getSnapshot().messages.length, 0);
    }
});
test('an unreadable frame ends the connection loudly instead of being skipped', () => {
    const bad = [
        'not json',
        JSON.stringify({ type: 'event', seq: 99, ts: sampleTime(1), body: { kind: 'activity', state: 'pending', event: { event: 'subagent_spawned' } } }),
        JSON.stringify({ ...SAMPLE_EVENTS[0], seq: -1 }),
        JSON.stringify(SAMPLE_WELCOME),
    ];
    for (const frame of bad) {
        const { transport, adapter } = connected();
        transport.deliver(frame);
        const connection = adapter.chatSession.getSnapshot().connection;
        assert.equal(connection.kind, 'unavailable', frame);
        assert.match(connection.detail, frame === bad[3] ? /repeated its handshake/ : /cannot read/);
        assert.equal(transport.closes, 1);
        assert.equal(adapter.getSnapshot().messages.length, 0);
    }
});
test('activity: lifecycle narration, subtitles and status from the activity-monitor event contract, without ids or hashes', () => {
    const { transport, adapter } = connected();
    replay(transport, SAMPLE_EVENTS.slice(0, 7));
    let feed = thread(adapter);
    assert.equal(feed.id, SAMPLE_RUN);
    assert.equal(feed.state, 'running');
    assert.equal(adapter.getSnapshot().turn, 'idle', 'the agent status is what the broker reports, never inferred from activity ');
    assert.equal(feed.statusTitle, 'Subagents working');
    assert.equal(feed.subtitle, '4 subagents started · 4 still working');
    assert.equal(feed.summary, '4 subagents are still working.');
    assert.ok(validThreadCopy(feed.name, feed.subtitle, feed.statusTitle));
    assert.deepEqual(feed.lanes.map(item => [item.id, item.parentId, item.name]), [[SAMPLE_RUN, null, 'Main agent'], ['child-a', SAMPLE_RUN, 'Subagent 1'], ['child-b', SAMPLE_RUN, 'Subagent 2'], ['child-c', SAMPLE_RUN, 'Subagent 3'], ['child-d', SAMPLE_RUN, 'Subagent 4']]);
    // A subagent at work is a pending step with no verdict, never an early success.
    assert.deepEqual(feed.lanes.slice(1).map(item => item.rows.length), [0, 0, 0, 0]);
    const [working, waiting] = feed.pendingSteps;
    assert.deepEqual([working.title, working.subtitle, working.laneId], ['Working on its task', '12 of 40 steps used', 'child-a']);
    assert.equal(working.report, 'The latest progress it reported:\n\n> Read 3 of 5 sample files.');
    assert.deepEqual([waiting.subtitle, waiting.report], ['0 of 1 step used', 'No progress has been reported yet.']);
    const dispatch = lane(adapter, SAMPLE_RUN).rows;
    assert.deepEqual(dispatch.map(row => [row.title, row.subtitle, row.status, row.phase]), [
        ['Started subagent 1', 'Gave it a budget of 40 steps and 2 narrowed capabilities', 'success', 'dispatch'],
        ['Started subagent 2', 'Gave it a budget of 1 step and 1 narrowed capability', 'success', 'dispatch'],
        ['Started subagent 3', 'Gave it a budget of 10 steps and 0 narrowed capabilities', 'success', 'dispatch'],
        ['Started subagent 4', 'Gave it a budget of 30 steps and 1 narrowed capability', 'success', 'dispatch'],
    ]);
    replay(transport, SAMPLE_EVENTS.slice(7));
    feed = thread(adapter);
    assert.equal(feed.state, 'completed');
    assert.equal(feed.verdict, null, 'the run\'s own outcome is not in the contract, so none is claimed');
    assert.equal(adapter.getSnapshot().turn, 'idle');
    assert.deepEqual([feed.statusTitle, feed.subtitle, feed.pendingSteps.length], ['Subagents ended', '4 subagents started · all have ended', 0]);
    assert.ok(validThreadCopy(feed.name, feed.subtitle, feed.statusTitle));
    assert.equal(feed.summary, 'Of 4 subagents: 1 finished, 1 finished part of their task, 1 were stopped, 1 failed. The run\'s own outcome is not part of the activity record yet.');
    assert.equal(feed.time, '09:00');
    const ends = feed.lanes.slice(1).map(item => item.rows[0]);
    assert.deepEqual(ends.map(row => [row.title, row.subtitle, row.status, row.outputFailed, row.phase]), [
        ['Finished its task', 'Completed its brief in 31 steps', 'success', false, 'finish'],
        ['Stopped by the main agent', 'Stopped after 1 step', 'interrupted', false, 'finish'],
        ['Used up its step budget', 'Failed after 10 steps', 'failed', true, 'finish'],
        ['Finished part of its task', 'Completed part of its brief in 30 steps', 'partial', false, 'finish'],
    ]);
    assert.equal(ends[0].durationMs, 6000);
    assert.match(ends[1].report, /^The main agent stopped Subagent 2 after 1 step\. Being stopped is not a failure of the subagent\. What it had produced before it stopped was kept\./);
    assert.match(ends[2].report, /failed on its own: used up its step budget\.[^]*> Stopped at the step limit\./);
    for (const row of [...feed.lanes.flatMap(item => item.rows)])
        for (const words of [row.title, row.subtitle, row.report])
            assert.doesNotMatch(words, /child-|aaaa|run-sample|files\.read/, `${row.id} speaks the user's language`);
});
test('the contract\'s edge cases: an unretained spawn, a contradictory second ending, nothing kept, no detail', () => {
    const { transport, adapter } = connected();
    replay(transport, [
        sampleEvent(1, 1, sampleActivity(1, 'pending', { event: 'subagent_progress', child_id: 'late', note: 'Halfway.', steps_spent: 2 })),
        sampleEvent(2, 3, sampleActivity(3, 'interrupted', { event: 'subagent_killed', child_id: 'late', reason: 'Enough.', partial_result_hash: null, steps_spent: 2 })),
        sampleEvent(3, 4, sampleActivity(4, 'success', { event: 'subagent_finished', child_id: 'late', result_hash: 'b'.repeat(64), steps_spent: 3, verdict: 'ok' })),
        sampleEvent(4, 5, sampleActivity(5, 'pending', { event: 'subagent_progress', child_id: 'late', note: 'Too late.', steps_spent: 4 })),
        sampleEvent(5, 6, sampleActivity(6, 'failed', { event: 'subagent_failed', child_id: 'other', failure: 'timeout', detail: ' ', steps_spent: 0 })),
        sampleEvent(6, 7, sampleActivity(7, 'failed', { event: 'subagent_failed', child_id: 'third', failure: 'cell_failed', detail: 'Exited.', steps_spent: 1 })),
    ]);
    const feed = thread(adapter);
    assert.equal(lane(adapter, SAMPLE_RUN).rows.length, 0, 'no spawn, no dispatch row');
    const [late, other, third] = feed.lanes.slice(1).map(item => item.rows[0]);
    assert.equal(late.status, 'interrupted', 'the first ending stands');
    assert.equal(late.subtitle, 'Stopped after 2 steps');
    assert.equal(late.durationMs, null);
    assert.match(late.report, /It had produced nothing when it stopped\./);
    assert.equal(other.title, 'Ran out of time');
    assert.doesNotMatch(other.report, /What it reported/);
    assert.equal(third.title, 'Its sandbox stopped working');
    // A spawn that was not retained: the pending step still says what it can.
    const fresh = connected();
    fresh.transport.deliver(sampleEvent(1, 1, sampleActivity(1, 'pending', { event: 'subagent_progress', child_id: 'late', note: 'Halfway.', steps_spent: 2 })));
    assert.equal(thread(fresh.adapter).pendingSteps[0].subtitle, '2 steps used');
    // A second run is a second thread, newest first.
    fresh.transport.deliver(sampleEvent(2, 2, { kind: 'activity', state: 'pending', event: { event: 'subagent_progress', child_id: 'x', note: 'n', steps_spent: 1, event_id: 'e', run_id: 'run-sample-2', ts: sampleTime(2) } }));
    assert.deepEqual(fresh.adapter.getSnapshot().threads.map(item => item.id), ['run-sample-2', SAMPLE_RUN]);
});
test('activity: each thing asked in the chat is a task, answered by the agent\'s next reply; the app\'s own clicks are not activity', () => {
    const { transport, adapter } = connected();
    const chat = (seq, author, id, text, chat_id = 'local') => sampleEvent(seq, seq, { kind: 'chat_message', chat_id, message_id: id, author, text });
    const ownClick = seq => sampleEvent(seq, seq, { kind: 'activity', state: 'success', event: { event: 'api_call', event_id: `api-${seq}`, run_id: 'api', ts: sampleTime(seq), caller: 'ui, uid 1000', capabilities: ['channel-send:local'], request: 'send_message', outcome: 'served' } });
    const long = 'Give me three short ideas for a relaxing Sunday, and make each one something I can do at home.';
    replay(transport, [
        chat(1, 'agent', 'm0', 'A scheduled reminder, with nothing asked.'),
        ownClick(2),
        chat(3, 'user', 'm1', long),
        chat(4, 'agent', 'm2', 'Sure. Here are three:\n1. Read in the sun.'),
        chat(5, 'user', 'm3', 'And one more?'),
        chat(6, 'user', 'm1', 'a replay of the first ask'),
        chat(7, 'user', 's1', 'in a side chat', 'side-1'),
        ownClick(8),
    ]);
    const threads = adapter.getSnapshot().threads;
    assert.deepEqual(threads.map(thread => thread.id), ['m3', 'm1'], 'newest first; no thread for the app\'s clicks, a side chat or a replay');
    const [waiting, answered] = threads;
    assert.deepEqual([answered.name, answered.subtitle, answered.state, answered.statusTitle, answered.chatId], ['Give me three short ideas for a relaxing…', 'Sure. Here are three:', 'completed', 'Answered', 'main']);
    assert.equal(answered.lanes[0].rows[0].report, 'The agent answered:\n\n> Sure. Here are three:\n> 1. Read in the sun.', 'the answer is quoted as untrusted text');
    assert.equal(answered.time, '09:00');
    assert.deepEqual([waiting.name, waiting.subtitle, waiting.state, waiting.lanes[0].rows.length, waiting.endedAt], ['And one more?', 'The agent is working on it.', 'running', 0, null]);
    assert.ok(validThreadCopy(answered.name, answered.subtitle, answered.statusTitle));
    // The next reply answers the waiting task; a reply after it answers nothing.
    replay(transport, [chat(9, 'agent', 'm4', 'A walk.'), chat(10, 'agent', 'm5', 'Unasked.')]);
    const [now] = adapter.getSnapshot().threads;
    assert.deepEqual([now.id, now.subtitle, now.state], ['m3', 'The agent answered: A walk.', 'completed']);
    assert.ok(validThreadCopy(now.name, now.subtitle, now.statusTitle));
    assert.ok(validThreadCopy(waiting.name, waiting.subtitle, waiting.statusTitle));
    assert.equal(adapter.getSnapshot().threads.length, 2);
    replay(transport, [chat(11, 'user', 'm6', 'Plan my week'), chat(12, 'agent', 'm7', 'Monday: rest, Tuesday: write, Wednesday: call Sam, Thursday: shop, Friday: run.'), chat(13, 'user', 'm8', 'Thanks'), chat(14, 'agent', 'm9', 'You are very welcome.')]);
    const [thanks, plan] = adapter.getSnapshot().threads;
    assert.equal(plan.subtitle, 'Monday: rest, Tuesday: write, Wednesday: call Sam, Thursday: shop, Friday:…');
    assert.equal(thanks.subtitle, 'You are very welcome.', 'four words are enough');
    for (const thread of [plan, thanks])
        assert.ok(validThreadCopy(thread.name, thread.subtitle, thread.statusTitle), thread.subtitle);
});
test('local API calls are a thread of their own: one lane per caller, one row per call, never its content', () => {
    const { transport, adapter } = connected();
    const call = (seq, caller, request, outcome, capabilities) => sampleEvent(seq, seq, { kind: 'activity', state: outcome === 'served' ? 'success' : 'failed', event: { event: 'api_call', event_id: `api-${seq}`, run_id: 'api', ts: sampleTime(seq), caller, capabilities, request, outcome } });
    replay(transport, [
        sampleEvent(1, 1, sampleActivity(1, 'pending', { event: 'subagent_progress', child_id: 'late', note: 'Halfway.', steps_spent: 2 })),
        call(2, 'uid 1000', 'hello', 'served', ['goals:read']),
        call(3, 'uid 1000', 'manage_schedule', 'served', ['goals:read']),
        call(4, 'token caller', 'send_message', 'refused', []),
        call(5, 'uid 1000', 'invoke_skill', 'refused', ['goals:read']),
    ]);
    const threads = adapter.getSnapshot().threads;
    assert.deepEqual(threads.map(item => item.id), ['api', SAMPLE_RUN], 'newest first, by first appearance');
    const api = threads[0];
    assert.deepEqual([api.name, api.subtitle, api.statusTitle, api.state, api.verdict], ['Local API calls', '4 calls · 2 served, 2 refused', 'API calls recorded', 'completed', null]);
    assert.ok(validThreadCopy(api.name, api.subtitle, api.statusTitle));
    assert.equal(api.summary, '4 calls from 2 callers: 2 served, 2 refused.');
    assert.equal(api.time, '09:00');
    assert.deepEqual(api.pendingSteps, []);
    assert.deepEqual(api.lanes.map(item => [item.id, item.name, item.parentId, item.rows.length]), [['api:0', 'uid 1000', null, 3], ['api:1', 'token caller', null, 1]]);
    assert.deepEqual(api.lanes.flatMap(item => item.rows).map(row => [row.title, row.subtitle, row.status, row.outputFailed, row.icon, row.phase]), [
        ['Connected', 'Served, with 1 capability', 'success', false, 'check', 'action'],
        ['Changed a schedule', 'Served, with 1 capability', 'success', false, 'check', 'action'],
        ['Ran a skill', 'Refused, with 1 capability', 'failed', true, 'warning', 'action'],
        ['Sent a message', 'Refused, with 0 capabilities', 'failed', true, 'warning', 'action'],
    ]);
    const [hello, , skill] = api.lanes[0].rows;
    assert.equal(hello.report, 'uid 1000: connected. The API served it, with the caller\'s 1 capability.');
    assert.equal(skill.report, 'uid 1000: ran a skill. The API refused it before any part of Hatchling acted on it. The caller held 1 capability.');
    assert.deepEqual([hello.id, hello.laneId, hello.startedAt, hello.endedAt, hello.durationMs], ['api-2', 'api:0', sampleTime(2), sampleTime(2), null]);
    // Every kind of request has its own words.
    const kinds = ['hello', 'subscribe', 'send_message', 'new_conversation', 'decide', 'manage_schedule', 'invoke_skill', 'open_artifact', 'share_artifact', 'attach_artifact', 'save_file', 'take_control', 'return_control', 'stop_browser'];
    const every = connected();
    replay(every.transport, kinds.map((request, index) => call(index + 1, 'uid 1000', request, 'served', [])));
    const titles = every.adapter.getSnapshot().threads[0].lanes[0].rows.map(row => row.title);
    assert.equal(new Set(titles).size, kinds.length);
    assert.equal(every.adapter.getSnapshot().threads[0].subtitle, '14 calls · 14 served, 0 refused');
});
test('coding-harness runs are a thread of their own: one lane per harness, one row per run, never a hash', () => {
    const { transport, adapter } = connected();
    const states = { done: 'success', needs_clarification: 'partial', failed: 'failed', timed_out: 'failed', refused: 'failed' };
    const run = (seq, harness, outcome, files) => sampleEvent(seq, seq, { kind: 'activity', state: states[outcome], event: { event: 'harness_run', event_id: `harness-${seq}`, run_id: 'harness', ts: sampleTime(seq), harness, binary_sha256: 'c'.repeat(64), brief_sha256: 'd'.repeat(64), outcome, files_changed: files } });
    replay(transport, [
        sampleEvent(1, 1, sampleActivity(1, 'pending', { event: 'subagent_progress', child_id: 'late', note: 'Halfway.', steps_spent: 2 })),
        run(2, 'claude-code', 'done', 2),
        run(3, 'codex-cli', 'needs_clarification', 0),
        run(4, 'claude-code', 'failed', 1),
        run(5, 'claude-code', 'timed_out', 0),
        run(6, 'codex-cli', 'refused', 0),
        run(7, 'aider', 'done', 1),
    ]);
    const threads = adapter.getSnapshot().threads;
    assert.deepEqual(threads.map(item => item.id), ['harness', SAMPLE_RUN]);
    const harness = threads[0];
    assert.deepEqual([harness.name, harness.subtitle, harness.statusTitle, harness.state, harness.verdict], ['Coding harness runs', '6 runs · 2 finished, 4 did not', 'Harness runs recorded', 'completed', null]);
    assert.ok(validThreadCopy(harness.name, harness.subtitle, harness.statusTitle));
    assert.equal(harness.summary, '6 runs of 3 harnesses: 2 finished, 4 did not.');
    assert.equal(harness.time, '09:00');
    assert.deepEqual(harness.pendingSteps, []);
    assert.deepEqual(harness.lanes.map(item => [item.id, item.name, item.parentId, item.rows.length]), [['harness:0', 'Claude Code', null, 3], ['harness:1', 'Codex CLI', null, 2], ['harness:2', 'aider', null, 1]]);
    assert.deepEqual(harness.lanes.flatMap(item => item.rows).map(row => [row.title, row.subtitle, row.status, row.outputFailed, row.icon, row.phase]), [
        ['Finished its task', 'Changed 2 files', 'success', false, 'check', 'action'],
        ['Did not finish', 'Changed 1 file', 'failed', true, 'warning', 'action'],
        ['Ran out of time', 'Changed 0 files', 'failed', true, 'clock', 'action'],
        ['Asked a question', 'Changed 0 files', 'partial', false, 'warning', 'action'],
        ['Refused before it started', 'Changed 0 files', 'failed', true, 'warning', 'action'],
        ['Finished its task', 'Changed 1 file', 'success', false, 'check', 'action'],
    ]);
    const [first] = harness.lanes[0].rows;
    assert.equal(first.report, 'Claude Code finished its task, changing 2 files. Hatchling pinned the exact program that ran, and keeps a hash of the brief it was given. Any changes are a patch for review, not applied.');
    assert.deepEqual([first.id, first.laneId, first.startedAt, first.endedAt, first.durationMs], ['harness-2', 'harness:0', sampleTime(2), sampleTime(2), null]);
    assert.match(harness.lanes[1].rows[0].report, /^Codex CLI stopped to ask a question, which the next brief answers,/);
    for (const row of harness.lanes.flatMap(item => item.rows))
        for (const words of [row.title, row.subtitle, row.report])
            assert.doesNotMatch(words, /cccc|dddd|harness-\d/, `${row.id} shows no hash or id`);
});
test('a run\'s trail is a thread of its own: a row per tool by how its uses ended, and the model\'s calls', () => {
    const { transport, adapter } = connected();
    const tally = (tool, done, asked, refused, failed) => ({ tool, done, asked, refused, failed });
    const summary = (seq, chat, tools, calls, refused = 0) => sampleEvent(seq, seq, { kind: 'activity', state: 'success', event: { event: 'run_summary', event_id: `session-${seq}:summary`, run_id: `session-${seq}`, ts: sampleTime(seq), cell_id: `run-${seq}`, chat, tools, model_calls: calls, model_calls_refused: refused, peak_input_tokens: 5432, output_tokens: 310 } });
    const tools = [tally('goals_read', 1, 0, 0, 0), tally('memory_propose', 0, 1, 1, 0), tally('web_page', 0, 0, 1, 1)];
    replay(transport, [summary(1, true, tools, 3), summary(2, false, [], 1, 2), summary(1, true, tools, 3)]);
    const threads = adapter.getSnapshot().threads;
    assert.deepEqual(threads.map(item => item.id), ['session-2:summary', 'session-1:summary'], 'a summary seen again changes nothing');
    const [background, chat] = threads;
    assert.deepEqual([chat.name, chat.subtitle, chat.statusTitle, chat.state, chat.chatId], ['What the chat run did', '3 tools · 3 model calls', 'Trail recorded', 'completed', 'main']);
    assert.ok(validThreadCopy(chat.name, chat.subtitle, chat.statusTitle));
    assert.equal(chat.summary, 'Used 3 tools, as Hatchling served them: read your goals, asked to remember something, read a web page.');
    assert.deepEqual(chat.lanes[0].rows.map(row => [row.title, row.subtitle, row.status, row.outputFailed, row.icon]), [
        ['Read your goals', '1 done', 'success', false, 'check'],
        ['Asked to remember something', '1 asked you · 1 refused', 'partial', false, 'warning'],
        ['Read a web page', '1 refused · 1 failed', 'failed', true, 'warning'],
        ['Called the model', '3 model calls · largest prompt 5.4K tokens', 'success', false, 'check'],
    ]);
    assert.deepEqual(timelineGroups(chat).flatMap(group => group.rows.map(row => row.title)), ['Read your goals', 'Asked to remember something', 'Read a web page', 'Called the model'], 'each row is its own step on the timeline');
    assert.equal(chat.lanes[0].rows[0].report, 'Read your goals: 1 done. Hatchling counted each use as it answered it, never from what the agent said.');
    assert.equal(chat.lanes[0].rows[3].report, 'The run made 3 model calls, as the model server reported them. Its largest prompt was 5432 tokens, which is how full the model\'s context got; it wrote 310 tokens in all.');
    assert.deepEqual([background.name, background.subtitle, 'chatId' in background], ['What background work did', '0 tools · 1 model call', false]);
    assert.ok(validThreadCopy(background.name, background.subtitle, background.statusTitle));
    assert.equal(background.summary, 'Used no tools: everything in its answer came from the model alone.');
    const model = background.lanes[0].rows[0];
    assert.deepEqual([model.subtitle, model.status, model.icon], ['1 model call · largest prompt 5.4K tokens · 2 refused at your usage limit', 'partial', 'warning']);
    assert.match(model.report, /Hatchling refused 2 calls past a usage limit you set\.$/);
    const none = connected();
    replay(none.transport, [summary(3, false, [], 0, 1)]);
    const refusedOnly = none.adapter.getSnapshot().threads[0].lanes[0].rows[0];
    assert.equal(refusedOnly.subtitle, 'No model call answered · 1 refused at your usage limit');
    assert.equal(refusedOnly.report, 'The run made 0 model calls, as the model server reported them. Hatchling refused 1 call past a usage limit you set.');
});
test('interrupted and partial render distinctly from failure and success; the existing two are unchanged', () => {
    // The pre- rendering, which the e2e suites and screenshots pin.
    assert.deepEqual(stepOutcome('success'), { mark: 'success', icon: 'check', reportIcon: 'check', sentence: null });
    assert.deepEqual(stepOutcome('failed'), { mark: 'failed', icon: 'close', reportIcon: 'warning', sentence: 'This step failed. The overall task has a separate verdict.' });
    const interrupted = stepOutcome('interrupted');
    const partial = stepOutcome('partial');
    assert.deepEqual([interrupted.mark, interrupted.icon, interrupted.reportIcon], ['interrupted', 'stop', 'stop']);
    assert.match(interrupted.sentence, /stopped before it finished\. Being stopped is not a failure/);
    assert.deepEqual([partial.mark, partial.icon], ['partial', 'warning']);
    assert.match(partial.sentence, /only part of its work/);
    const all = ['success', 'failed', 'interrupted', 'partial'].map(stepOutcome);
    assert.equal(new Set(all.map(item => item.mark)).size, 4);
    assert.equal(new Set(all.map(item => item.icon)).size, 4, 'each status has its own timeline mark');
    assert.equal(new Set(all.map(item => item.sentence)).size, 4);
});
test('replays apply once: a duplicate changes nothing, and a reconnect resumes after the last event', async () => {
    const { transport, adapter } = connected();
    replay(transport);
    const before = adapter.getSnapshot();
    replay(transport);
    assert.equal(adapter.getSnapshot(), before, 'duplicates publish nothing');
    transport.drop('Wi-Fi dropped.');
    assert.deepEqual(adapter.chatSession.getSnapshot().connection, { kind: 'offline', detail: 'Wi-Fi dropped.' });
    await adapter.chatSession.reconnect();
    assert.equal(transport.connects, 2);
    await adapter.chatSession.reconnect();
    assert.equal(transport.connects, 2, 'a connection already opening is kept');
    transport.open();
    transport.deliver(SAMPLE_WELCOME);
    assert.deepEqual(transport.last(), { type: 'subscribe', after_seq: SAMPLE_EVENTS.length });
    replay(transport);
    assert.equal(adapter.getSnapshot().messages.length, before.messages.length);
});
test('scheduled work: made with the welcome\'s grants, listed, removed, and what isn\'t there yet said plainly', async () => {
    const transport = new FakeTransport();
    const adapter = new LiveAdapter(transport, 'test-ui');
    const local = { grant: 'channel-send', channel: 'local' };
    transport.open();
    transport.deliver({ ...SAMPLE_WELCOME, grants: [local] });
    const last = op => transport.sent.filter(frame => frame.type === 'action' && frame.action.action === 'manage_schedule' && frame.action.op.op === op).at(-1);
    // Each refresh asks for the schedules, then for what was kept for the owner.
    const lists = async (schedules) => {
        answer(transport, last('list').request_id, { result: 'schedule', outcome: { result: 'listed', schedules } });
        await flush();
        answer(transport, last('list-parked').request_id, { result: 'schedule', outcome: { result: 'parked', parked: [] } });
        await flush();
    };
    await lists([]);
    assert.deepEqual(adapter.getSnapshot().jobs, []);
    const creating = adapter.execute({ type: 'job.create', title: 'Stretch', kind: 'time', schedule: 'weekdays 08:00', recurring: true, source: '', event: '' });
    const create = transport.last();
    assert.deepEqual(create.action, { action: 'manage_schedule', op: { op: 'create', when: { every: 'cron', expr: '0 8 * * 1-5', tz: 'UTC' }, task: 'Stretch', grants: [local], channel: 'local' } });
    answer(transport, create.request_id, { result: 'schedule', outcome: { result: 'created', id: 'sch-1' } });
    await flush();
    await lists([{ id: 'sch-1', kind: 'task', when: create.action.op.when, channel: 'local', grants: [local], next_due: 1790000000, task: 'Stretch' }]);
    assert.deepEqual(await creating, { ok: true, message: 'Scheduled. Its results arrive in this chat.', id: 'sch-1' });
    const [job] = adapter.getSnapshot().jobs;
    assert.deepEqual([job.id, job.title, job.schedule, job.limit], ['sch-1', 'Stretch', 'Weekdays at 08:00 (UTC)', 'May post in this chat']);
    // Watching a source isn't there yet; nothing is sent for it.
    const sent = transport.sent.length;
    const hook = await adapter.execute({ type: 'job.create', title: 'x', kind: 'hook', schedule: 'when it arrives', recurring: false, source: 'GitHub', event: 'New message' });
    assert.equal(hook.ok, false);
    assert.match(hook.message, /at a time instead/);
    for (const command of [{ type: 'job.update', id: 'sch-1', title: 'x', kind: 'time', schedule: 'daily 08:00', recurring: true, source: '', event: '' }, { type: 'job.toggle', id: 'sch-1' }, { type: 'job.run', id: 'sch-1' }]) {
        const result = await adapter.execute(command);
        assert.equal(result.ok, false, command.type);
        assert.match(result.message, /Remove it/, command.type);
    }
    assert.equal(transport.sent.length, sent, 'nothing sent for what isn\'t there');
    const removing = adapter.execute({ type: 'job.delete', id: 'sch-1' });
    const cancel = transport.last();
    assert.deepEqual(cancel.action, { action: 'manage_schedule', op: { op: 'cancel', id: 'sch-1' } });
    answer(transport, cancel.request_id, { result: 'schedule', outcome: { result: 'cancelled' } });
    await flush();
    await lists([]);
    assert.deepEqual(await removing, { ok: true, message: 'Removed. It won’t run again.' });
    assert.deepEqual(adapter.getSnapshot().jobs, []);
    assert.equal(adapter.attention.items().length, 0, 'nothing needs the owner');
});
test('the Feed page: its prompt is a daily schedule that posts there, a post now is a one-off of it, and a run that ends reads it again', async () => {
    const transport = new FakeTransport();
    const adapter = new LiveAdapter(transport, 'test-ui');
    const model = { grant: 'egress', destination: 'localhost:11434' };
    const page = { grant: 'channel-send', channel: 'feed' };
    transport.open();
    transport.deliver({ ...SAMPLE_WELCOME, grants: [model, { grant: 'channel-send', channel: 'local' }] });
    const last = (action, op) => transport.sent.filter(frame => frame.type === 'action' && frame.action.action === action && (op === undefined || frame.action.op.op === op)).at(-1);
    const lists = async (schedules) => {
        answer(transport, last('manage_schedule', 'list').request_id, { result: 'schedule', outcome: { result: 'listed', schedules } });
        await flush();
        answer(transport, last('manage_schedule', 'list-parked').request_id, { result: 'schedule', outcome: { result: 'parked', parked: [] } });
        await flush();
    };
    const made = async (id) => {
        answer(transport, transport.last().request_id, { result: 'schedule', outcome: { result: 'created', id } });
        await flush();
    };
    await lists([]);
    answer(transport, last('read_feed').request_id, { result: 'feed', posts: [{ post_id: 'post-1', written_at: '2026-11-04T13:30:00Z', text: 'Your Tuesday\nTwo goals moved.' }] });
    await flush();
    assert.deepEqual(adapter.getSnapshot().posts.map(post => [post.id, post.title, post.body]), [['post-1', 'Your Tuesday', 'Two goals moved.']]);
    assert.deepEqual(adapter.feed.current(), { kind: 'read', setup: null, writing: false });
    // Until a time is picked, the prompt's post is written at 08:30.
    const saving = adapter.execute({ type: 'feed.prompt', text: 'What moved.' });
    const create = transport.last();
    assert.deepEqual(create.action.op, { op: 'create', when: { every: 'cron', expr: '30 8 * * *', tz: 'UTC' }, task: 'What moved.', grants: [model, page], channel: 'feed' });
    await made('sch-1');
    const daily = { id: 'sch-1', kind: 'task', when: create.action.op.when, channel: 'feed', grants: [model, page], next_due: 1790000000, task: 'What moved.' };
    await lists([daily]);
    assert.deepEqual(await saving, { ok: true, message: 'Saved. Hatchling writes your post every day at 08:30.' });
    assert.equal(adapter.getSnapshot().jobs[0].limit, 'May reach localhost:11434, post on your Feed page');
    // Saved again, it keeps its time, and the new one replaces the old.
    const again = adapter.execute({ type: 'feed.prompt', text: 'Shorter.' });
    assert.equal(transport.last().action.op.when.expr, '30 8 * * *');
    await made('sch-2');
    const shorter = { ...daily, id: 'sch-2', task: 'Shorter.' };
    await lists([daily, shorter]);
    assert.deepEqual(transport.last().action.op, { op: 'cancel', id: 'sch-1' });
    answer(transport, transport.last().request_id, { result: 'schedule', outcome: { result: 'cancelled' } });
    await flush();
    await lists([shorter]);
    assert.equal((await again).ok, true);
    // A post now is a one-off of the daily schedule.
    const writing = adapter.execute({ type: 'feed.generate' });
    const once = transport.last().action.op;
    assert.deepEqual([once.when.every, once.task, once.channel, once.grants], ['once', 'Shorter.', 'feed', [model, page]]);
    await made('sch-3');
    await lists([shorter, { ...shorter, id: 'sch-3', when: once.when }]);
    assert.match((await writing).message, /Writing your post now/);
    assert.equal(adapter.feed.current().writing, true);
    assert.match((await adapter.execute({ type: 'feed.react', id: 'post-1' })).message, /Reactions aren’t kept yet/);
    // A run that only started reads nothing; one that ended reads the lists, then the page.
    const sent = transport.sent.length;
    transport.deliver(sampleEvent(1, 1, { kind: 'schedule', outcome: 'fired', schedule_id: 'sch-3' }));
    await flush();
    assert.equal(transport.sent.length, sent);
    transport.deliver(sampleEvent(2, 2, { kind: 'schedule', outcome: 'completed', schedule_id: 'sch-3' }));
    await flush();
    await lists([shorter]);
    answer(transport, last('read_feed').request_id, { result: 'feed', posts: [{ post_id: 'post-2', written_at: '2026-11-04T13:31:00Z', text: 'Wednesday\nOne goal closed.' }, { post_id: 'post-1', written_at: '2026-11-04T13:30:00Z', text: 'Your Tuesday\nTwo goals moved.' }] });
    await flush();
    assert.deepEqual(adapter.getSnapshot().posts.map(post => post.id), ['post-2', 'post-1']);
    assert.deepEqual(adapter.feed.current(), { kind: 'read', setup: { prompt: 'Shorter.', time: '08:30', when: 'Every day at 08:30 (UTC)' }, writing: false });
});
test('the Ideas page: the agent\'s suggestions, started, set aside and brought back by id, and read again when a run ends', async () => {
    const { transport, adapter } = connected();
    const last = action => transport.sent.filter(frame => frame.type === 'action' && frame.action.action === action).at(-1);
    const suggestion = (state = 'suggested') => ({ suggestion_id: 'idea-1', title: 'A run plan', summary: 'A week of short runs.', why: 'You set a goal to run 5k.', area: 'health', suggested_at: '2026-11-04T13:30:00Z', state });
    const page = async (suggestions) => {
        answer(transport, last('manage_suggestions').request_id, { result: 'suggestions', outcome: { result: 'listed', suggestions } });
        await flush();
    };
    await page([suggestion()]);
    assert.deepEqual(adapter.getSnapshot().ideas.map(idea => [idea.id, idea.title, idea.domain, idea.state]), [['idea-1', 'A run plan', 'Health & fitness', 'suggested']]);
    assert.deepEqual(adapter.ideas.current(), { kind: 'read', suggesting: false });
    for (const [type, op, state] of [['idea.activate', 'start', 'started'], ['idea.dismiss', 'dismiss', 'dismissed'], ['idea.restore', 'restore', 'suggested']]) {
        const acting = adapter.execute({ type, id: 'idea-1' });
        assert.deepEqual(transport.last().action, { action: 'manage_suggestions', op: { op, id: 'idea-1' } });
        answer(transport, transport.last().request_id, { result: 'suggestions', outcome: { result: 'changed', suggestion: suggestion(state) } });
        await flush();
        await page([suggestion(state)]);
        assert.equal((await acting).ok, true, type);
    }
    const sent = transport.sent.length;
    assert.match((await adapter.execute({ type: 'idea.edit', id: 'idea-1', summary: 'x' })).message, /Start it, and change what it says/);
    assert.equal(transport.sent.length, sent, 'an idea is never rewritten as the owner\'s');
    transport.deliver(sampleEvent(1, 1, { kind: 'schedule', outcome: 'completed', schedule_id: 'sch-1' }));
    await flush();
    answer(transport, transport.sent.filter(frame => frame.type === 'action' && frame.action.action === 'manage_schedule' && frame.action.op.op === 'list').at(-1).request_id, { result: 'schedule', outcome: { result: 'listed', schedules: [] } });
    await flush();
    answer(transport, transport.sent.filter(frame => frame.type === 'action' && frame.action.action === 'manage_schedule' && frame.action.op.op === 'list-parked').at(-1).request_id, { result: 'schedule', outcome: { result: 'parked', parked: [] } });
    await flush();
    await page([]);
    assert.deepEqual(adapter.getSnapshot().ideas, []);
});
test('a closed connection\'s late callbacks are ignored', () => {
    const transport = new FakeTransport();
    const adapter = new LiveAdapter(transport);
    const old = transport.current;
    old.closed('first try failed');
    adapter.chatSession.reconnect();
    old.opened();
    old.received(JSON.stringify(SAMPLE_WELCOME));
    old.closed('again');
    assert.equal(transport.sent.length, 0);
    assert.equal(adapter.chatSession.getSnapshot().connection.kind, 'reconnecting');
});
test('a connection that ends before the welcome says how to get back in', async () => {
    const transport = new FakeTransport();
    const adapter = new LiveAdapter(transport, 'test-ui');
    transport.drop('Nothing listens there.');
    assert.deepEqual(adapter.chatSession.getSnapshot().connection, { kind: 'offline', detail: 'Nothing listens there.' }, 'never opened: the transport says why');
    await adapter.chatSession.reconnect();
    transport.open();
    transport.drop('The connection to Hatchling ended.');
    const { kind, detail } = adapter.chatSession.getSnapshot().connection;
    assert.equal(kind, 'offline');
    assert.match(detail, /hatchling-host url/, 'opened, then ended before the welcome: a key it took no longer');
    await adapter.chatSession.reconnect();
    transport.open();
    transport.deliver(SAMPLE_WELCOME);
    transport.drop('Wi-Fi dropped.');
    assert.deepEqual(adapter.chatSession.getSnapshot().connection, { kind: 'offline', detail: 'Wi-Fi dropped.' }, 'after the welcome: the transport says why');
});
test('chat: typed events become messages; replays replace, never duplicate; every chat is tracked', () => {
    const { transport, adapter } = connected();
    replay(transport);
    const first = adapter.getSnapshot();
    const { chats, messages } = first;
    assert.deepEqual(chats, [{ id: 'main', title: 'Main', channelId: null, pinned: false, archived: false, unread: false, updatedAt: sampleTime(13) }]);
    assert.deepEqual(messages.slice(0, 2).map(item => [item.id, item.role, item.kind, item.text, item.time]), [['message-1', 'user', 'text', 'Please check the sample release notes.', '09:00'], ['message-2', 'agent', 'text', 'On it. I will ask four helpers.', '09:00']]);
    transport.deliver(sampleEvent(17, 20, { kind: 'chat_message', chat_id: 'main', message_id: 'message-2', author: 'agent', text: 'On it. Four helpers are working.' }));
    transport.deliver(sampleEvent(18, 21, { kind: 'chat_message', chat_id: 'side-1', message_id: 'message-9', author: 'system', text: 'A [notice](https://example.invalid) as plain text.' }));
    const after = adapter.getSnapshot();
    assert.equal(after.messages.length, 4);
    assert.equal(after.messages[1].text, 'On it. Four helpers are working.');
    assert.deepEqual(after.messages[3], { id: 'message-9', chatId: 'side-1', time: '09:00', source: null, reaction: null, kind: 'notice', role: 'agent', text: 'A [notice](https://example.invalid) as plain text.', evidenceId: null });
    assert.deepEqual(after.chats.map(item => [item.id, item.title, item.updatedAt]), [['main', 'Main', sampleTime(20)], ['side-1', 'Conversation', sampleTime(21)]]);
    assert.notEqual(after.messages[0], first.messages[0], 'a new snapshot, not a mutated one');
    assert.deepEqual(after.messages[0], first.messages[0]);
    assert.equal(first.messages[1].text, 'On it. I will ask four helpers.', 'an earlier snapshot keeps what it said');
    assert.deepEqual(new LiveAdapter(new FakeTransport()).getSnapshot().chats.map(item => [item.id, 'updatedAt' in item]), [['main', false]]);
});
test('an answer from a run is followed by what the run used, opening its record; until the record comes, the answer stands alone', () => {
    const { transport, adapter } = connected();
    const summary = (seq, cell, chat, tools) => sampleEvent(seq, seq, { kind: 'activity', state: 'success', event: { event: 'run_summary', event_id: `s-${seq}:summary`, run_id: `s-${seq}`, ts: sampleTime(seq), cell_id: cell, chat, tools, model_calls: 2, model_calls_refused: 0, peak_input_tokens: 5000, output_tokens: 40, context_window: 16384 } });
    const goals = { tool: 'goals_read', done: 1, asked: 0, refused: 0, failed: 0 };
    const web = { tool: 'web_page', done: 0, asked: 0, refused: 1, failed: 0 };
    replay(transport, [
        sampleEvent(1, 1, { kind: 'chat_message', chat_id: 'local', message_id: 'm-1', author: 'user', text: 'My goals?' }),
        sampleEvent(2, 2, { kind: 'chat_message', chat_id: 'local', message_id: 'm-2', author: 'agent', text: 'Ship 1.0.', run: 'run-a' }),
        sampleEvent(3, 3, { kind: 'chat_message', chat_id: 'local', message_id: 'm-3', author: 'agent', text: 'No run named.' }),
        sampleEvent(4, 4, { kind: 'chat_message', chat_id: 'local', message_id: 'm-4', author: 'agent', text: 'Hello.', run: 'run-b' }),
        sampleEvent(5, 5, { kind: 'chat_message', chat_id: 'local', message_id: 'm-5', author: 'agent', text: 'Its record is background work.', run: 'run-c' }),
    ]);
    assert.deepEqual(adapter.getSnapshot().messages.map(item => item.id), ['m-1', 'm-2', 'm-3', 'm-4', 'm-5'], 'no record yet: the answers alone');
    transport.deliver(summary(6, 'run-a', true, [goals, web]));
    transport.deliver(summary(7, 'run-b', true, []));
    transport.deliver(summary(8, 'run-c', false, [goals]));
    const messages = adapter.getSnapshot().messages;
    assert.deepEqual(messages.map(item => item.id), ['m-1', 'm-2', 'm-2:trail', 'm-3', 'm-4', 'm-4:trail', 'm-5']);
    assert.deepEqual(messages[2], { id: 'm-2:trail', chatId: 'main', time: '09:00', source: null, reaction: null, kind: 'notice', role: 'agent', text: 'Used: Read your goals · Read a web page (1 refused)', evidenceId: 's-6:summary' });
    assert.equal(messages[5].text, 'Used no tools: this answer came from the model alone.');
    // How full the context got on the last chat answer, against the declared window.
    assert.deepEqual(adapter.usage.context(), { tokens: 5000, window: 16384 });
});
test('markup and script in tool output and agent text render as inert text (negative test)', () => {
    // Control: without the adapter, the renderer makes these links real.
    const raw = walk(markdown(HOSTILE_TEXT, false));
    assert.ok(raw.tags.includes('a'), 'the hostile text would otherwise carry live links');
    const { transport, adapter } = connected();
    replay(transport);
    const message = adapter.getSnapshot().messages.find(item => item.id === 'message-3');
    const stopped = lane(adapter, 'child-b').rows[0];
    const opened = [];
    const rendered = [walk(markdown(message.text, false)), walk(markdown(stopped.report, true, { openArtifact: id => opened.push(id) }))];
    for (const out of rendered) {
        for (const tag of out.tags)
            assert.ok(FORMATTING.has(tag), `only formatting is built, not <${tag}>`);
        assert.deepEqual(out.hrefs, [], 'no link at all from untrusted text');
        assert.ok(out.text.includes('<script>alert("x")</script>'), 'markup is shown as the text it is');
        assert.ok(out.text.includes('<img src=x onerror="alert(1)">'));
        assert.ok(out.text.includes('[Approve] (https://example.invalid/approve)'), 'a link shows its real target as text');
        assert.ok(out.text.includes('[Open the file] (artifact:sample-file)'));
    }
    assert.ok(rendered[0].tags.includes('strong') && rendered[0].tags.includes('code'), 'allowed formatting survives');
    // In a report, untrusted text is a quotation: no headings, tables or code blocks of its own.
    for (const tag of ['h1', 'h2', 'h3', 'table', 'pre', 'ul', 'ol'])
        assert.ok(!rendered[1].tags.includes(tag), `the reason cannot build a <${tag}>`);
    assert.ok(rendered[1].tags.includes('blockquote'));
    assert.deepEqual(opened, []);
    // The pending step's note is quoted the same way.
    const fresh = connected();
    fresh.transport.deliver(sampleEvent(1, 1, sampleActivity(1, 'pending', { event: 'subagent_progress', child_id: 'n', note: HOSTILE_TEXT, steps_spent: 1 })));
    const note = walk(markdown(thread(fresh.adapter).pendingSteps[0].report, true, { openArtifact: id => opened.push(id) }));
    assert.deepEqual([note.hrefs, note.tags.filter(tag => !FORMATTING.has(tag))], [[], []]);
});
test('confirmation UI: typed approve and deny, through the existing approval review', async () => {
    const { transport, adapter } = connected();
    replay(transport);
    const [card] = adapter.taskApprovals.getSnapshot();
    assert.deepEqual(card, {
        id: 'confirm-1', revision: 2, chatId: 'main', threadId: '', kind: 'tool', title: 'Hatchling asks before it continues',
        description: 'Post the shorter sample summary to the sample team space?', resource: 'This request expires at 09:10 UTC.',
        detail: 'Allow once answers this request as described, and only this version of it. Your answer is recorded once.',
        scopes: ['once'], status: 'pending', time: '09:00', report: ['Operation: Post a summary', 'From the agent, Text: The shorter sample summary.'].join(String.fromCharCode(10)), fields: [{ label: 'Operation', value: 'Post a summary', source: 'broker' }, { label: 'Text', value: 'The shorter sample summary.', source: 'agent' }],
    });
    const changes = [];
    const review = new ApprovalReview(adapter.taskApprovals, () => changes.push(1), 60000);
    const pending = review.respond(card, 'once');
    await flush();
    const sent = transport.last();
    assert.equal(sent.type, 'action');
    assert.deepEqual(sent.action, { action: 'decide', confirmation_id: 'confirm-1', revision: 2, digest: 'a'.repeat(64), decision: 'approve' });
    answer(transport, sent.request_id, { result: 'decided', confirmation_id: 'confirm-1', decision: 'approve' });
    await pending;
    assert.equal(review.state('confirm-1').phase, 'unknown', 'accepted, and waiting for the broker\'s record');
    transport.deliver(decidedEvent(17, 'approve'));
    assert.equal(adapter.taskApprovals.getSnapshot()[0].status, 'allowed-once');
    assert.equal(review.pending('main').length, 0);
    // A replayed offer of a decided confirmation is never presented again.
    transport.deliver(confirmation(18, 3));
    assert.equal(adapter.taskApprovals.getSnapshot()[0].status, 'allowed-once');
    review.dispose();
    // Deny.
    const other = connected();
    other.transport.deliver(confirmation(1, 1));
    const denying = other.adapter.taskApprovals.respond({ id: 'attempt-deny', requestId: 'confirm-1', revision: 1, decision: 'deny' });
    await flush();
    assert.equal(other.transport.last().action.decision, 'deny');
    answer(other.transport, 'attempt-deny', { result: 'decided', confirmation_id: 'confirm-1', decision: 'deny' });
    assert.deepEqual(await denying, { state: 'accepted', detail: 'Hatchling recorded your answer.' });
    other.transport.deliver(decidedEvent(2, 'deny', 1));
    assert.deepEqual(other.adapter.taskApprovals.getSnapshot().map(item => [item.status, item.resource]), [['denied', 'This request does not expire.']]);
});
test('a site offers Allow for task, sent as approve_session and shown as allowed for the task', async () => {
    const { transport, adapter } = connected();
    const site = sampleEvent(1, 1, { kind: 'confirmation', confirmation_id: 'confirm-1', revision: 1, chat_id: 'main', summary: 'Let the agent read a site', expires_at: null, digest: 'digest-1', fields: [{ label: 'Site', value: 'https://a.example', source: 'broker' }], session: true });
    transport.deliver(site);
    const [card] = adapter.taskApprovals.getSnapshot();
    assert.deepEqual(card.scopes, ['once', 'task']);
    assert.match(card.detail, /Allow for task also answers every request like it until the agent's session ends; in a chat, that's this answer/);
    const pending = adapter.taskApprovals.respond({ id: 'attempt-task', requestId: 'confirm-1', revision: 1, decision: 'task' });
    await flush();
    assert.deepEqual(transport.last().action, { action: 'decide', confirmation_id: 'confirm-1', revision: 1, digest: 'digest-1', decision: 'approve_session' });
    answer(transport, 'attempt-task', { result: 'decided', confirmation_id: 'confirm-1', decision: 'approve_session' });
    assert.deepEqual(await pending, { state: 'accepted', detail: 'Hatchling recorded your answer.' });
    transport.deliver(decidedEvent(2, 'approve_session', 1));
    assert.equal(adapter.taskApprovals.getSnapshot()[0].status, 'allowed-task');
    // A request that doesn't offer it still refuses it, before anything is sent.
    const other = connected();
    other.transport.deliver(confirmation(1, 1));
    const sent = other.transport.sent.length;
    assert.match((await other.adapter.taskApprovals.respond({ id: 'a1', requestId: 'confirm-1', revision: 1, decision: 'task' })).detail, /not offered/);
    assert.equal(other.transport.sent.length, sent, 'nothing sent');
    assert.deepEqual(other.adapter.taskApprovals.getSnapshot()[0].scopes, ['once']);
    // The broker's rejection of a decision it doesn't offer has its words.
    const third = connected();
    third.transport.deliver(site);
    const rejected = third.adapter.taskApprovals.respond({ id: 'a2', requestId: 'confirm-1', revision: 1, decision: 'task' });
    await flush();
    answer(third.transport, 'a2', { result: 'decision_rejected', confirmation_id: 'confirm-1', reason: 'not_offered' });
    assert.deepEqual(await rejected, { state: 'rejected', detail: 'This request can only be approved once, so nothing was decided.' });
});
test('a site that can be kept offers Allow always, sent as approve_always and shown as always allowed', async () => {
    const { transport, adapter } = connected();
    transport.deliver(sampleEvent(1, 1, { kind: 'confirmation', confirmation_id: 'confirm-1', revision: 1, chat_id: 'main', summary: 'Let the agent read a site', expires_at: null, digest: 'digest-1', fields: [{ label: 'Site', value: 'https://a.example', source: 'broker' }], session: true, always: true }));
    const [card] = adapter.taskApprovals.getSnapshot();
    assert.deepEqual(card.scopes, ['once', 'task', 'always']);
    assert.match(card.detail, /Allow always answers every request like it, in every session, until you revoke it in Permissions\./);
    const pending = adapter.taskApprovals.respond({ id: 'attempt-always', requestId: 'confirm-1', revision: 1, decision: 'always' });
    await flush();
    assert.deepEqual(transport.last().action, { action: 'decide', confirmation_id: 'confirm-1', revision: 1, digest: 'digest-1', decision: 'approve_always' });
    answer(transport, 'attempt-always', { result: 'decided', confirmation_id: 'confirm-1', decision: 'approve_always' });
    assert.deepEqual(await pending, { state: 'accepted', detail: 'Hatchling recorded your answer.' });
    transport.deliver(decidedEvent(2, 'approve_always', 1));
    assert.equal(adapter.taskApprovals.getSnapshot()[0].status, 'allowed-always');
    // Deciding always asks the broker for its list again; and for what's
    // kept, since a decision can keep something.
    const asked = transport.sent.slice(-2).map(frame => frame.action);
    assert.deepEqual(asked, [{ action: 'manage_sites', op: { op: 'list' } }, { action: 'manage_memory', op: { op: 'list' } }]);
    // Where it isn't offered, it's refused before anything is sent.
    const other = connected();
    other.transport.deliver(sampleEvent(1, 1, { kind: 'confirmation', confirmation_id: 'confirm-1', revision: 1, chat_id: 'main', summary: 'Let the agent read a site', expires_at: null, digest: 'digest-1', fields: [], session: true }));
    assert.deepEqual(other.adapter.taskApprovals.getSnapshot()[0].scopes, ['once', 'task']);
    const before = other.transport.sent.length;
    assert.match((await other.adapter.taskApprovals.respond({ id: 'a1', requestId: 'confirm-1', revision: 1, decision: 'always' })).detail, /Allowing always is not offered/);
    assert.equal(other.transport.sent.length, before, 'nothing sent');
});
test('allowed sites: the broker\'s list, loaded on connecting; revoked by the owner, and listed again', async () => {
    const { transport, adapter } = connected();
    const listing = transport.sent.find(frame => frame.type === 'action' && frame.action.action === 'manage_sites');
    assert.deepEqual(listing?.action, { action: 'manage_sites', op: { op: 'list' } }, 'asked for on connecting');
    assert.deepEqual(adapter.getSnapshot().websitePermissions, [], 'empty until the broker answers');
    answer(transport, listing.request_id, { result: 'sites', outcome: { result: 'listed', sites: [{ site: 'https://a.example', allowed_at: '2026-10-20T08:00:00Z' }] } });
    await flush();
    assert.deepEqual(adapter.getSnapshot().websitePermissions, [{ host: 'https://a.example', policy: 'allow' }]);
    assert.deepEqual(await adapter.execute({ type: 'permission.website', host: 'https://b.example', policy: 'allow' }), { ok: false, message: "Allow a site from the agent's request for it, with Allow always." });
    const revoking = adapter.execute({ type: 'permission.website', host: 'https://a.example', policy: 'ask' });
    await flush();
    const revoke = transport.last();
    assert.deepEqual(revoke.action, { action: 'manage_sites', op: { op: 'revoke', site: 'https://a.example' } });
    answer(transport, revoke.request_id, { result: 'sites', outcome: { result: 'revoked' } });
    await flush();
    const relisting = transport.last();
    assert.deepEqual(relisting.action, { action: 'manage_sites', op: { op: 'list' } });
    answer(transport, relisting.request_id, { result: 'sites', outcome: { result: 'listed', sites: [] } });
    assert.deepEqual(await revoking, { ok: true, message: 'Revoked. The agent asks again the next time it wants a page on this site.' });
    assert.deepEqual(adapter.getSnapshot().websitePermissions, []);
    // Every other answer is told as what it is; only revoked and not found list again.
    for (const [outcome, ok, words, relists] of [
        [{ result: 'sites', outcome: { result: 'not_found' } }, false, /no longer allowed/, true],
        [{ result: 'sites', outcome: { result: 'off' } }, false, /keeps no allowed sites/, false],
        [{ result: 'sites', outcome: { result: 'failed' } }, false, /nothing was revoked/, false],
        [{ result: 'sites', outcome: { result: 'listed', sites: [] } }, false, /nothing was revoked/, false],
        [{ result: 'refused', reason: 'not_granted' }, false, /may not change the allowed sites/, false],
        [{ result: 'decided', confirmation_id: 'c', decision: 'approve' }, false, /not confirmed/, false],
    ]) {
        const pending = adapter.execute({ type: 'permission.website', host: 'https://a.example', policy: 'block' });
        await flush();
        answer(transport, transport.last().request_id, outcome);
        await flush();
        if (relists) {
            assert.deepEqual(transport.last().action.op, { op: 'list' });
            answer(transport, transport.last().request_id, { result: 'refused', reason: 'unavailable' });
        }
        const settled = await pending;
        assert.equal(settled.ok, ok);
        assert.match(settled.message, words);
    }
    // A list the broker didn't give leaves the last one as it was.
    assert.deepEqual(adapter.getSnapshot().websitePermissions, []);
    // Offline, nothing is sent and the owner is told.
    adapter.dispose();
    assert.equal((await adapter.execute({ type: 'permission.website', host: 'https://a.example', policy: 'ask' })).ok, false);
});
test('signed-in sites: the broker\'s list, loaded on connecting; signed out of and deleted by the owner, and listed again', async () => {
    const { transport, adapter } = connected();
    const listing = transport.sent.find(frame => frame.type === 'action' && frame.action.action === 'manage_profiles');
    assert.deepEqual(listing?.action, { action: 'manage_profiles', op: { op: 'list' } }, 'asked for on connecting');
    assert.deepEqual(adapter.getSnapshot().signedInSites, [], 'empty until the broker answers');
    const profile = { profile_id: 'profile-1', sites: ['a.example:443', 'idp.example:443'], signed_in: true, created_at: '2026-10-22T08:00:00Z', last_used: '2026-10-22T09:00:00Z' };
    answer(transport, listing.request_id, { result: 'profiles', outcome: { result: 'listed', profiles: [profile] } });
    await flush();
    assert.deepEqual(adapter.getSnapshot().signedInSites, [{ id: 'profile-1', sites: ['a.example:443', 'idp.example:443'], signedIn: true, lastUsed: '2026-10-22T09:00:00Z', signIns: [] }]);
    for (const [command, op, done, words] of [
        [{ type: 'profile.sign-out', id: 'profile-1' }, { op: 'sign_out', profile_id: 'profile-1' }, 'signed_out', /Signed out\. Its sites still open/],
        [{ type: 'profile.delete', id: 'profile-1' }, { op: 'delete', profile_id: 'profile-1' }, 'deleted', /Deleted\. The agent asks again/],
    ]) {
        const pending = adapter.execute(command);
        await flush();
        const change = transport.last();
        assert.deepEqual(change.action, { action: 'manage_profiles', op });
        answer(transport, change.request_id, { result: 'profiles', outcome: { result: done } });
        await flush();
        const relisting = transport.last();
        assert.deepEqual(relisting.action, { action: 'manage_profiles', op: { op: 'list' } });
        answer(transport, relisting.request_id, { result: 'profiles', outcome: { result: 'listed', profiles: [] } });
        const settled = await pending;
        assert.equal(settled.ok, true);
        assert.match(settled.message, words);
        assert.deepEqual(adapter.getSnapshot().signedInSites, []);
    }
    // Every other answer is told as what it is; only not found lists again.
    for (const [outcome, words, relists] of [
        [{ result: 'profiles', outcome: { result: 'deleted' } }, /not confirmed/, false],
        [{ result: 'profiles', outcome: { result: 'not_found' } }, /already gone/, true],
        [{ result: 'profiles', outcome: { result: 'off' } }, /keeps no signed-in sites/, false],
        [{ result: 'profiles', outcome: { result: 'failed' } }, /nothing changed/, false],
        [{ result: 'profiles', outcome: { result: 'listed', profiles: [] } }, /nothing changed/, false],
        [{ result: 'refused', reason: 'not_granted' }, /may not change the signed-in sites/, false],
        [{ result: 'sites', outcome: { result: 'revoked' } }, /not confirmed/, false],
    ]) {
        const pending = adapter.execute({ type: 'profile.sign-out', id: 'profile-1' });
        await flush();
        answer(transport, transport.last().request_id, outcome);
        await flush();
        if (relists) {
            assert.deepEqual(transport.last().action.op, { op: 'list' });
            answer(transport, transport.last().request_id, { result: 'refused', reason: 'unavailable' });
        }
        const settled = await pending;
        assert.equal(settled.ok, false);
        assert.match(settled.message, words);
    }
    // A list the broker didn't give leaves the last one as it was.
    assert.deepEqual(adapter.getSnapshot().signedInSites, []);
    // Offline, nothing is sent and the owner is told.
    adapter.dispose();
    assert.equal((await adapter.execute({ type: 'profile.delete', id: 'profile-1' })).ok, false);
});
test('sign-ins: listed by label on each signed-in site, kept straight to the broker, deleted by the owner', async () => {
    const { transport, adapter } = connected();
    const listing = action => transport.sent.find(frame => frame.type === 'action' && frame.action.action === action);
    const profile = { profile_id: 'profile-1', sites: ['a.example:443'], signed_in: true, created_at: '2026-10-22T08:00:00Z', last_used: '2026-10-22T09:00:00Z' };
    answer(transport, listing('manage_profiles').request_id, { result: 'profiles', outcome: { result: 'listed', profiles: [profile] } });
    assert.deepEqual(listing('manage_sign_ins')?.action, { action: 'manage_sign_ins', op: { op: 'list' } }, 'asked for on connecting');
    const work = { sign_in_id: 'cred-1', profile_id: 'profile-1', origin: 'https://a.example', label: 'Work', created_at: '2026-10-22T08:00:00Z' };
    const theirs = { ...work, sign_in_id: 'cred-2', profile_id: 'profile-9', label: 'Not this site' };
    answer(transport, listing('manage_sign_ins').request_id, { result: 'sign_ins', outcome: { result: 'listed', sign_ins: [work, theirs] } });
    await flush();
    assert.deepEqual(adapter.getSnapshot().signedInSites[0].signIns, [{ id: 'cred-1', label: 'Work', origin: 'https://a.example' }], 'its own, by label');
    // Kept: sent once, as its own request, then listed again. Never in the state.
    const entry = { profileId: 'profile-1', origin: 'https://a.example', label: 'Home', username: 'zoe-user', password: 'hunter2-secret' };
    const keeping = adapter.signIns.store(entry);
    await flush();
    const kept = transport.last();
    assert.deepEqual(kept.action, { action: 'manage_sign_ins', op: { op: 'store', profile_id: 'profile-1', origin: 'https://a.example', label: 'Home', username: 'zoe-user', password: 'hunter2-secret' } });
    answer(transport, kept.request_id, { result: 'sign_ins', outcome: { result: 'stored', sign_in: { ...work, sign_in_id: 'cred-3', label: 'Home' } } });
    await flush();
    const relisting = transport.last();
    assert.deepEqual(relisting.action, { action: 'manage_sign_ins', op: { op: 'list' } });
    answer(transport, relisting.request_id, { result: 'sign_ins', outcome: { result: 'listed', sign_ins: [work, { ...work, sign_in_id: 'cred-3', label: 'Home' }] } });
    assert.deepEqual(await keeping, { ok: true, message: 'Kept. The agent can sign in with it on this site; it never sees the password.' });
    const state = JSON.stringify(adapter.getSnapshot());
    assert.ok(!state.includes('hunter2') && !state.includes('zoe-user'), 'the secrets are in no state');
    assert.deepEqual(adapter.getSnapshot().signedInSites[0].signIns.map(item => item.label), ['Work', 'Home']);
    // Deleted, by the owner's command.
    const deleting = adapter.execute({ type: 'sign-in.delete', id: 'cred-3' });
    await flush();
    const deletion = transport.last();
    assert.deepEqual(deletion.action, { action: 'manage_sign_ins', op: { op: 'delete', sign_in_id: 'cred-3' } });
    answer(transport, deletion.request_id, { result: 'sign_ins', outcome: { result: 'deleted' } });
    await flush();
    answer(transport, transport.last().request_id, { result: 'sign_ins', outcome: { result: 'listed', sign_ins: [work] } });
    assert.deepEqual(await deleting, { ok: true, message: 'Deleted. The agent can no longer sign in with it.' });
    // Every other answer is told as what it is; only not found lists again.
    const refusals = { no_profile: /site is gone/, bad_origin: /site’s own addresses/, not_the_profiles: /not one of the signed-in site’s/, bad_label: /Give the sign-in a name/, no_password: /Type the password/, too_long: /too long/, full: /Delete one first/ };
    const others = [
        ...Object.entries(refusals).map(([reason, words]) => [{ result: 'sign_ins', outcome: { result: 'refused', reason } }, words, false]),
        [{ result: 'sign_ins', outcome: { result: 'deleted' } }, /not confirmed/, false],
        [{ result: 'sign_ins', outcome: { result: 'not_found' } }, /already gone/, true],
        [{ result: 'sign_ins', outcome: { result: 'off' } }, /keeps no sign-ins/, false],
        [{ result: 'sign_ins', outcome: { result: 'failed' } }, /nothing changed/, false],
        [{ result: 'sign_ins', outcome: { result: 'listed', sign_ins: [] } }, /nothing changed/, false],
        [{ result: 'refused', reason: 'not_granted' }, /may not change the sign-ins/, false],
        [{ result: 'profiles', outcome: { result: 'deleted' } }, /not confirmed/, false],
    ];
    for (const [outcome, words, relists] of others) {
        const pending = adapter.signIns.store(entry);
        await flush();
        answer(transport, transport.last().request_id, outcome);
        await flush();
        if (relists) {
            assert.deepEqual(transport.last().action.op, { op: 'list' });
            answer(transport, transport.last().request_id, { result: 'refused', reason: 'unavailable' });
        }
        const settled = await pending;
        assert.equal(settled.ok, false);
        assert.match(settled.message, words);
    }
    assert.deepEqual(adapter.getSnapshot().signedInSites[0].signIns.map(item => item.label), ['Work'], 'a list the broker did not give leaves the last');
    adapter.dispose();
    assert.equal((await adapter.signIns.store(entry)).ok, false, 'offline: nothing sent');
});
test('a decision is re-validated at dispatch, against the broker\'s latest state, not the card that was drawn', async () => {
    const { transport, adapter } = connected();
    transport.deliver(confirmation(1, 1));
    const drawn = adapter.taskApprovals.getSnapshot()[0];
    transport.deliver(confirmation(2, 2, 'Post a different summary?'));
    const sentBefore = transport.sent.length;
    // The review UI's own guard sees the change too...
    const review = new ApprovalReview(adapter.taskApprovals, () => undefined, 60000);
    await review.respond(drawn, 'once');
    // ...and the port refuses at dispatch even when asked directly.
    assert.deepEqual(await adapter.taskApprovals.respond({ id: 'a1', requestId: 'confirm-1', revision: 1, decision: 'once' }), { state: 'rejected', detail: 'This request changed after it was shown. Review the new version before deciding.' });
    assert.match((await adapter.taskApprovals.respond({ id: 'a2', requestId: 'confirm-1', revision: 2, decision: 'task' })).detail, /not offered/);
    assert.match((await adapter.taskApprovals.respond({ id: 'a3', requestId: 'nothing', revision: 1, decision: 'once' })).detail, /no longer waiting/);
    assert.equal(transport.sent.length, sentBefore, 'nothing stale was sent');
    // One answer per attempt, and one in flight per request.
    const first = adapter.taskApprovals.respond({ id: 'a4', requestId: 'confirm-1', revision: 2, decision: 'once' });
    assert.equal(adapter.taskApprovals.respond({ id: 'a4', requestId: 'confirm-1', revision: 2, decision: 'once' }), first);
    assert.match((await adapter.taskApprovals.respond({ id: 'a5', requestId: 'confirm-1', revision: 2, decision: 'deny' })).detail, /already on its way/);
    await flush();
    assert.equal(transport.sent.length, sentBefore + 1);
    transport.drop('gone');
    assert.equal((await first).state, 'unknown', 'a lost answer is unknown, never a rejection');
    assert.match((await adapter.taskApprovals.respond({ id: 'a6', requestId: 'confirm-1', revision: 2, decision: 'once' })).detail, /not connected/);
    review.dispose();
});
test('the broker\'s answers to a decision map to what the person sees', async () => {
    const cases = [
        [{ result: 'decision_rejected', confirmation_id: 'confirm-1', reason: 'expired' }, 'rejected', /expired/, 'expired'],
        [{ result: 'decision_rejected', confirmation_id: 'confirm-1', reason: 'unknown' }, 'rejected', /no longer has this request/, 'cancelled'],
        [{ result: 'decision_rejected', confirmation_id: 'confirm-1', reason: 'stale_revision' }, 'rejected', /changed after it was shown/, 'pending'],
        [{ result: 'decision_rejected', confirmation_id: 'confirm-1', reason: 'already_decided' }, 'rejected', /already decided/, 'pending'],
        [{ result: 'refused', reason: 'not_connected' }, 'rejected', /not connected to its broker/, 'pending'],
        [{ result: 'refused', reason: 'bad_request' }, 'rejected', /could not read the decision/, 'pending'],
        [{ result: 'refused', reason: 'unavailable' }, 'rejected', /cannot take decisions/, 'pending'],
        [{ result: 'refused', reason: 'not_granted' }, 'rejected', /not allowed to take decisions/, 'pending'],
        [{ result: 'message_accepted', message_id: 'm' }, 'unknown', /something other than a decision/, 'pending'],
    ];
    for (const [result, state, words, status] of cases) {
        const { transport, adapter } = connected();
        transport.deliver(confirmation(1, 1));
        const reply = adapter.taskApprovals.respond({ id: 'x', requestId: 'confirm-1', revision: 1, decision: 'once' });
        await flush();
        answer(transport, 'x', result);
        const outcome = await reply;
        assert.equal(outcome.state, state);
        assert.match(outcome.detail, words);
        assert.equal(adapter.taskApprovals.getSnapshot()[0].status, status);
        if (status !== 'pending') {
            assert.match((await adapter.taskApprovals.respond({ id: 'y', requestId: 'confirm-1', revision: 1, decision: 'once' })).detail, /no longer waiting/);
            transport.deliver(confirmation(2, 2));
            assert.equal(adapter.taskApprovals.getSnapshot()[0].status, 'pending', 'a newer revision is a new question');
        }
    }
});
test('approvals reconcile without resending, and an older revision never replaces a newer one', async () => {
    const { transport, adapter } = connected();
    transport.deliver(confirmation(1, 2));
    transport.deliver(confirmation(2, 1, 'An older version.'));
    assert.equal(adapter.taskApprovals.getSnapshot()[0].revision, 2);
    // A confirmation that names no chat belongs to the main one.
    transport.deliver(sampleEvent(3, 3, { kind: 'confirmation', confirmation_id: 'confirm-2', revision: 1, summary: 'A general question.', digest: 'digest-q', fields: [] }));
    assert.deepEqual(adapter.taskApprovals.getSnapshot().map(item => [item.id, item.chatId]), [['confirm-1', 'main'], ['confirm-2', 'main']]);
    await adapter.taskApprovals.refresh('confirm-1');
    assert.deepEqual(transport.last(), { type: 'subscribe', after_seq: 3 });
    transport.drop();
    const count = transport.sent.length;
    await adapter.taskApprovals.refresh('confirm-1');
    assert.equal(transport.sent.length, count, 'nothing to ask while disconnected');
    let heard = 0;
    const stop = adapter.taskApprovals.subscribe(() => heard++);
    adapter.chatSession.reconnect();
    stop();
    transport.open();
    transport.deliver(SAMPLE_WELCOME);
    assert.equal(heard, 1, 'a stopped listener hears nothing more');
});
test('sending: the typed send_message action under the UI\'s request id, with honest outcomes', async () => {
    const { transport, adapter } = connected();
    const command = { type: 'chat.send', chatId: 'main', text: 'Hello' };
    const sending = adapter.chatSession.send({ id: 'send-1', command });
    assert.equal(adapter.chatSession.send({ id: 'send-1', command }), sending, 'a repeated request gets the first outcome');
    await flush();
    // The app's main chat is the broker's local channel on the wire.
    assert.deepEqual(transport.last(), { type: 'action', request_id: 'send-1', action: { action: 'send_message', chat_id: 'local', text: 'Hello' } });
    assert.equal(sentActions(transport).length, 1);
    answer(transport, 'send-1', { result: 'message_accepted', message_id: 'message-10' });
    assert.deepEqual(await sending, { state: 'accepted', messageId: 'message-10' });
    assert.deepEqual(await adapter.chatSession.checkDelivery('send-1'), { state: 'accepted', messageId: 'message-10' });
    assert.equal((await adapter.chatSession.checkDelivery('never-sent')).state, 'unknown');
    assert.match((await adapter.chatSession.send({ id: 'f', command: { type: 'chat.attach', chatId: 'main', artifactId: 'a' } })).detail, /This file is no longer available to send/);
    assert.match((await adapter.chatSession.send({ id: 'g', command: { ...command, chatId: 'gone' } })).detail, /no longer available/);
    for (const [index, [result, state, words]] of [
        [{ result: 'refused', reason: 'not_connected' }, 'rejected', /not connected to its broker/],
        [{ result: 'refused', reason: 'bad_request' }, 'rejected', /could not read the message/],
        [{ result: 'refused', reason: 'unavailable' }, 'rejected', /cannot take messages/],
        [{ result: 'refused', reason: 'not_granted' }, 'rejected', /not allowed to send messages/],
        [{ result: 'decided', confirmation_id: 'c', decision: 'approve' }, 'unknown', /not confirmed/],
    ].entries()) {
        const id = `send-case-${index}`;
        const outcome = adapter.chatSession.send({ id, command });
        await flush();
        answer(transport, id, result);
        const settled = await outcome;
        assert.equal(settled.state, state);
        assert.match(settled.detail, words);
    }
    const lost = adapter.chatSession.send({ id: 'lost', command });
    await flush();
    transport.drop();
    assert.equal((await lost).state, 'unknown', 'a dropped connection is unknown, never a rejection');
    assert.match((await adapter.chatSession.send({ id: 'offline', command })).detail, /not connected to Hatchling, so nothing was sent/);
    await adapter.chatSession.retryHistory('main');
    assert.equal(transport.last().type, 'action', 'no history request while disconnected');
    let heard = 0;
    const stop = adapter.chatSession.subscribe(() => heard++);
    await adapter.chatSession.reconnect();
    stop();
    transport.open();
    transport.deliver(SAMPLE_WELCOME);
    assert.equal(heard, 1, 'a stopped listener hears nothing more');
});
test('a new conversation: the typed new_conversation action on the local chat, never a Command, with honest outcomes', async () => {
    const { transport, adapter } = connected();
    const start = () => adapter.conversations.start('main');
    const started = start();
    await flush();
    assert.deepEqual(transport.last().action, { action: 'new_conversation', chat_id: 'local' });
    answer(transport, transport.last().request_id, { result: 'conversation_started', chat_id: 'local' });
    assert.deepEqual(await started, { ok: true, message: 'New conversation started.' });
    for (const [result, words] of [
        [{ result: 'refused', reason: 'not_connected' }, /not connected to its broker, so the agent still has the last conversation/],
        [{ result: 'refused', reason: 'bad_request' }, /no such conversation/],
        [{ result: 'refused', reason: 'unavailable' }, /cannot start a new conversation right now/],
        [{ result: 'refused', reason: 'not_granted' }, /may not start a new conversation/],
        [{ result: 'message_accepted', message_id: 'm' }, /not confirmed/],
    ]) {
        const outcome = start();
        await flush();
        answer(transport, transport.last().request_id, result);
        const settled = await outcome;
        assert.equal(settled.ok, false);
        assert.match(settled.message, words);
    }
    // The broker's notice shows where it starts, as Hatchling's words.
    transport.deliver(sampleEvent(40, 40, { kind: 'chat_message', chat_id: 'local', message_id: 'notice-1', author: 'system', text: 'New conversation. The agent is given nothing said before this.' }));
    assert.deepEqual(adapter.getSnapshot().messages.filter(message => message.id === 'notice-1').map(message => [message.kind, message.chatId]), [['notice', 'main']]);
    transport.drop();
    const offline = await start();
    assert.equal(offline.ok, false);
    assert.match(offline.message, /not connected to Hatchling/);
});
test('history retry asks for what came after the last event', async () => {
    const { transport, adapter } = connected();
    replay(transport, SAMPLE_EVENTS.slice(0, 3));
    await adapter.chatSession.retryHistory('main');
    assert.deepEqual(transport.last(), { type: 'subscribe', after_seq: 3 });
});
test('the adapter: product commands go to the broker or are refused, never simulated', async () => {
    const { transport, adapter } = connected();
    const sending = adapter.execute({ type: 'chat.send', chatId: 'main', text: 'Hi' });
    await flush();
    answer(transport, transport.last().request_id, { result: 'message_accepted', message_id: 'message-11' });
    assert.deepEqual(await sending, { ok: true, message: 'Sent.', id: 'message-11' });
    const refused = adapter.execute({ type: 'chat.send', chatId: 'main', text: 'Hi again' });
    await flush();
    answer(transport, transport.last().request_id, { result: 'refused', reason: 'unavailable' });
    assert.deepEqual(await refused, { ok: false, message: 'Hatchling cannot take messages right now, so it was not sent.' });
    // A port that accepts without an id still reports success, without inventing one.
    adapter.chatSession.send = async () => ({ state: 'accepted', messageId: null });
    assert.deepEqual(await adapter.execute({ type: 'chat.send', chatId: 'main', text: 'x' }), { ok: true, message: 'Sent.' });
    assert.deepEqual(await adapter.execute({ type: 'chat.read', id: 'main' }), { ok: true, message: 'Read.' });
    for (const type of ['demo.reset', 'demo.advance', 'demo.scenario'])
        assert.match((await adapter.execute({ type, step: 1, scenario: 'research' })).message, /Sample scenarios are not available/);
    for (const command of [{ type: 'artifact.create', title: 't', format: 'markdown' }, { type: 'purchase.approve', id: 'p', revision: 1, total: 1 }, { type: 'turn.interrupt' }])
        assert.deepEqual(await adapter.execute(command), { ok: false, message: 'Hatchling cannot do this from the app yet.' });
});
test('the snapshot is honest: only what the broker said, display preferences kept locally', async () => {
    const { transport, adapter } = connected();
    const state = adapter.getSnapshot();
    for (const key of ['artifacts', 'goals', 'ideas', 'posts', 'purchases', 'browserTasks', 'devices', 'jobs', 'notifications', 'memory', 'connectors', 'channels', 'providers', 'permissions', 'websitePermissions', 'signedInSites', 'networkPermissions', 'approvalHistory', 'publications', 'systemEntries', 'secureItems'])
        assert.deepEqual(state[key], [], `${key} is empty, not sample data`);
    assert.deepEqual([state.version, state.spendingLimit, state.spentToday, state.settings.allowlist, state.turn, state.clock], [1, 0, 0, [], 'idle', '']);
    let heard = 0;
    adapter.subscribe(() => heard++);
    let other = 0;
    adapter.subscribe(() => other++)();
    const revision = adapter.getSnapshot().revision;
    for (const [command, field, value] of [[{ type: 'settings.theme', value: 'dark' }, 'theme', 'dark'], [{ type: 'settings.accent', value: 'blue' }, 'accent', 'blue'], [{ type: 'settings.interface-size', value: 'large' }, 'interfaceSize', 'large'], [{ type: 'settings.chat-panel', value: false }, 'keepChatPanelVisible', false]]) {
        assert.deepEqual(await adapter.execute(command), { ok: true, message: 'Saved.' });
        assert.equal(adapter.getSnapshot().settings[field], value);
    }
    assert.equal(heard, 4);
    assert.equal(adapter.getSnapshot().revision, revision + 4);
    assert.equal(sentActions(transport).length, 0, 'preferences never reach the broker');
    // Events no view shows yet still move the clock, and nothing else.
    replay(transport, SAMPLE_EVENTS.slice(14));
    assert.equal(adapter.getSnapshot().clock, sampleTime(17));
    adapter.dispose();
    assert.equal(transport.closes, 1);
    assert.equal(adapter.chatSession.getSnapshot().connection.kind, 'offline');
    const atDispose = heard;
    await adapter.execute({ type: 'settings.theme', value: 'light' });
    assert.equal(heard, atDispose, 'nothing is heard after dispose');
    assert.equal(other, 0, 'an unsubscribed listener hears nothing');
});
test('the client: answers match their requests, a stray answer is dropped, a waiting request is not sent twice', async () => {
    const transport = new FakeTransport();
    const events = [];
    const client = new LiveClient(transport, 'raw', event => { events.push(event.seq); return undefined; });
    client.connect();
    client.connect();
    assert.equal(transport.connects, 1);
    client.resync();
    assert.equal(transport.sent.length, 0, 'nothing before the connection opens');
    transport.open();
    transport.deliver(SAMPLE_WELCOME);
    const action = { action: 'send_message', chat_id: 'main', text: 'x' };
    const first = client.request('r1', action);
    assert.equal(client.request('r1', action), first);
    answer(transport, 'stray', { result: 'refused', reason: 'unavailable' });
    answer(transport, 'r1', { result: 'message_accepted', message_id: 'm1' });
    assert.deepEqual(await first, { kind: 'answered', result: { result: 'message_accepted', message_id: 'm1' } });
    assert.equal(transport.sent.filter(frame => frame.type === 'action').length, 1);
    replay(transport, SAMPLE_EVENTS.slice(0, 2));
    assert.deepEqual(events, [1, 2]);
    assert.equal(client.decision('none'), undefined);
    client.dispose();
    assert.deepEqual(client.connection(), { kind: 'offline', detail: 'The app closed its connection.' });
});
test('the runtime check accepts every typed fixture and the frames the UI sends', () => {
    for (const frame of [SAMPLE_WELCOME, ...SAMPLE_EVENTS])
        assert.ok(isUiResponse(frame), JSON.stringify(frame));
    assert.deepEqual(decodeResponse(JSON.stringify(SAMPLE_EVENTS[2])), SAMPLE_EVENTS[2]);
    for (const request of [{ type: 'hello', schema_version: 1, client: 'x' }, { type: 'subscribe', after_seq: null }, { type: 'subscribe' }, { type: 'action', request_id: 'r', action: { action: 'decide', confirmation_id: 'c', revision: 1, digest: 'd', decision: 'deny' } }])
        assert.ok(isUiRequest(JSON.parse(encodeRequest(request))), JSON.stringify(request));
});
test('the runtime check rejects whatever the generated types would not describe', () => {
    const event = SAMPLE_EVENTS[2];
    const bad = [
        null, 7, 'event', [], { ...event, type: 'evnt' }, { ...event, seq: 1.5 }, { ...event, seq: -1 }, { ...event, ts: 5 },
        { ...event, body: { ...event.body, state: 'unknown' } },
        { ...event, body: { ...event.body, event: { ...event.body.event, capabilities: ['x', 3] } } },
        { ...event, body: { ...event.body, event: { ...event.body.event, depth: 256 } } },
        { type: 'event', seq: 1, ts: sampleTime(1) },
        { ...SAMPLE_WELCOME, schema_version: '1' },
        { type: 'action_result', request_id: 'r', result: { result: 'refused', reason: 'maybe' } },
    ];
    for (const frame of bad)
        assert.equal(isUiResponse(frame), false, JSON.stringify(frame));
    assert.equal(isUiRequest({ type: 'hello', schema_version: 1 }), false);
    assert.equal(decodeResponse('{'), undefined);
});
test('the runtime check interprets each keyword exactly', () => {
    const none = {};
    assert.equal(conforms('x', true, none), true);
    assert.equal(conforms('x', false, none), false);
    assert.equal(conforms('x', { $ref: '#/$defs/Missing' }, none), false);
    assert.equal(conforms('x', { $ref: 'elsewhere#/Thing' }, { Thing: true }), false);
    assert.equal(conforms('x', { $ref: '#/$defs/constructor' }, none), false, 'only own definitions resolve');
    assert.equal(conforms(null, { type: ['string', 'null'] }, none), true);
    assert.equal(conforms(true, { type: 'boolean' }, none), true);
    assert.equal(conforms(1.5, { type: 'number' }, none), true);
    assert.equal(conforms(Infinity, { type: 'number' }, none), false);
    assert.equal(conforms({}, { type: 'array' }, none), false);
    assert.equal(conforms('x', { type: 'mystery' }, none), false);
    assert.equal(conforms(3, { minimum: 4 }, none), false);
    assert.equal(conforms(5, { maximum: 4 }, none), false);
    assert.equal(conforms(4, { minimum: 4, maximum: 4 }, none), true);
    assert.equal(conforms('3', { minimum: 4 }, none), true, 'bounds apply to numbers only');
    assert.equal(conforms('a', { oneOf: [{ type: 'string' }, { const: 'a' }] }, none), false, 'oneOf means exactly one');
    assert.equal(conforms(['a', 1], { items: { type: 'string' } }, none), false);
    assert.equal(conforms(['a'], { items: { type: 'string' } }, none), true);
    assert.equal(conforms({ a: 1 }, { required: ['b'] }, none), false);
    assert.equal(conforms({ a: 1, b: 'x' }, { properties: { a: { type: 'integer' } }, additionalProperties: false }, none), false);
    assert.equal(conforms({ a: 1, b: 'x' }, { properties: { a: { type: 'integer' } }, additionalProperties: { type: 'string' } }, none), true);
    assert.equal(conforms({ a: 1, b: 'x' }, { properties: { a: { type: 'integer' } } }, none), true, 'unknown fields are allowed, as serde allows them');
});
test('wording helpers: inert links, quotations, counts and times', () => {
    assert.equal(inertLinks('[a](b) and [c](d)'), '[a] (b) and [c] (d)');
    assert.equal(quoted('one\r\ntwo\n'), '> one\n> two\n> ');
    assert.deepEqual([count(1, 'step', 'steps'), count(0, 'step', 'steps')], ['1 step', '0 steps']);
    // In the person's own time zone, not UTC.
    const zone = process.env.TZ;
    try {
        const at = '2026-10-07T23:59:59Z';
        process.env.TZ = 'UTC';
        assert.equal(clockTime(at), '23:59');
        process.env.TZ = 'America/New_York';
        assert.equal(clockTime(at), '19:59');
        process.env.TZ = 'Asia/Kolkata';
        assert.equal(clockTime(at), '05:29');
    }
    finally {
        if (zone === undefined)
            delete process.env.TZ;
        else
            process.env.TZ = zone;
    }
    assert.equal(clockTime('yesterday'), '');
    assert.deepEqual([between(null, sampleTime(1)), between(sampleTime(2), sampleTime(1)), between(sampleTime(1), sampleTime(3))], [null, null, 2000]);
});
test('goals: listed into the snapshot, set and changed by the page\'s commands, closed only with an ending', async () => {
    const { transport, adapter } = connected();
    const last = op => transport.sent.filter(frame => frame.type === 'action' && frame.action.action === 'manage_goals' && frame.action.op.op === op).at(-1);
    const goal = { id: 'g1', title: 'Run a 10k', detail: '', state: 'open', created_at: 1793707200, updated_at: 1793707200 };
    const list = async (goals) => {
        answer(transport, last('list').request_id, { result: 'goals', outcome: { result: 'listed', goals } });
        await flush();
    };
    assert.deepEqual(adapter.getSnapshot().goals, [], 'empty until the broker answers');
    await list([goal]);
    assert.deepEqual(adapter.getSnapshot().goals.map(item => [item.id, item.title, item.status]), [['g1', 'Run a 10k', 'open']]);
    const creating = adapter.execute({ type: 'goal.create', title: 'Read more', description: 'Twelve books', category: 'Interests', due: '2027-01-01' });
    assert.deepEqual(last('create').action, { action: 'manage_goals', op: { op: 'create', title: 'Read more', detail: 'Twelve books' } });
    answer(transport, last('create').request_id, { result: 'goals', outcome: { result: 'saved', goal: { ...goal, id: 'g2', title: 'Read more' } } });
    await flush();
    await list([goal, { ...goal, id: 'g2', title: 'Read more' }]);
    assert.equal((await creating).ok, true);
    const updating = adapter.execute({ type: 'goal.update', id: 'g1', title: 'Run a half', description: 'In spring', due: '' });
    assert.deepEqual(last('update').action, { action: 'manage_goals', op: { op: 'update', id: 'g1', title: 'Run a half', detail: 'In spring' } });
    answer(transport, last('update').request_id, { result: 'goals', outcome: { result: 'rejected' } });
    assert.equal((await updating).ok, false);
    assert.match((await adapter.execute({ type: 'goal.close', id: 'g1' })).message, /reached or set aside/);
    assert.match((await adapter.execute({ type: 'goal.reopen', id: 'g1' })).message, /stays closed/);
    assert.equal(typeof adapter.goals.close, 'function');
});
test('memory: listed into the snapshot, forgotten from the page, never added there', async () => {
    const { transport, adapter } = connected();
    const last = op => transport.sent.filter(frame => frame.type === 'action' && frame.action.action === 'manage_memory' && frame.action.op.op === op).at(-1);
    answer(transport, last('list').request_id, { result: 'memory', outcome: { result: 'listed', kept: [{ id: 'user/kept-1', content: 'Their cat is Biscuit.', kept_at: 1793707200, approved: true }] } });
    await flush();
    assert.deepEqual(adapter.getSnapshot().memory.map(entry => [entry.id, entry.body]), [['user/kept-1', 'Their cat is Biscuit.']]);
    const forgetting = adapter.execute({ type: 'memory.delete', id: 'user/kept-1' });
    assert.deepEqual(last('forget').action, { action: 'manage_memory', op: { op: 'forget', id: 'user/kept-1' } });
    answer(transport, last('forget').request_id, { result: 'memory', outcome: { result: 'forgotten' } });
    await flush();
    answer(transport, last('list').request_id, { result: 'memory', outcome: { result: 'listed', kept: [] } });
    assert.equal((await forgetting).ok, true);
    assert.deepEqual(adapter.getSnapshot().memory, []);
    assert.match((await adapter.execute({ type: 'memory.save', id: '', title: 't', body: 'b' })).message, /you approve its words/);
});
test('an approval that says what approving does is labelled so; one that doesn\'t is Allow once', () => {
    const { transport, adapter } = connected();
    const card = (id, extra) => sampleEvent(1, 1, { kind: 'confirmation', confirmation_id: id, revision: 1, chat_id: 'main', summary: 'Remember this about you?', expires_at: null, digest: `digest-${id}`, fields: [{ label: 'What it would remember', value: 'Their cat is Biscuit.', source: 'agent' }], ...extra });
    transport.deliver(card('keep-1', { approve: 'Remember it' }));
    const [kept] = adapter.taskApprovals.getSnapshot();
    assert.equal(kept.approveLabel, 'Remember it');
    assert.match(kept.detail, /^Remember it answers this request as described/);
    assert.deepEqual(kept.scopes, ['once']);
    const other = connected();
    other.transport.deliver(card('plain-1', {}));
    const [plain] = other.adapter.taskApprovals.getSnapshot();
    assert.equal('approveLabel' in plain, false);
    assert.match(plain.detail, /^Allow once answers/);
});
test('Hatchling\'s own word reads the schedules again: a schedule the owner approved appears', async () => {
    const { transport } = connected();
    const lists = () => transport.sent.filter(frame => frame.type === 'action' && frame.action.action === 'manage_schedule' && frame.action.op.op === 'list').length;
    const before = lists();
    transport.deliver(sampleEvent(1, 1, { kind: 'chat_message', chat_id: 'local', message_id: 'm-agent', author: 'agent', text: 'I asked you to approve it.' }));
    assert.equal(lists(), before, 'not on the agent\'s words');
    transport.deliver(sampleEvent(2, 2, { kind: 'chat_message', chat_id: 'local', message_id: 'm-system', author: 'system', text: 'Scheduled: “Stretch.”' }));
    assert.equal(lists(), before + 1);
});
test('approvals history: what was decided before the connection, as it was decided, after what\'s live', async () => {
    const { transport, adapter } = connected();
    const asked = transport.sent.find(frame => frame.type === 'action' && frame.action.action === 'list_decisions');
    assert.deepEqual(asked.action, { action: 'list_decisions' });
    assert.deepEqual(adapter.taskApprovals.getSnapshot(), [], 'nothing until the broker answers');
    let told = 0;
    const unsubscribe = adapter.taskApprovals.subscribe(() => { told++; });
    const record = (id, decision) => ({ confirmation_id: id, summary: 'Remember this about you?', fields: [{ label: 'What it would remember', value: 'Their cat is Biscuit.', source: 'agent' }], decision, decided_at: '2026-11-03T12:00:00Z' });
    answer(transport, asked.request_id, { result: 'decisions', decisions: [record('kept-1', 'approve'), record('kept-2', 'deny'), record('confirm-1', 'approve_always')] });
    await flush();
    assert.ok(told > 0, 'the history is told when it arrives');
    // A request the stream shows live is shown live, once.
    transport.deliver(sampleEvent(1, 1, { kind: 'confirmation', confirmation_id: 'confirm-1', revision: 1, chat_id: 'main', summary: 'Let the agent read a site', expires_at: null, digest: 'digest-1', fields: [{ label: 'Site', value: 'https://a.example', source: 'broker' }], session: true, always: true }));
    const cards = adapter.taskApprovals.getSnapshot();
    assert.deepEqual(cards.map(card => [card.id, card.status]), [['confirm-1', 'pending'], ['kept-1', 'allowed-once'], ['kept-2', 'denied']]);
    assert.equal(cards[1].description, 'Remember this about you?');
    assert.match(cards[1].report, /From the agent, What it would remember: Their cat is Biscuit\./);
    unsubscribe();
    // An answer that isn't the history changes nothing.
    const other = connected();
    const otherAsked = other.transport.sent.find(frame => frame.type === 'action' && frame.action.action === 'list_decisions');
    answer(other.transport, otherAsked.request_id, { result: 'refused', reason: 'not_granted' });
    await flush();
    assert.deepEqual(other.adapter.taskApprovals.getSnapshot(), []);
});
