import test from 'node:test';
import assert from 'node:assert/strict';
import { ScriptedAdapter } from '../../dist/app/adapters/scripted-adapter.js';
import { getDownload } from '../../dist/app/adapters/reducers/content.js';
const make = () => new ScriptedAdapter({ animate: false });
const ok = async (app, command) => { const result = await app.execute(command); assert.equal(result.ok, true, result.message); return result; };
const artifact = (app, id) => { const value = app.getSnapshot().artifacts.find((item) => item.id === id); assert.ok(value); return value; };
test('Markdown edits never update an approved snapshot without fresh version approval', async () => {
    const app = make();
    const first = artifact(app, 'backlog');
    await ok(app, { type: 'artifact.publish', id: first.id, approvedVersion: first.version });
    await ok(app, { type: 'artifact.save', id: first.id, content: '# Changed privately' });
    assert.equal(app.getSnapshot().publications[0].content, first.content);
    assert.equal((await app.execute({ type: 'artifact.publish', id: first.id, approvedVersion: first.version })).ok, false);
    await ok(app, { type: 'artifact.publish', id: first.id, approvedVersion: artifact(app, first.id).version });
    assert.equal(app.getSnapshot().publications.at(-1).content, '# Changed privately');
});
test('interactive apps are private, editable as app data, and never publishable or offline documents', async () => {
    const app = make();
    const item = app.getSnapshot().artifacts.find((entry) => entry.kind === 'app');
    await ok(app, { type: 'artifact.app-toggle', id: item.id, index: 0 });
    assert.match(artifact(app, item.id).content, /^\[x\]/);
    assert.equal((await app.execute({ type: 'artifact.publish', id: item.id, approvedVersion: item.version })).ok, false);
    assert.equal((await app.execute({ type: 'artifact.download', id: item.id })).ok, false);
});
test('documents support recoverable trash, restore, favorite, and safe download', async () => {
    const app = make();
    await ok(app, { type: 'artifact.pin', id: 'backlog' });
    assert.equal(artifact(app, 'backlog').pinned, true);
    await ok(app, { type: 'artifact.trash', id: 'backlog' });
    assert.equal(artifact(app, 'backlog').trashUntil, '2026-10-23');
    await ok(app, { type: 'artifact.restore', id: 'backlog' });
    assert.equal(artifact(app, 'backlog').trashUntil, null);
    const result = await ok(app, { type: 'artifact.download', id: 'backlog' });
    assert.match(result.download.filename, /\.md$/);
    assert.match(result.download.text, /^# Hatchling/);
});
test('non-Markdown documents are view-only', async () => {
    const app = make();
    for (const id of ['roadmap', 'budget'])
        assert.equal((await app.execute({ type: 'artifact.save', id, content: 'changed' })).ok, false);
});
test('slide save clamps layer geometry and rebuilds escaped offline HTML', async () => {
    const app = make();
    await ok(app, { type: 'artifact.save-slide', id: 'deck', slide: 0, layerId: 's1a', text: '<script>not executable</script>', x: 300, y: -4, width: 110, size: 200 });
    const item = artifact(app, 'deck');
    const layer = item.slides[0][0];
    assert.deepEqual([layer.x, layer.y, layer.width, layer.size], [0, 0, 100, 80]);
    const download = getDownload(structuredClone(item));
    assert.match(download.filename, /\.html$/);
    assert.match(download.text, /&lt;script&gt;/);
    // No script element in any spelling: `<SCRIPT`, `< script`, `<script src=…>`.
    assert.doesNotMatch(download.text, /<\s*script/i);
});
test('imported data assets download with a correct extension, not a base64 filename', async () => {
    const app = make();
    const result = await ok(app, { type: 'artifact.import', title: 'My image', format: 'image', content: '', dataUrl: 'data:image/png;base64,aGVsbG8=' });
    const download = getDownload(structuredClone(artifact(app, result.id)));
    assert.equal(download.filename, 'my-image.png');
    assert.equal(download.mime, 'image/png');
    assert.equal((await app.execute({ type: 'artifact.import', title: 'Unsafe', format: 'html', content: '', dataUrl: 'data:text/html;base64,aGVsbG8=' })).ok, false);
});
test('all seven pulse shapes are represented with dated countdown evidence', () => {
    const app = make();
    const goals = app.getSnapshot().goals;
    assert.deepEqual([...new Set(goals.map((goal) => goal.pulseKind))].sort(), ['blocked', 'countdown', 'milestone', 'monitoring', 'rhythm', 'scheduled', 'waiting']);
    const countdown = goals.find((goal) => goal.pulseKind === 'countdown');
    assert.ok(countdown.due);
    assert.match(countdown.timeline[0].text, /Sep 25/);
});
test('goal creation, update, close, reopen and owned-job disable are explicit', async () => {
    const app = make();
    const result = await ok(app, { type: 'goal.create', title: 'Read a short book', description: 'Finish the first chapter', category: 'Interests', due: '2026-09-30' });
    await ok(app, { type: 'goal.update', id: result.id, title: 'Read two chapters', description: 'Keep a note', due: '2026-10-01' });
    await ok(app, { type: 'goal.close', id: result.id });
    assert.equal(app.getSnapshot().goals.find((goal) => goal.id === result.id).status, 'closed');
    await ok(app, { type: 'goal.reopen', id: result.id });
    assert.equal(app.getSnapshot().goals.find((goal) => goal.id === result.id).status, 'open');
    const owned = app.getSnapshot().jobs.find((job) => job.goalId);
    assert.ok(owned);
    await ok(app, { type: 'goal.close', id: owned.goalId });
    assert.equal(app.getSnapshot().jobs.find((job) => job.id === owned.id).health, 'disabled');
});
test('ideas cover ten domains and save concrete completion evidence', async () => {
    const app = make();
    assert.equal(new Set(app.getSnapshot().ideas.map((idea) => idea.domain)).size, 10);
    for (const idea of app.getSnapshot().ideas.filter((item) => item.sampleOnly)) {
        assert.match(idea.title, /^I can /);
        assert.match(idea.signal, /^Sample fact:/);
    }
    await ok(app, { type: 'idea.activate', id: 'idea-0' });
    await ok(app, { type: 'demo.advance', step: 4 });
    assert.equal(app.getSnapshot().ideas.find((item) => item.id === 'idea-0').state, 'executing');
    await ok(app, { type: 'demo.advance', step: 5 });
    assert.equal(app.getSnapshot().ideas.find((item) => item.id === 'idea-0').state, 'completed');
    assert.ok(artifact(app, app.getSnapshot().ideas.find((item) => item.id === 'idea-0').artifactId));
});
test('device commands target a named reachable capability and never silently reroute', async () => {
    const app = make();
    const offline = app.getSnapshot().devices.find((device) => !device.reachable);
    const online = app.getSnapshot().devices.find((device) => device.reachable);
    assert.equal((await app.execute({ type: 'device.command', id: offline.id, capability: offline.capabilities[0] })).ok, false);
    assert.equal((await app.execute({ type: 'device.command', id: online.id, capability: 'Unadvertised action' })).ok, false);
    const count = app.getSnapshot().messages.length;
    await ok(app, { type: 'device.command', id: online.id, capability: online.capabilities[0] });
    assert.equal(app.getSnapshot().messages.length, count);
    assert.match(app.getSnapshot().notifications[0].title, new RegExp(online.name));
});
test('source logs are append-only but user-facing memory notes can be edited and removed', async () => {
    const app = make();
    const log = app.getSnapshot().memory.find((entry) => entry.kind === 'logs');
    assert.equal((await app.execute({ type: 'memory.save', id: log.id, title: 'Overwrite', body: 'No' })).ok, false);
    assert.equal((await app.execute({ type: 'memory.delete', id: log.id })).ok, false);
    await ok(app, { type: 'memory.save', id: '', title: 'A useful preference', body: 'Leave Sunday open.' });
    const note = app.getSnapshot().memory[0];
    await ok(app, { type: 'memory.delete', id: note.id });
    assert.equal(app.getSnapshot().memory.some((entry) => entry.id === note.id), false);
});
test('one-shot schedules deliver once and then disable; recurrence needs separate authorization', async () => {
    const app = make();
    const base = { type: 'job.create', title: 'A small reminder', kind: 'time', schedule: 'Sep 24 at 9 am', recurring: false, source: '', event: '' };
    assert.equal((await app.execute({ ...base, schedule: 'Every day at 9 am' })).ok, false);
    const result = await ok(app, base);
    const before = app.getSnapshot().notifications.length;
    await ok(app, { type: 'job.run', id: result.id });
    assert.equal(app.getSnapshot().notifications.length, before + 1);
    assert.equal((await app.execute({ type: 'job.run', id: result.id })).ok, false);
    assert.equal(app.getSnapshot().notifications.length, before + 1);
});
test('event hooks require both a connected-source identifier and event description', async () => {
    const app = make();
    const base = { type: 'job.create', title: 'Change alert', kind: 'hook', schedule: 'When an important update arrives', recurring: true, source: '', event: '' };
    assert.equal((await app.execute(base)).ok, false);
    await ok(app, { ...base, source: 'github', event: 'A pull request changes' });
});
