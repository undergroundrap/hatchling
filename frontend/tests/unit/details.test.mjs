import test from 'node:test';
import assert from 'node:assert/strict';
import { createFixtures } from '../../dist/app/adapters/fixtures.js';
import { ScriptedAdapter } from '../../dist/app/adapters/scripted-adapter.js';
import { startCi, advanceCi } from '../../dist/app/adapters/activity-fixtures.js';
import { parseCommand } from '../../dist/app/protocol/codec.js';
import { accents, accentColors } from '../../dist/app/features/settings/appearance.js';
import { timelineGroups, durationLabel, timeLabel, reportTitleAndBody } from '../../dist/app/features/activity/timeline.js';
import { agentStatus } from '../../dist/app/features/context/agent-status.js';
import { safeLink } from '../../dist/app/components/markdown.js';
import { animationLibrary } from '../../dist/app/components/animations.js';
import { sampleProperty } from '../../dist/app/components/lottie-player.js';
const make = () => new ScriptedAdapter({ animate: false });
const memoryStorage = () => {
    const map = new Map();
    return { getItem: (key) => map.get(key) ?? null, setItem: (key, value) => map.set(key, value), removeItem: (key) => map.delete(key) };
};
const trace = () => createFixtures().threads.find((thread) => thread.id === 'dynamic04');
function luminance(hex) {
    const [r, g, b] = [1, 3, 5].map((index) => parseInt(hex.slice(index, index + 2), 16) / 255).map((value) => value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4);
    return r * .2126 + g * .7152 + b * .0722;
}
test('fresh settings default to white personalization and dark mode, whatever the OS appearance', () => {
    const state = createFixtures();
    assert.equal(state.settings.accent, 'white');
    assert.equal(state.settings.theme, 'dark');
    assert.equal(state.settings.keepChatPanelVisible, true);
    assert.equal(accentColors(state.settings, false).background, '#f1f1f1');
    assert.equal(accentColors(state.settings, true).background, '#e4e7eb');
});
test('accent and chat-panel commands validate at the persisted-data boundary', () => {
    for (const choice of accents)
        assert.deepEqual(parseCommand({ type: 'settings.accent', value: choice.id }), { type: 'settings.accent', value: choice.id });
    for (const value of ['white ', 'random', '#000000', {}, null])
        assert.equal(parseCommand({ type: 'settings.accent', value }), null);
    assert.equal(parseCommand({ type: 'settings.chat-panel', value: 'false' }), null);
    assert.equal(parseCommand({ type: 'chat.read', id: 3 }), null);
    assert.deepEqual(parseCommand({ type: 'settings.chat-panel', value: false }), { type: 'settings.chat-panel', value: false });
});
test('all personalization colors choose an AA-readable foreground in dark and light mode', () => {
    for (const choice of accents) {
        for (const light of [false, true]) {
            const pair = accentColors({ accent: choice.id, avatarAccent: '#a78c57' }, light);
            const a = luminance(pair.background), b = luminance(pair.foreground);
            assert.ok((Math.max(a, b) + .05) / (Math.min(a, b) + .05) >= 4.5, `${choice.id}/${light}`);
        }
    }
});
test('avatar match uses supplied validated palette and never treats it as style markup', () => {
    assert.equal(accentColors({ accent: 'avatar', avatarAccent: '#123AbC' }, false).background, '#123AbC');
    for (const avatarAccent of ['url(javascript:alert(1))', 'red;display:none', '#fff', '#zzzzzz'])
        assert.equal(accentColors({ accent: 'avatar', avatarAccent }, false).background, '#a78c57');
});
test('personal appearance and pin choices replay without replacing existing v3 data', async () => {
    const storage = memoryStorage();
    const first = new ScriptedAdapter({ animate: false, storage });
    await first.execute({ type: 'identity.update', name: 'Pip', vibe: 'Evidence first.' });
    await first.execute({ type: 'settings.accent', value: 'avatar' });
    await first.execute({ type: 'settings.chat-panel', value: false });
    const next = new ScriptedAdapter({ animate: false, storage });
    assert.equal(next.getSnapshot().settings.name, 'Pip');
    assert.equal(next.getSnapshot().settings.accent, 'avatar');
    assert.equal(next.getSnapshot().settings.keepChatPanelVisible, false);
    await next.execute({ type: 'demo.reset' });
    assert.equal(next.getSnapshot().settings.accent, 'white');
});
test('rich reference trace has mandatory evidence, timing, lane ownership, and six-ms inspection', () => {
    const thread = trace();
    assert.ok(thread);
    for (const lane of thread.lanes) {
        for (const row of lane.rows) {
            for (const field of ['title', 'subtitle', 'icon', 'report', 'status', 'workId', 'startedAt', 'endedAt'])
                assert.ok(row[field], `${row.id}/${field}`);
            assert.equal(row.laneId, lane.id);
            assert.equal(Date.parse(row.endedAt) - Date.parse(row.startedAt), row.durationMs);
        }
    }
    const row = thread.lanes.flatMap((lane) => lane.rows).find((item) => item.title === 'Inspected sample collection contents');
    assert.equal(row.durationMs, 6);
    assert.match(row.report, /successful inspection lasting 6 ms/);
});
test('timeline groups parent first and leaves child work out of the parent lane', () => {
    const thread = trace();
    const groups = timelineGroups(thread);
    assert.equal(groups[0].lane.parentId, null);
    assert.equal(groups[1].ordinal, 1);
    assert.ok(groups[0].rows.some((row) => row.phase === 'dispatch'));
    assert.ok(groups[1].rows.some((row) => row.phase === 'start'));
    const childIds = groups[1].rows.map((row) => row.id);
    assert.ok(groups[0].rows.every((row) => !childIds.includes(row.id)));
});
test('timeline deduplication replaces same-work revisions but not same-looking distinct work', () => {
    const thread = trace();
    const lane = thread.lanes[1];
    const row = lane.rows[0];
    lane.rows.push({ ...row, id: 'revised', subtitle: 'Revised evidence for the same concrete work' });
    lane.rows.push({ ...row, id: 'different', workId: 'different' });
    lane.rows.push({ ...row, id: 'misowned', workId: 'misowned', laneId: 'wrong-lane' });
    const rows = timelineGroups(thread)[1].rows;
    assert.equal(rows.filter((item) => item.workId === row.workId).length, 1);
    assert.equal(rows.find((item) => item.workId === row.workId).id, 'revised');
    assert.ok(rows.some((item) => item.id === 'different'));
    assert.ok(!rows.some((item) => item.id === 'misowned'));
});
test('timeline timestamps sort chronologically, with no invented values for missing evidence', () => {
    const thread = trace();
    thread.lanes[1].rows.reverse();
    const rows = timelineGroups(thread)[1].rows;
    assert.deepEqual(rows.map((row) => row.startedAt), rows.map((row) => row.startedAt).toSorted());
    assert.equal(timeLabel(null), 'Time not recorded');
    assert.equal(timeLabel('garbage'), 'Time not recorded');
    assert.equal(durationLabel(undefined), 'Not recorded');
    assert.equal(durationLabel(-1), 'Not recorded');
    assert.equal(durationLabel(Infinity), 'Not recorded');
});
test('duration and UTC formatting retain the reported precision', () => {
    assert.equal(timeLabel('2026-09-25T09:41:20.006Z'), '09:41:20.006');
    assert.equal(durationLabel(0), '0 ms');
    assert.equal(durationLabel(6), '6 ms');
    assert.equal(durationLabel(4200), '4.2 s');
    assert.equal(durationLabel(86420), '1 min 26.4 s');
});
test('reports remove only a duplicate heading, not independent evidence headings', () => {
    const row = trace().lanes[1].rows[0];
    assert.deepEqual(reportTitleAndBody({ ...row, title: 'Read note', report: '# Read note\n\nEvidence.' }), { title: 'Read note', body: 'Evidence.' });
    assert.equal(reportTitleAndBody({ ...row, title: 'Read note', report: '## Evidence\n\nContent.' }).body, '## Evidence\n\nContent.');
});
test('summary and per-step artifact references resolve to local downloadable evidence', async () => {
    const adapter = make();
    const state = adapter.getSnapshot();
    const thread = state.threads.find((item) => item.id === 'dynamic04');
    const ids = new Set([...thread.evidenceArtifactIds, ...thread.lanes.flatMap((lane) => lane.rows.flatMap((row) => row.artifactIds ?? []))]);
    for (const id of ids) {
        assert.ok(state.artifacts.some((artifact) => artifact.id === id));
        const result = await adapter.execute({ type: 'artifact.download', id });
        assert.equal(result.ok, true);
        assert.ok(result.download);
    }
    assert.match(state.artifacts.find((item) => item.id === 'dynamic-loader').content, /illustrat|scripted/i);
});
test('agent status is Connected only at rest and uses concrete live status titles', () => {
    const state = createFixtures();
    assert.equal(agentStatus(state).label, 'Connected');
    startCi(state);
    assert.equal(agentStatus(state).label, 'Fixing CI');
    assert.equal(agentStatus(state).threadId, 'live-ci');
    state.turn = 'idle';
    assert.equal(agentStatus(state).label, 'Fixing CI', 'background work is still active');
    advanceCi(state, 6);
    assert.equal(agentStatus(state).label, 'Checking tests');
    advanceCi(state, 7);
    assert.equal(agentStatus(state).label, 'Connected');
});
test('pause, stall, and approval override cosmetic running text', () => {
    const state = createFixtures();
    startCi(state);
    state.turn = 'awaiting';
    assert.equal(agentStatus(state).label, 'Waiting for confirmation');
    state.turn = 'stalled';
    assert.equal(agentStatus(state).kind, 'waiting');
    state.turn = 'interrupted';
    assert.equal(agentStatus(state).label, 'Work is paused');
});
test('browser task status and takeover are surfaced when no agent thread is running', () => {
    const state = createFixtures();
    state.browserTasks[0].state = 'running';
    state.browserTasks[0].statusTitle = 'Checking delivery';
    assert.equal(agentStatus(state).label, 'Checking delivery');
    state.browserTasks[0].controller = 'user';
    assert.equal(agentStatus(state).label, 'Waiting for you');
    state.browserTasks[0].controller = 'agent';
    state.browserTasks[0].state = 'parked';
    assert.equal(agentStatus(state).kind, 'waiting');
});
test('CI completion delivers exactly once to its own side chat and sets that chat unread', async () => {
    const app = make();
    await app.execute({ type: 'chat.read', id: 'chat-0' });
    await app.execute({ type: 'chat.read', id: 'reference' });
    await app.execute({ type: 'demo.scenario', scenario: 'fix-ci' });
    assert.equal(app.getSnapshot().chats.find((chat) => chat.id === 'chat-0').unread, false);
    await app.execute({ type: 'demo.advance', step: 6 });
    assert.equal(app.getSnapshot().chats.find((chat) => chat.id === 'chat-0').unread, false);
    await app.execute({ type: 'demo.advance', step: 7 });
    await app.execute({ type: 'demo.advance', step: 7 });
    assert.equal(app.getSnapshot().messages.filter((message) => message.id.startsWith('ci-result-')).length, 1);
    assert.equal(app.getSnapshot().chats.find((chat) => chat.id === 'chat-0').unread, true);
    assert.equal(app.getSnapshot().chats.find((chat) => chat.id === 'reference').unread, false);
    await app.execute({ type: 'chat.read', id: 'chat-0' });
    assert.equal(app.getSnapshot().chats.find((chat) => chat.id === 'chat-0').unread, false);
});
test('mark read is scoped, replayable, and does not delete any transcript', async () => {
    const storage = memoryStorage();
    const app = new ScriptedAdapter({ storage, animate: false });
    await app.execute({ type: 'chat.send', chatId: 'chat-0', text: 'Hello' });
    const before = app.getSnapshot().messages.length;
    assert.equal(app.getSnapshot().chats.find((chat) => chat.id === 'chat-0').unread, true);
    assert.equal((await app.execute({ type: 'chat.read', id: 'missing' })).ok, false);
    await app.execute({ type: 'chat.read', id: 'chat-0' });
    const next = new ScriptedAdapter({ storage, animate: false });
    assert.equal(next.getSnapshot().chats.find((chat) => chat.id === 'chat-0').unread, false);
    assert.equal(next.getSnapshot().messages.length, before);
});
test('interrupted CI neither advances nor claims connected; resume restarts saved work', async () => {
    const app = make();
    await app.execute({ type: 'demo.scenario', scenario: 'fix-ci' });
    await app.execute({ type: 'turn.interrupt' });
    await app.execute({ type: 'demo.advance', step: 7 });
    assert.equal(agentStatus(app.getSnapshot()).label, 'Work is paused');
    assert.equal(app.getSnapshot().threads[0].state, 'interrupted');
    await app.execute({ type: 'turn.resume' });
    assert.equal(agentStatus(app.getSnapshot()).label, 'Fixing CI');
    await app.execute({ type: 'demo.advance', step: 7 });
    assert.equal(agentStatus(app.getSnapshot()).label, 'Connected');
});
test('reload of incomplete CI reports paused instead of claiming a live remote worker', async () => {
    const storage = memoryStorage();
    const app = new ScriptedAdapter({ storage, animate: false });
    await app.execute({ type: 'demo.scenario', scenario: 'fix-ci' });
    const restored = new ScriptedAdapter({ storage, animate: false });
    assert.equal(restored.getSnapshot().threads[0].state, 'interrupted');
    assert.equal(agentStatus(restored.getSnapshot()).label, 'Work is paused');
});
test('Markdown allows safe explicit URLs, rejects code schemes, controls, and relative guesses', () => {
    assert.equal(safeLink('https://example.com/report'), 'https://example.com/report');
    assert.equal(safeLink('mailto:person@example.com'), 'mailto:person@example.com');
    for (const value of ['javascript:alert(1)', 'data:text/html,evil', 'file:///etc/passwd', '//example.com', '/report', 'https://example.com/ space', '\nhttps://example.com', 'java\tscript:evil'])
        assert.equal(safeLink(value), null, value);
});
test('authored Lottie documents have bounded frame counts and no external images or expressions', () => {
    assert.equal(Object.keys(animationLibrary).length, 8);
    for (const [name, animation] of Object.entries(animationLibrary)) {
        assert.equal(animation.fr, 60);
        assert.equal(animation.w, 24);
        assert.equal(animation.h, 24);
        assert.equal(animation.ip, 0);
        assert.equal(animation.op, name === 'search' ? 51 : name === 'arrow' ? 141 : name === 'spark' ? 300 : 65);
        assert.deepEqual(animation.assets, []);
        assert.ok(animation.layers.every((layer) => layer.ty === 4));
        assert.doesNotMatch(JSON.stringify(animation), /https?:|data:|expression|\.woff/i);
    }
});
test('Lottie property interpolation is bounded and respects static, linear, and hold values', () => {
    assert.deepEqual(sampleProperty({ a: 0, k: 12 }, 19), [12]);
    assert.deepEqual(sampleProperty({ a: 0, k: [12, 24] }, 19), [12, 24]);
    const property = { a: 1, k: [{ t: 0, s: [0, 10] }, { t: 10, s: [10, 20] }] };
    assert.deepEqual(sampleProperty(property, -1), [0, 10]);
    assert.deepEqual(sampleProperty(property, 5), [5, 15]);
    assert.deepEqual(sampleProperty(property, 20), [10, 20]);
    property.k[0].h = 1;
    assert.deepEqual(sampleProperty(property, 9), [0, 10]);
});
test('code evidence downloads keep source-language extensions rather than appending .ts', async () => {
    const app = make();
    for (const [id, name] of [['dynamic-loader', 'loader.py'], ['dynamic-sites', 'sample-entries.json']]) {
        const result = await app.execute({ type: 'artifact.download', id });
        assert.equal(result.download.filename, name);
    }
});
