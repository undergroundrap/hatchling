import test from 'node:test';
import assert from 'node:assert/strict';
import { createFixtures } from '../../dist/app/adapters/fixtures.js';
import { ScriptedAdapter } from '../../dist/app/adapters/scripted-adapter.js';
import { parseCommand } from '../../dist/app/protocol/codec.js';
import { searchWorkspace } from '../../dist/app/features/search/search-model.js';
import { scheduleGroup } from '../../dist/app/features/context/schedules.js';
const make = () => new ScriptedAdapter({ animate: false });
test('synthetic landing has example chat, inert history, identity documents, and seven unowned jobs', () => {
    const state = createFixtures();
    assert.equal(state.settings.name, 'hatchling');
    assert.equal(state.jobs.filter((job) => job.goalId === null).length, 7);
    assert.ok(state.jobs.some((job) => job.goalId === 'g4'));
    assert.equal(state.approvalHistory.length, 5);
    assert.equal(state.memory.filter((entry) => entry.id.startsWith('identity-')).length, 3);
    assert.ok(state.messages.some((message) => message.id === 'ref-dynamic' && message.chatId === 'reference'));
    assert.equal(state.turn, 'idle');
});
test('empty search yields six deterministic reference recents and does not mutate state', () => {
    const state = createFixtures();
    const before = structuredClone(state);
    assert.deepEqual(searchWorkspace(state, '  ').map((result) => result.id), ['reference', 'chat-0', 'main', 'chat-1', 'chat-2', 'chat-3']);
    assert.deepEqual(state, before);
});
test('visited recents honor caller order and omit archived or missing chats', () => {
    const state = createFixtures();
    state.chats.find((chat) => chat.id === 'chat-0').archived = true;
    assert.deepEqual(searchWorkspace(state, '', ['missing', 'chat-1', 'chat-0', 'reference']).map((item) => item.id), ['chat-1', 'reference']);
});
test('search finds transcript-only phrases with a matching snippet and ignores case', () => {
    const state = createFixtures();
    const matches = searchWorkspace(state, '  A SMALLER CHECKLIST  ');
    assert.ok(matches.some((item) => item.id === 'reference' && item.preview.includes('A smaller checklist')));
    assert.equal(searchWorkspace(state, 'a9z9 nonexistent phrase').length, 0);
});
test('workspace search includes artifacts and memory but excludes trashed artifacts', () => {
    const state = createFixtures();
    assert.ok(searchWorkspace(state, 'Hatchling improvement backlog').some((item) => item.kind === 'artifact' && item.id === 'backlog'));
    state.artifacts.find((item) => item.id === 'backlog').trashUntil = '2026-10-23';
    assert.equal(searchWorkspace(state, 'Hatchling improvement backlog').some((item) => item.kind === 'artifact' && item.id === 'backlog'), false);
    assert.ok(searchWorkspace(state, 'Leave a good handoff').some((item) => item.id === 'identity-soul'));
});
test('identity update is atomic, trims values, and leaves trust state unchanged', async () => {
    const app = make();
    const before = app.getSnapshot();
    assert.equal((await app.execute({ type: 'identity.update', name: '  Pip  ', vibe: '  Kind, precise, curious.  ' })).ok, true);
    const state = app.getSnapshot();
    assert.equal(state.settings.name, 'Pip');
    assert.equal(state.settings.vibe, 'Kind, precise, curious.');
    assert.equal(before.settings.name, 'hatchling');
    assert.deepEqual(state.permissions, before.permissions);
    assert.deepEqual(state.approvalHistory, before.approvalHistory);
});
test('invalid identity never partially changes either field', async () => {
    const app = make();
    const before = app.getSnapshot();
    for (const [name, vibe] of [['', 'a'], ['a', ''], ['a'.repeat(61), 'b'], ['a', 'b'.repeat(241)]]) {
        assert.equal((await app.execute({ type: 'identity.update', name, vibe })).ok, false);
        assert.deepEqual(app.getSnapshot(), before);
    }
});
test('identity and schedule updates survive validated event replay', async () => {
    const values = new Map();
    const storage = { getItem: (key) => values.get(key) ?? null, setItem: (key, value) => values.set(key, value), removeItem: (key) => values.delete(key) };
    const app = new ScriptedAdapter({ animate: false, storage });
    await app.execute({ type: 'identity.update', name: 'Pip', vibe: 'Warm and grounded.' });
    const job = app.getSnapshot().jobs.find((item) => item.id === 'ref-job-heartbeat');
    await app.execute({ ...job, type: 'job.update', schedule: 'Every 45 minutes' });
    const restored = new ScriptedAdapter({ animate: false, storage });
    assert.equal(restored.getSnapshot().settings.name, 'Pip');
    assert.equal(restored.getSnapshot().jobs.find((item) => item.id === job.id).schedule, 'Every 45 minutes');
});
test('schedule grouping follows time or event semantics rather than fixture ids', () => {
    const state = createFixtures();
    assert.equal(state.jobs.filter((job) => job.goalId === null && scheduleGroup(job) === 'Daily').length, 5);
    assert.equal(scheduleGroup(state.jobs.find((job) => job.id === 'ref-job-health')), 'Weekly');
    // Run once, it's neither daily nor weekly; every weekday is daily work.
    assert.equal(scheduleGroup({ ...state.jobs[0], recurring: false }), 'Reminders');
    assert.equal(scheduleGroup({ ...state.jobs[0], recurring: true, schedule: 'Weekdays at 08:00 (America/New_York)' }), 'Daily');
    assert.equal(scheduleGroup({ ...state.jobs[0], recurring: true, schedule: 'Every Friday at 17:30 (America/New_York)' }), 'Weekly');
    assert.equal(scheduleGroup({ ...state.jobs[0], kind: 'hook' }), 'Events');
});
test('editing a schedule preserves ownership and paused state; invalid recurrence is rejected', async () => {
    const app = make();
    const job = app.getSnapshot().jobs.find((item) => item.goalId === 'g4');
    await app.execute({ type: 'job.toggle', id: job.id });
    const result = await app.execute({ ...job, type: 'job.update', schedule: 'Every Friday at 5 pm America/New_York' });
    assert.equal(result.ok, true);
    const updated = app.getSnapshot().jobs.find((item) => item.id === job.id);
    assert.equal(updated.goalId, 'g4');
    assert.equal(updated.health, 'disabled');
    const before = app.getSnapshot();
    assert.equal((await app.execute({ ...job, type: 'job.update', recurring: false, schedule: 'Every hour' })).ok, false);
    assert.deepEqual(app.getSnapshot(), before);
});
test('one-time reference schedule delivers once and does not silently repeat', async () => {
    const app = make();
    const count = app.getSnapshot().notifications.length;
    assert.equal((await app.execute({ type: 'job.run', id: 'ref-job-tour' })).ok, true);
    assert.equal(app.getSnapshot().notifications.length, count + 1);
    assert.equal((await app.execute({ type: 'job.run', id: 'ref-job-tour' })).ok, false);
    assert.equal(app.getSnapshot().notifications.length, count + 1);
});
test('runtime decoder validates identity and edited schedule payloads', () => {
    assert.equal(parseCommand({ type: 'identity.update', name: 'Pip' }), null);
    assert.equal(parseCommand({ type: 'job.update', title: 'Missing id' }), null);
    assert.deepEqual(parseCommand({ type: 'identity.update', name: 'Pip', vibe: 'Direct.' }), { type: 'identity.update', name: 'Pip', vibe: 'Direct.' });
});
