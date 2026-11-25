import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { gunzipSync } from 'node:zlib';
import { ScriptedAdapter } from '../../dist/app/adapters/scripted-adapter.js';
import { createFixtures } from '../../dist/app/adapters/fixtures.js';
import { startBrowserWatch, reduceBrowserSession } from '../../dist/app/adapters/browser-fixtures.js';
import { relativeChatTime } from '../../dist/app/features/chat/chat-navigation.js';
import { turnAction } from '../../dist/app/features/chat/chat.js';
import { clampSize } from '../../dist/app/components/resize.js';
import { browserLabel } from '../../dist/app/features/browser/session.js';
import { attachmentLabel } from '../../dist/app/features/chat/message-actions.js';
import { parseCommand } from '../../dist/app/protocol/codec.js';
const make = () => new ScriptedAdapter({ animate: false });
const start = async (a) => { await a.execute({ type: 'demo.scenario', scenario: 'browser-watch' }); return a.getSnapshot().browserTasks.find(t => t.frame).id; };
const move = (a, id, sequence) => a.execute({ type: 'browser.advance', id, sequence });
const current = (a, id) => a.getSnapshot().browserTasks.find(t => t.id === id);
const saved = () => { const map = new Map(); return { getItem: k => map.get(k) ?? null, setItem: (k, v) => map.set(k, v), removeItem: k => map.delete(k) }; };
test('hover recency uses the supplied clock and does not fabricate absent or invalid times', () => {
    const now = '2026-09-25T10:00:00Z';
    assert.equal(relativeChatTime('2026-09-25T09:25:00Z', now), '35m');
    assert.equal(relativeChatTime('2026-09-25T09:00:00Z', now), '1h');
    assert.equal(relativeChatTime(now, now), 'Now');
    for (const at of [undefined, 'bad', '2027-01-01'])
        assert.equal(relativeChatTime(at, now), '');
    assert.equal(relativeChatTime(now, 'bad'), '');
    assert.equal(relativeChatTime('2026-09-23T10:00:00Z', now), '2d');
});
test('resize bounds are finite, clamped and deterministic even in tiny windows', () => {
    assert.equal(clampSize(200, 240, 420), 240);
    assert.equal(clampSize(800, 240, 420), 420);
    assert.equal(clampSize(NaN, 240, 420), 240);
    assert.equal(clampSize(300, 240, 200), 240);
});
test('normal initial conversation has no unsolicited reply-choice buttons', () => {
    assert.equal(createFixtures().messages.filter(m => m.chatId === 'main' && m.kind === 'choice').length, 0);
});
test('explicit bounded-choice scenario is still available', async () => { const a = make(); await a.execute({ type: 'demo.scenario', scenario: 'choices' }); assert.ok(a.getSnapshot().messages.some(m => m.kind === 'choice')); a.dispose(); });
test('name request updates only after submission and preserves the vibe', async () => {
    const a = make(), vibe = a.getSnapshot().settings.vibe;
    await a.execute({ type: 'chat.send', chatId: 'main', text: 'Change my name to Pip' });
    assert.equal(a.getSnapshot().settings.name, 'Pip');
    assert.equal(a.getSnapshot().settings.vibe, vibe);
    assert.equal(a.getSnapshot().messages.at(-1).text, 'Done — you can call me Pip.');
    a.dispose();
});
test('invalid oversized name is rejected transactionally', async () => { const a = make(), before = structuredClone(a.getSnapshot()); assert.equal((await a.execute({ type: 'chat.send', chatId: 'main', text: 'Change my name to ' + 'x'.repeat(61) })).ok, false); assert.deepEqual(a.getSnapshot(), before); a.dispose(); });
test('browser message and activity share one task id and the originating chat', async () => { const a = make(); await a.execute({ type: 'chat.send', chatId: 'reference', text: 'Open your browser and search for reading nooks' }); const t = a.getSnapshot().browserTasks.find(t => t.frame); assert.ok(t); assert.equal(t.chatId, 'reference'); assert.ok(a.getSnapshot().messages.some(m => m.kind === 'browser' && m.taskId === t.id && m.chatId === 'reference')); assert.ok(a.getSnapshot().threads.some(x => x.id === `watch-${t.id}` && x.chatId === 'reference')); a.dispose(); });
test('sample browser advances in sequence and parks before an unapproved challenge', async () => { const a = make(), id = await start(a); await move(a, id, 2); assert.equal(current(a, id).frame.sequence, 0); await move(a, id, 1); await move(a, id, 2); const t = current(a, id); assert.equal(t.state, 'parked'); assert.equal(t.challenge, 'waiting'); assert.equal(a.getSnapshot().turn, 'awaiting'); assert.equal(t.attemptsRemaining, 1); assert.ok(t.frame.image.endsWith('browser-sample-challenge.png')); await move(a, id, 3); assert.equal(current(a, id).frame.sequence, 2); a.dispose(); });
test('takeover freezes frame updates and permits local input; agent input is rejected', async () => { const a = make(), id = await start(a); assert.equal((await a.execute({ type: 'browser.input', id, text: 'sample' })).ok, false); await a.execute({ type: 'browser.takeover', id }); await move(a, id, 1); assert.equal(current(a, id).frame.sequence, 0); assert.equal((await a.execute({ type: 'browser.input', id, text: 'sample' })).ok, true); assert.equal(current(a, id).frame.query, 'sample'); await a.execute({ type: 'browser.resume', id }); await move(a, id, 1); assert.equal(current(a, id).frame.sequence, 1); a.dispose(); });
test('pending challenge cannot be solved by agent or bypassed by return-control', async () => { const a = make(), id = await start(a); await move(a, id, 1); await move(a, id, 2); assert.equal((await a.execute({ type: 'browser.solve', id })).ok, false); await a.execute({ type: 'browser.takeover', id }); assert.equal((await a.execute({ type: 'browser.resume', id })).ok, false); await a.execute({ type: 'browser.solve', id }); assert.equal(current(a, id).controller, 'user'); await move(a, id, 3); assert.equal(current(a, id).frame.sequence, 2); await a.execute({ type: 'browser.resume', id }); await move(a, id, 3); assert.equal(current(a, id).state, 'completed'); a.dispose(); });
test('one-time challenge grant consumes exactly one attempt and completes only the local result', async () => { const a = make(), id = await start(a); await move(a, id, 1); await move(a, id, 2); await a.execute({ type: 'browser.grant', id }); assert.equal(current(a, id).attemptsRemaining, 0); assert.equal(a.getSnapshot().turn, 'working'); assert.equal((await a.execute({ type: 'browser.grant', id })).ok, false); await move(a, id, 3); assert.equal(current(a, id).state, 'completed'); assert.match(current(a, id).lastConfirmedState, /Not a verified web search/); a.dispose(); });
test('stopping keeps the last frame and rejects takeover, input and re-execution', async () => {
    const a = make(), id = await start(a);
    await move(a, id, 1);
    await a.execute({ type: 'browser.cancel', id });
    const frame = structuredClone(current(a, id).frame);
    for (const type of ['browser.resume', 'browser.takeover', 'browser.solve', 'browser.start'])
        assert.equal((await a.execute({ type, id })).ok, false);
    await move(a, id, 2);
    assert.deepEqual(current(a, id).frame, frame);
    assert.equal(browserLabel(current(a, id)), 'Stopped');
    a.dispose();
});
test('queued tasks cannot take over or enter the occupied browser slot', () => {
    const state = createFixtures();
    state.browserTasks = [];
    startBrowserWatch(state);
    state.revision++;
    startBrowserWatch(state);
    const queued = state.browserTasks.at(-1);
    assert.equal(queued.state, 'queued');
    for (const type of ['browser.takeover', 'browser.resume', 'browser.start'])
        assert.equal(reduceBrowserSession(state, { type, id: queued.id }).ok, false);
    reduceBrowserSession(state, { type: 'browser.cancel', id: state.browserTasks[0].id });
    assert.equal(reduceBrowserSession(state, { type: 'browser.start', id: queued.id }).ok, true);
});
test('reload parks the browser and unrelated commands never advance it', async () => { const storage = saved(), a = new ScriptedAdapter({ storage, animate: false }), id = await start(a); await move(a, id, 1); a.dispose(); const b = new ScriptedAdapter({ storage, animate: false }); assert.equal(current(b, id).state, 'parked'); assert.equal(current(b, id).controller, 'user'); await b.execute({ type: 'chat.read', id: 'main' }); await move(b, id, 2); assert.equal(current(b, id).frame.sequence, 1); await b.execute({ type: 'browser.resume', id }); await move(b, id, 2); assert.equal(current(b, id).state, 'parked'); b.dispose(); });
test('composer Stop also pauses a framed browser task', async () => { const a = make(), id = await start(a); await a.execute({ type: 'turn.interrupt' }); await move(a, id, 1); assert.equal(current(a, id).controller, 'user'); assert.equal(current(a, id).frame.sequence, 0); a.dispose(); });
test('completed browser has a completed label rather than misleading stopped feedback', async () => { const a = make(), id = await start(a); await move(a, id, 1); await move(a, id, 2); await a.execute({ type: 'browser.grant', id }); await move(a, id, 3); assert.equal(browserLabel(current(a, id)), 'Completed'); a.dispose(); });
test('native archive is a real gzip and contains only the named illustrative files', () => { const bytes = readFileSync(new URL('../../public/media/hatchling-sample-handoff.tar.gz', import.meta.url)); assert.equal(bytes[0], 0x1f); assert.equal(bytes[1], 0x8b); const tar = gunzipSync(bytes).toString(); assert.match(tar, /check_sample_bundle.py/); assert.match(tar, /bundle-manifest.md/); assert.match(tar, /illustrative|illustration|UI|local/i); assert.equal(attachmentLabel({ format: 'archive', title: 'Sample handoff tarball' }), 'GZ'); });
test('live activity keeps every child step in its own lane with a stable pending-to-complete id', async () => {
    const a = make();
    await a.execute({ type: 'demo.scenario', scenario: 'bundle-files' });
    await a.execute({ type: 'demo.advance', step: 8 });
    let t = a.getSnapshot().threads.find(x => x.id === 'live-bundle');
    assert.equal(t.pendingSteps.length, 2);
    assert.ok(t.lanes[1].rows.length >= 9);
    await a.execute({ type: 'demo.advance', step: 9 });
    t = a.getSnapshot().threads.find(x => x.id === 'live-bundle');
    assert.ok(t.lanes[1].rows.some(x => x.id === 'verify-current'));
    for (const l of t.lanes) {
        assert.equal(new Set(l.rows.map(x => x.workId)).size, l.rows.length);
        assert.ok(l.rows.every(x => x.laneId === l.id));
    }
    assert.equal(t.lanes[0].rows.find(x => x.id === 'bundle-sha-failed').status, 'failed');
    a.dispose();
});
test('side-chat delete acknowledgement is exactly the reference wording', async () => { const a = make(); const result = await a.execute({ type: 'chat.delete', id: 'reference' }); assert.equal(result.message, 'Side chat deleted'); assert.ok(!a.getSnapshot().chats.some(x => x.id === 'reference')); a.dispose(); });
test('browser persistence command decoder rejects malformed input', () => {
    for (const cmd of [{ type: 'browser.advance', id: 'x', sequence: '2' }, { type: 'browser.input', id: 'x', text: null }, { type: 'browser.input', id: 2, text: 'q' }])
        assert.equal(parseCommand(cmd), null);
    assert.deepEqual(parseCommand({ type: 'browser.advance', id: 'x', sequence: 2 }), { type: 'browser.advance', id: 'x', sequence: 2 });
});
test('an incomplete or empty name never erases the current identity', async () => {
    const a = make();
    const before = a.getSnapshot().settings.name;
    assert.equal((await a.execute({ type: 'chat.send', chatId: 'main', text: 'Change my name to .' })).ok, false);
    assert.equal(a.getSnapshot().settings.name, before);
    a.dispose();
});
test('avatar request consumes only the next attachment and rejects non-raster content', async () => {
    const a = make();
    await a.execute({ type: 'chat.send', chatId: 'main', text: 'Change my avatar to this picture' });
    const imported = await a.execute({ type: 'artifact.import', title: 'Local portrait.png', format: 'image', content: '', dataUrl: 'data:image/png;base64,aGVsbG8=' });
    const id = imported.id;
    assert.ok(id);
    await a.execute({ type: 'chat.attach', chatId: 'main', artifactId: id, avatarAccent: '#14a0dc' });
    assert.equal(a.getSnapshot().settings.avatarAccent, '#14a0dc');
    assert.equal(a.getSnapshot().settings.accent, 'white');
    await a.execute({ type: 'chat.send', chatId: 'main', text: 'Here is another unrelated file.' });
    await a.execute({ type: 'identity.avatar', value: '' });
    await a.execute({ type: 'chat.attach', chatId: 'main', artifactId: id });
    assert.equal(a.getSnapshot().settings.avatar, '');
    a.dispose();
});
test('the chat offers Stop while a run works, live too, and Resume only in the demo (WO-051)', () => {
    assert.equal(turnAction('live', 'working'), 'stop');
    assert.equal(turnAction('scripted', 'working'), 'stop');
    assert.equal(turnAction('scripted', 'interrupted'), 'resume');
    assert.equal(turnAction('live', 'interrupted'), null);
    for (const mode of ['live', 'scripted'])
        for (const turn of ['idle', 'awaiting', 'stalled'])
            assert.equal(turnAction(mode, turn), null, `${mode} ${turn}`);
});
