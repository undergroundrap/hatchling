// the interactive surfaces on the live adapter (agent status,
// artifacts and files, the live browser and who is driving), end to end over a
// fake transport with the typed fixtures in src/adapters/live/surface-fixtures.ts.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { LiveAdapter } from '../../dist/app/adapters/live/adapter.js';
import { ArtifactShelf, fileFormat, imageFromUrl, imageUrl } from '../../dist/app/adapters/live/artifacts.js';
import { conforms } from '../../dist/app/adapters/live/protocol/conform.js';
import { controlAfter, drives, switching } from '../../dist/app/adapters/live/control.js';
import { SAMPLE_WELCOME, sampleEvent, sampleTime } from '../../dist/app/adapters/live/fixtures.js';
import { CONTROL_TRANSITIONS } from '../../dist/app/adapters/live/protocol/generated.js';
import { HOSTILE_MARKDOWN, SAMPLE_PNG, SURFACE_EVENTS, openedText, sampleArtifact, sampleSession } from '../../dist/app/adapters/live/surface-fixtures.js';
import { markdown } from '../../dist/app/components/markdown.js';
import { agentStatus } from '../../dist/app/features/context/agent-status.js';
import { browserLabel, controlPill, pagePoint, switchingControl } from '../../dist/app/features/browser/session.js';
class FakeTransport {
    sent = [];
    closes = 0;
    handlers = [];
    connect(handlers) { this.handlers.push(handlers); }
    send(text) { this.sent.push(JSON.parse(text)); }
    close() { this.closes++; }
    deliver(frame) { this.handlers.at(-1).received(JSON.stringify(frame)); }
    open() { this.handlers.at(-1).opened(); }
    last() { return this.sent.at(-1); }
    actions() { return this.sent.filter(frame => frame.type === 'action'); }
}
const connected = (frames = SURFACE_EVENTS) => {
    const transport = new FakeTransport();
    const adapter = new LiveAdapter(transport, 'test-ui');
    transport.open();
    transport.deliver(SAMPLE_WELCOME);
    for (const frame of frames)
        transport.deliver(frame);
    return { transport, adapter };
};
let seq = 100;
const event = body => sampleEvent(++seq, 30, body);
const flush = () => new Promise(resolve => setImmediate(resolve));
/** Run a command and answer the action it sends with `result`. */
const answered = async (transport, adapter, command, result, send = () => adapter.execute(command)) => {
    const pending = send();
    await flush();
    const sent = transport.last();
    transport.deliver({ type: 'action_result', request_id: sent.request_id, result });
    return { sent: sent.action, outcome: await pending };
};
const task = (adapter, id = 'web-1') => adapter.getSnapshot().browserTasks.find(item => item.id === id);
const control = (session_id, handoff, frame_sequence = 1) => event({ kind: 'browser_control', session_id, handoff, frame_sequence });
// ---- A minimal DOM, as in live-adapter.test.mjs.
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
globalThis.document = { createElement: tag => new FakeElement(tag), createElementNS: (namespace, tag) => Object.assign(new FakeElement(tag), { namespaceURI: namespace }), createTextNode: data => new FakeText(data), createDocumentFragment: () => new FakeNode() };
const walk = (node, out = { tags: [], text: '', hrefs: [], srcs: [] }) => {
    if (node instanceof FakeText)
        out.text += node.data;
    if (node instanceof FakeElement) {
        out.tags.push(node.tagName);
        for (const [name, list] of [['href', out.hrefs], ['src', out.srcs]])
            if (node.attributes.has(name))
                list.push(node.attributes.get(name));
    }
    for (const child of node.childNodes)
        walk(child, out);
    return out;
};
test('avatar states map 1:1 to the agent-status events, and nothing else sets them', () => {
    // Every state the schema defines, against the frozen turn states the avatar reads.
    const schema = JSON.parse(readFileSync(new URL('../../protocol/schema.json', import.meta.url), 'utf8'));
    const states = schema.$defs.AgentState.oneOf.map(option => option.const);
    assert.deepEqual(states, ['idle', 'working', 'awaiting_approval', 'paused', 'stalled']);
    const turns = [];
    const labels = [];
    for (const state of states) {
        const { transport, adapter } = connected([]);
        transport.deliver(event({ kind: 'agent_status', state, detail: 'x' }));
        const snapshot = adapter.getSnapshot();
        turns.push(snapshot.turn);
        labels.push(agentStatus({ ...snapshot, threads: [], browserTasks: [] }).label);
    }
    assert.deepEqual(turns, ['idle', 'working', 'awaiting', 'interrupted', 'stalled']);
    assert.equal(new Set(labels).size, 5, 'each state reads differently on the status line');
    // No optimistic state: running activity, a browser session or a pending send never set it.
    const { transport, adapter } = connected([SURFACE_EVENTS[4]]);
    void adapter.execute({ type: 'chat.send', chatId: 'main', text: 'Hi' });
    assert.equal(adapter.getSnapshot().turn, 'idle');
    transport.deliver(event({ kind: 'agent_status', state: 'working', detail: 'Now it is working' }));
    assert.equal(adapter.getSnapshot().turn, 'working');
});
test('artifacts: the listing becomes the views\' records, every format mapped, content loaded on open', () => {
    const formats = ['markdown', 'text', 'csv', 'code', 'html', 'image', 'pdf', 'document', 'spreadsheet'];
    const { adapter } = connected(formats.map(format => event({ kind: 'artifact_listed', ...sampleArtifact(format, format, 2, format === 'pdf' ? 2 : null) })));
    const records = adapter.getSnapshot().artifacts;
    assert.deepEqual(records.map(item => [item.id, item.kind, item.format]), [
        ['markdown', 'document', 'markdown'], ['text', 'document', 'text'], ['csv', 'document', 'csv'], ['code', 'document', 'code'],
        ['html', 'document', 'code'], ['image', 'media', 'image'], ['pdf', 'document', 'pdf'], ['document', 'document', 'word'], ['spreadsheet', 'document', 'spreadsheet'],
    ]);
    const [first] = records;
    assert.deepEqual([first.version, first.publishedVersion, first.created, first.content, first.asset, first.description, first.title], [2, null, sampleTime(0), '', null, 'Open it to load its content.', 'Sample markdown file']);
    assert.equal(records.find(item => item.id === 'pdf').publishedVersion, 2);
    assert.ok(records.every(item => item.kind !== 'app'), 'the scripted sample app is never offered live');
    // A listing with no revisions claims none.
    const empty = connected([event({ kind: 'artifact_listed', ...sampleArtifact('empty', 'text', 0) })]);
    assert.deepEqual(['version', 'created'].map(key => empty.adapter.getSnapshot().artifacts[0][key]), [0, '']);
});
test('artifact panel: open, version and share are typed actions, re-checked when sent', async () => {
    const { transport, adapter } = connected();
    const { sent, outcome } = await answered(transport, adapter, { type: 'artifact.open', id: 'notes' }, openedText('notes', 2, 'Sample notes.'));
    assert.deepEqual(sent, { action: 'open_artifact', artifact_id: 'notes', revision: 2 }, 'the newest revision');
    assert.deepEqual(outcome, { ok: true, message: 'Opened.' });
    assert.equal(adapter.getSnapshot().artifacts[0].content, 'Sample notes.');
    // A later listing keeps what was fetched; a new revision needs fetching again.
    transport.deliver(event({ kind: 'artifact_listed', ...sampleArtifact('notes', 'markdown', 2, 1) }));
    assert.equal(adapter.getSnapshot().artifacts[0].content, 'Sample notes.');
    transport.deliver(event({ kind: 'artifact_listed', ...sampleArtifact('notes', 'markdown', 3) }));
    assert.deepEqual([adapter.getSnapshot().artifacts[0].content, adapter.getSnapshot().artifacts[0].version], ['', 3]);
    // Share the reviewed version only while it is listed.
    const before = transport.actions().length;
    assert.match((await adapter.execute({ type: 'artifact.publish', id: 'notes', approvedVersion: 9 })).message, /no longer listed/);
    assert.equal(transport.actions().length, before, 'nothing unlisted was sent');
    const shared = await answered(transport, adapter, { type: 'artifact.publish', id: 'notes', approvedVersion: 2 }, { result: 'artifact_shared', artifact_id: 'notes', revision: 2 });
    assert.deepEqual([shared.sent, shared.outcome], [{ action: 'share_artifact', artifact_id: 'notes', revision: 2 }, { ok: true, message: 'Shared.' }]);
    assert.match((await answered(transport, adapter, { type: 'artifact.publish', id: 'notes', approvedVersion: 2 }, { result: 'artifact_shared', artifact_id: 'notes', revision: 1 })).outcome.message, /something else/);
    const refusals = { 'unknown-build': /no longer has this version/, incomplete: /not finished/, 'hash-mismatch': /changed after it was made/, blocked: /share review stopped/, 'gate-error': /could not check/, 'bad-target': /cannot share to that place/, 'delivery-failed': /sending the file failed/ };
    for (const [reason, words] of Object.entries(refusals))
        assert.match((await answered(transport, adapter, { type: 'artifact.publish', id: 'notes', approvedVersion: 2 }, { result: 'share_refused', artifact_id: 'notes', reason })).outcome.message, words);
    assert.match((await answered(transport, adapter, { type: 'artifact.publish', id: 'notes', approvedVersion: 2 }, { result: 'artifact_refused', artifact_id: 'notes', reason: 'unknown_revision' })).outcome.message, /no longer has this version of the file/);
    // Opening: refusals, mismatched answers, unknown files.
    assert.match((await answered(transport, adapter, { type: 'artifact.open', id: 'notes' }, { result: 'artifact_refused', artifact_id: 'notes', reason: 'unknown_artifact' })).outcome.message, /no longer has this file/);
    for (const result of [openedText('other', 3, 'x'), openedText('notes', 2, 'x'), { result: 'artifact_saved', artifact_id: 'notes', revision: 3 }])
        assert.match((await answered(transport, adapter, { type: 'artifact.open', id: 'notes' }, result)).outcome.message, /something else/);
    for (const [reason, words] of [['not_connected', /not connected to its broker/], ['bad_request', /could not read the request/], ['unavailable', /cannot do this right now/], ['not_granted', /not allowed to do this/]])
        assert.match((await answered(transport, adapter, { type: 'artifact.open', id: 'notes' }, { result: 'refused', reason })).outcome.message, words);
    assert.match((await adapter.execute({ type: 'artifact.open', id: 'missing' })).message, /no longer available/);
});
test('script and remote content in a markdown artifact render inert, and nothing is fetched (negative test)', async () => {
    const { transport, adapter } = connected();
    await answered(transport, adapter, { type: 'artifact.open', id: 'notes' }, openedText('notes', 2, HOSTILE_MARKDOWN));
    const record = adapter.getSnapshot().artifacts[0];
    const opened = [];
    const out = walk(markdown(record.content, true, { openArtifact: id => opened.push(id) }));
    assert.deepEqual([out.hrefs, out.srcs, opened], [[], [], []], 'no link, no image source, nothing opened');
    for (const tag of ['script', 'img', 'a', 'button', 'iframe'])
        assert.ok(!out.tags.includes(tag), `no <${tag}>`);
    assert.ok(out.text.includes('<script>fetch("https://example.invalid/steal")</script>'), 'the script is shown as text');
    assert.ok(out.text.includes('![tracker] (https://example.invalid/t.png)'), 'a remote image is shown as text, never loaded');
    assert.ok(out.tags.includes('strong') && out.tags.includes('h1'), 'ordinary formatting survives');
    assert.equal(record.asset, null, 'no asset to load');
    // Source formats are shown exactly, and HTML is source, never rendered.
    const source = 'if (a[0](b)) { render("<b>x</b>"); }';
    const code = connected([event({ kind: 'artifact_listed', ...sampleArtifact('page', 'html') })]);
    await answered(code.transport, code.adapter, { type: 'artifact.open', id: 'page' }, openedText('page', 1, source));
    assert.deepEqual([code.adapter.getSnapshot().artifacts[0].format, code.adapter.getSnapshot().artifacts[0].content], ['code', source]);
});
test('images are inline data only; other bodies say why there is no preview', async () => {
    const { transport, adapter } = connected();
    await answered(transport, adapter, { type: 'artifact.open', id: 'photo' }, { result: 'artifact_opened', artifact_id: 'photo', revision: 1, body: { type: 'image', ...SAMPLE_PNG } });
    assert.equal(adapter.getSnapshot().artifacts[1].asset, `data:image/png;base64,${SAMPLE_PNG.base64}`);
    assert.equal(imageUrl({ media_type: 'gif', base64: 'not base64!' }), null);
    for (const [type, mime] of [['jpeg', 'image/jpeg'], ['webp', 'image/webp'], ['gif', 'image/gif']])
        assert.equal(imageUrl({ media_type: type, base64: 'AAAA' }), `data:${mime};base64,AAAA`);
    const bodies = [[{ type: 'unpreviewable', bytes: 3145728 }, /\(3 MB\) has no preview/], [{ type: 'too_large', bytes: 1572864, limit: 1048576 }, /too large to show here: 1\.5 MB, and the limit is 1 MB/]];
    for (const [body, words] of bodies) {
        await answered(transport, adapter, { type: 'artifact.open', id: 'photo' }, { result: 'artifact_opened', artifact_id: 'photo', revision: 1, body });
        assert.match(adapter.getSnapshot().artifacts[1].description, words);
        assert.equal(adapter.getSnapshot().artifacts[1].asset, null);
    }
});
test('downloads: the fetched content, fetched first when need be, and only what can be saved', async () => {
    const { transport, adapter } = connected();
    const notes = await answered(transport, adapter, { type: 'artifact.download', id: 'notes' }, openedText('notes', 2, '# Notes'));
    assert.deepEqual(notes.sent.action, 'open_artifact');
    assert.deepEqual(notes.outcome, { ok: true, message: 'Downloaded.', download: { filename: 'Sample markdown file', mime: 'text/markdown', text: '# Notes', asset: null } });
    const before = transport.actions().length;
    assert.equal((await adapter.execute({ type: 'artifact.download', id: 'notes' })).ok, true);
    assert.equal(transport.actions().length, before, 'already fetched: nothing sent');
    const photo = await answered(transport, adapter, { type: 'artifact.download', id: 'photo' }, { result: 'artifact_opened', artifact_id: 'photo', revision: 1, body: { type: 'image', ...SAMPLE_PNG } });
    assert.deepEqual(photo.outcome.download, { filename: 'Sample image file', mime: 'image/png', text: null, asset: `data:image/png;base64,${SAMPLE_PNG.base64}` });
    for (const body of [{ type: 'image', media_type: 'png', base64: '!!' }, { type: 'unpreviewable', bytes: 1 }]) {
        const other = connected();
        assert.match((await answered(other.transport, other.adapter, { type: 'artifact.download', id: 'photo' }, { result: 'artifact_opened', artifact_id: 'photo', revision: 1, body })).outcome.message, /cannot be saved/);
    }
    const plain = connected([event({ kind: 'artifact_listed', ...sampleArtifact('t', 'text') })]);
    const text = await answered(plain.transport, plain.adapter, { type: 'artifact.download', id: 't' }, openedText('t', 1, 'x'));
    assert.equal(text.outcome.download.mime, 'text/plain');
    assert.match((await plain.adapter.execute({ type: 'artifact.download', id: 'gone' })).message, /no longer available/);
    const titled = connected([event({ kind: 'artifact_listed', ...sampleArtifact('t', 'text'), title: '../???' })]);
    assert.equal((await answered(titled.transport, titled.adapter, { type: 'artifact.download', id: 't' }, openedText('t', 1, 'x'))).outcome.download.filename, '..');
    const nameless = connected([event({ kind: 'artifact_listed', ...sampleArtifact('t', 'text'), title: '???' })]);
    assert.equal((await answered(nameless.transport, nameless.adapter, { type: 'artifact.download', id: 't' }, openedText('t', 1, 'x'))).outcome.download.filename, 'download');
});
test('files: added files are a typed save, limits in plain words, artifacts attached to chats', async () => {
    const { transport, adapter } = connected();
    const saved = await answered(transport, adapter, { type: 'artifact.import', title: 'notes.md', format: 'markdown', content: '# Hi', dataUrl: '' }, { result: 'artifact_saved', artifact_id: 'new-1', revision: 1 });
    assert.deepEqual(saved.sent, { action: 'save_file', chat_id: null, name: 'notes.md', format: 'markdown', content: { type: 'text', text: '# Hi' } });
    assert.deepEqual(saved.outcome, { ok: true, message: 'Saved.', id: 'new-1' });
    const image = await answered(transport, adapter, { type: 'artifact.import', title: 'p.png', format: 'image', content: '', dataUrl: `data:image/png;base64,${SAMPLE_PNG.base64}` }, { result: 'artifact_saved', artifact_id: 'new-2', revision: 1 });
    assert.deepEqual(image.sent.content, { type: 'image', ...SAMPLE_PNG });
    const before = transport.actions().length;
    assert.match((await adapter.execute({ type: 'artifact.import', title: 'p.svg', format: 'image', content: '', dataUrl: 'data:image/svg+xml;base64,AAAA' })).message, /PNG, JPEG, WebP or GIF/);
    assert.match((await adapter.execute({ type: 'artifact.import', title: 'v', format: 'video', content: '', dataUrl: '' })).message, /cannot be added from the app yet/);
    assert.equal(transport.actions().length, before);
    for (const [reason, words] of [[{ refusal: 'too_large', limit: 10485760 }, /larger than Hatchling accepts\. The limit is 10 MB/], [{ refusal: 'unsupported_type' }, /does not accept this kind/], [{ refusal: 'full' }, /file store is full/]])
        assert.match((await answered(transport, adapter, { type: 'artifact.import', title: 'a.txt', format: 'text', content: 'x', dataUrl: '' }, { result: 'file_refused', reason })).outcome.message, words);
    assert.match((await answered(transport, adapter, { type: 'artifact.import', title: 'a.txt', format: 'text', content: 'x', dataUrl: '' }, { result: 'artifact_opened', artifact_id: 'a', revision: 1, body: { type: 'text', text: '' } })).outcome.message, /something else/);
    const attached = await answered(transport, adapter, { type: 'chat.attach', chatId: 'main', artifactId: 'notes' }, { result: 'message_accepted', message_id: 'file-2' });
    assert.deepEqual([attached.sent, attached.outcome], [{ action: 'attach_artifact', chat_id: 'local', artifact_id: 'notes', revision: 2 }, { ok: true, message: 'Sent.' }]);
    assert.match((await adapter.execute({ type: 'chat.attach', chatId: 'main', artifactId: 'gone' })).message, /no longer available to send/);
});
test('files in chat arrive as attachments that name the artifact; their words are plain text', () => {
    const { transport, adapter } = connected();
    transport.deliver(event({ kind: 'attachment', chat_id: 'main', message_id: 'file-3', author: 'user', artifact_id: 'photo', revision: 1, text: '[x](https://example.invalid)' }));
    transport.deliver(event({ kind: 'attachment', chat_id: 'side', message_id: 'file-4', author: 'system', artifact_id: 'photo', revision: 1, text: 'Saved.' }));
    const files = adapter.getSnapshot().messages.filter(message => message.kind === 'attachment');
    assert.deepEqual(files.map(message => [message.id, message.role, message.artifactId, message.text]), [['file-1', 'agent', 'notes', 'Here are the sample notes.'], ['file-3', 'user', 'photo', '[x](https://example.invalid)'], ['file-4', 'agent', 'photo', 'Saved.']]);
    assert.ok(adapter.getSnapshot().chats.some(chat => chat.id === 'side'));
});
test('the file formats the wire carries from the device, and only those', () => {
    const carried = ['markdown', 'text', 'csv', 'code', 'html', 'image'];
    for (const format of carried)
        assert.equal(fileFormat(format), format);
    for (const format of ['pdf', 'word', 'spreadsheet', 'archive', 'slides', 'app', 'video', 'audio'])
        assert.equal(fileFormat(format), undefined, format);
    assert.deepEqual(imageFromUrl('data:image/webp;base64,AAAA'), { media_type: 'webp', base64: 'AAAA' });
    assert.equal(imageFromUrl('https://example.invalid/a.png'), undefined);
});
test('live browser: the session, its newest page snapshot and its action log render from events', () => {
    const { transport, adapter } = connected();
    let current = task(adapter);
    assert.deepEqual([current.title, current.site, current.step, current.statusTitle, current.state, current.controller, current.lastConfirmedState, current.chatId], ['Find a sample tote bag', 'shop.example.invalid', 'Searching the sample shop', 'Working', 'running', 'agent', 'The sample shop home page', 'main']);
    assert.deepEqual(current.frame, { sequence: 1, page: 'home', title: 'Sample shop', query: '', capturedAt: sampleTime(6), image: `data:image/png;base64,${SAMPLE_PNG.base64}` });
    // A newer snapshot replaces; an older one never does.
    transport.deliver(event({ kind: 'browser_frame', session_id: 'web-1', sequence: 3, captured_at: sampleTime(9), title: 'Results', image: null }));
    transport.deliver(event({ kind: 'browser_frame', session_id: 'web-1', sequence: 2, captured_at: sampleTime(8), title: 'Old', image: SAMPLE_PNG }));
    transport.deliver(event({ kind: 'browser_frame', session_id: 'nobody', sequence: 9, captured_at: sampleTime(9), title: 'x' }));
    current = task(adapter);
    assert.deepEqual([current.frame.sequence, current.frame.title, current.frame.image], [3, 'Results', null]);
    // The browser card sits in the chat that asked for it.
    assert.deepEqual(adapter.getSnapshot().messages.filter(message => message.kind === 'browser').map(message => [message.id, message.chatId, message.taskId]), [['browser:web-1', 'main', 'web-1']]);
    const log = adapter.getSnapshot().threads.find(thread => thread.id === 'browser:web-1');
    assert.deepEqual([log.name, log.subtitle, log.statusTitle, log.state, log.summary], ['Browser session', '1 action logged · hatchling is driving', 'Hatchling is driving', 'running', '1 action on the page.']);
    assert.deepEqual(log.lanes[0].rows.map(row => [row.title, row.subtitle, row.report]), [['Searched for a tote bag', 'Done by hatchling', '> Searched for a tote bag']]);
    // A session with no chat has no card; ended sessions read as ended.
    transport.deliver(event({ kind: 'browser_session', ...sampleSession('web-2', 'failed', null) }));
    transport.deliver(event({ kind: 'browser_session', ...sampleSession('web-1', 'completed') }));
    const threads = adapter.getSnapshot().threads;
    assert.deepEqual(['web-1', 'web-2'].map(id => [threads.find(thread => thread.id === `browser:${id}`).state, threads.find(thread => thread.id === `browser:${id}`).statusTitle]), [['completed', 'Session ended'], ['failed', 'Session ended']]);
    assert.equal(adapter.getSnapshot().messages.filter(message => message.kind === 'browser').length, 1);
    assert.equal('chatId' in task(adapter, 'web-2'), false);
    assert.equal('frame' in task(adapter, 'web-2'), false);
});
test('takeover: agent → user → agent, moved only by the handoffs the broker reports', async () => {
    const { transport, adapter } = connected();
    // Asking moves nothing: control moves when the broker reports the handoff.
    const asked = await answered(transport, adapter, { type: 'browser.takeover', id: 'web-1' }, { result: 'control_accepted', session_id: 'web-1', handoff: 'takeover' });
    assert.deepEqual([asked.sent, asked.outcome.ok, task(adapter).controller], [{ action: 'take_control', session_id: 'web-1' }, true, 'agent']);
    transport.deliver(control('web-1', 'takeover'));
    let current = task(adapter);
    assert.deepEqual([current.controller, current.statusTitle, current.step], ['switching', 'Switching control', 'Handing control to you. Nothing runs until it settles.']);
    // Nothing runs while control changes hands, except Stop.
    const before = transport.actions().length;
    for (const type of ['browser.takeover', 'browser.resume'])
        assert.match((await adapter.execute({ type, id: 'web-1' })).message, /changing hands/);
    assert.equal(transport.actions().length, before);
    transport.deliver(control('web-1', 'granted'));
    current = task(adapter);
    assert.deepEqual([current.controller, current.statusTitle], ['user', 'You have control']);
    assert.match((await adapter.execute({ type: 'browser.takeover', id: 'web-1' })).message, /already have control/);
    // Hand back from the newest frame seen.
    transport.deliver(event({ kind: 'browser_frame', session_id: 'web-1', sequence: 4, captured_at: sampleTime(9), title: 'Seen by you' }));
    const back = await answered(transport, adapter, { type: 'browser.resume', id: 'web-1' }, { result: 'control_accepted', session_id: 'web-1', handoff: 'hand_back' });
    assert.deepEqual(back.sent, { action: 'return_control', session_id: 'web-1', frame_sequence: 4 });
    transport.deliver(control('web-1', 'hand_back', 4));
    assert.match(task(adapter).step, /looks at the page again before it acts/);
    // A resume on the page the user left is held: the agent must look again first.
    transport.deliver(control('web-1', 'resumed', 4));
    assert.equal(task(adapter).controller, 'switching');
    transport.deliver(control('web-1', 'resumed', 5));
    assert.equal(task(adapter).controller, 'agent');
    assert.match((await adapter.execute({ type: 'browser.resume', id: 'web-1' })).message, /Hatchling already has control/);
});
test('no event is processed as agent-driven while the user drives, and nothing as anyone\'s while switching', () => {
    const { transport, adapter } = connected();
    const act = by => transport.deliver(event({ kind: 'browser_action', session_id: 'web-1', by, narration: `${by} acted` }));
    act('user');
    transport.deliver(control('web-1', 'takeover'));
    act('agent');
    act('user');
    transport.deliver(control('web-1', 'granted'));
    act('agent');
    act('user');
    transport.deliver(event({ kind: 'browser_action', session_id: 'nobody', by: 'user', narration: 'x' }));
    const log = adapter.getSnapshot().threads.find(thread => thread.id === 'browser:web-1');
    assert.deepEqual(log.lanes[0].rows.map(row => [row.title, row.subtitle]), [['Searched for a tote bag', 'Done by hatchling'], ['user acted', 'Done by you']]);
    assert.equal(log.summary, '2 actions on the page. 4 actions arrived from whoever was not driving and were not shown.');
    assert.equal(log.subtitle, '2 actions logged · you are driving');
    const one = connected();
    one.transport.deliver(event({ kind: 'browser_action', session_id: 'web-1', by: 'user', narration: 'x' }));
    assert.match(one.adapter.getSnapshot().threads.find(thread => thread.id === 'browser:web-1').summary, /1 action arrived from whoever was not driving and was not shown\./);
});
test('a code request on the stream is taken in stride: the browser and the connection carry on', () => {
    const { transport, adapter } = connected();
    const before = adapter.getSnapshot().browserTasks.length;
    transport.deliver(event({ kind: 'browser_code', session_id: 'web-1', request_id: 'code-1', origin: 'https://a.example', state: 'asked' }));
    assert.equal(adapter.getSnapshot().browserTasks.length, before);
    assert.equal(transport.closes, 0, 'not a broken protocol');
});
test('the user keeps the sign-in of a browser they drive, and the signed-in list is asked for again', async () => {
    const { transport, adapter } = connected();
    const keep = { type: 'browser.keep-signed-in', id: 'web-1' };
    const before = transport.actions().length;
    assert.match((await adapter.execute(keep)).message, /Hatchling already has control/);
    assert.equal(transport.actions().length, before, 'nothing sent while the agent drives');
    transport.deliver(control('web-1', 'takeover'));
    transport.deliver(control('web-1', 'granted'));
    const profile = { profile_id: 'profile-1', sites: ['a.example:443', 'idp.example:443'], signed_in: true, created_at: '2026-10-22T08:00:00Z', last_used: '2026-10-22T09:00:00Z' };
    const pending = adapter.execute(keep);
    await flush();
    const sent = transport.last();
    assert.deepEqual(sent.action, { action: 'keep_signed_in', session_id: 'web-1' });
    transport.deliver({ type: 'action_result', request_id: sent.request_id, result: { result: 'signed_in_kept', session_id: 'web-1', profile } });
    await flush();
    const listing = transport.last();
    assert.deepEqual(listing.action, { action: 'manage_profiles', op: { op: 'list' } });
    transport.deliver({ type: 'action_result', request_id: listing.request_id, result: { result: 'profiles', outcome: { result: 'listed', profiles: [profile] } } });
    const kept = await pending;
    assert.equal(kept.ok, true);
    assert.match(kept.message, /opens a\.example:443, idp\.example:443 signed in from now on/);
    assert.deepEqual(adapter.getSnapshot().signedInSites.map(item => item.id), ['profile-1']);
    for (const [result, words] of [
        [{ result: 'profiles', outcome: { result: 'off' } }, /keeps no signed-in sites/],
        [{ result: 'profiles', outcome: { result: 'failed' } }, /couldn't change its signed-in sites/],
        [{ result: 'profiles', outcome: { result: 'not_found' } }, /something else/],
        [{ result: 'control_refused', session_id: 'web-1', reason: 'not_allowed' }, /cannot move that way/],
        [{ result: 'signed_in_kept', session_id: 'other', profile }, /something else/],
    ]) {
        const { outcome } = await answered(transport, adapter, keep, result);
        assert.equal(outcome.ok, false);
        assert.match(outcome.message, words);
    }
});
test('a one-time code is asked on its browser, given straight to the broker or declined, and never kept', async () => {
    const { transport, adapter } = connected();
    const before = transport.actions().length;
    assert.match((await adapter.codes.give('web-1', '481516')).message, /No code is asked for/);
    assert.match((await adapter.execute({ type: 'browser.decline-code', id: 'web-1' })).message, /No code is asked for/);
    assert.equal(transport.actions().length, before, 'nothing sent while no code is asked for');
    const asked = (request_id, state = 'asked', session_id = 'web-1') => event({ kind: 'browser_code', session_id, request_id, origin: 'https://a.example', state });
    transport.deliver(asked('code-1'));
    assert.deepEqual(task(adapter).codeRequest, { id: 'code-1', origin: 'https://a.example' });
    // Given: sent once, as its own request, and gone from the browser.
    const given = await answered(transport, adapter, null, { result: 'code_taken', session_id: 'web-1' }, () => adapter.codes.give('web-1', '481516'));
    assert.deepEqual(given.sent, { action: 'give_code', session_id: 'web-1', request_id: 'code-1', code: '481516' });
    assert.deepEqual(given.outcome, { ok: true, message: 'Given. Hatchling types it into the page once, and forgets it.' });
    assert.equal(task(adapter).codeRequest, undefined);
    assert.ok(!JSON.stringify(adapter.getSnapshot()).includes('481516'), 'the code is in no state');
    // Declined, by the user's command.
    transport.deliver(asked('code-2'));
    const declined = await answered(transport, adapter, { type: 'browser.decline-code', id: 'web-1' }, { result: 'code_declined', session_id: 'web-1' });
    assert.deepEqual(declined.sent, { action: 'decline_code', session_id: 'web-1', request_id: 'code-2' });
    assert.equal(declined.outcome.ok, true);
    assert.equal(task(adapter).codeRequest, undefined);
    // Every other answer is told as what it is, and the request still waits.
    transport.deliver(asked('code-3'));
    const refusals = { no_request: /no longer asked for/, empty: /Type the code first/, too_long: /longer than any code/ };
    for (const [reason, words] of Object.entries(refusals))
        assert.match((await answered(transport, adapter, null, { result: 'code_refused', session_id: 'web-1', reason }, () => adapter.codes.give('web-1', 'x'))).outcome.message, words);
    for (const result of [{ result: 'code_taken', session_id: 'other' }, { result: 'code_refused', session_id: 'other', reason: 'empty' }, { result: 'browser_stopped', session_id: 'web-1' }]) {
        assert.match((await answered(transport, adapter, null, result, () => adapter.codes.give('web-1', 'x'))).outcome.message, /something else/);
        assert.match((await answered(transport, adapter, { type: 'browser.decline-code', id: 'web-1' }, result)).outcome.message, /something else/);
    }
    assert.match((await answered(transport, adapter, null, { result: 'refused', reason: 'not_granted' }, () => adapter.codes.give('web-1', 'x'))).outcome.message, /./);
    assert.deepEqual(task(adapter).codeRequest, { id: 'code-3', origin: 'https://a.example' });
    // The broker moves it on: another request's end, or another browser's, leaves it; its own ends it.
    transport.deliver(asked('code-9', 'expired'));
    transport.deliver(asked('code-3', 'asked', 'nobody'));
    assert.equal(task(adapter).codeRequest?.id, 'code-3');
    transport.deliver(asked('code-3', 'filled'));
    assert.equal(task(adapter).codeRequest, undefined);
    // An ended browser shows no request.
    transport.deliver(asked('code-4'));
    transport.deliver(event({ kind: 'browser_session', ...sampleSession('web-1', 'cancelled') }));
    assert.equal(task(adapter).codeRequest, undefined);
});
test("the user's own input goes only to a browser they drive, and the broker answers it", async () => {
    const { transport, adapter } = connected();
    const click = { type: 'browser.click', id: 'web-1', x: 10, y: 20 };
    const before = transport.actions().length;
    assert.match((await adapter.execute(click)).message, /Hatchling already has control/);
    assert.match((await adapter.execute({ ...click, id: 'nobody' })).message, /no longer available/);
    transport.deliver(control('web-1', 'takeover'));
    assert.match((await adapter.execute({ type: 'browser.key', id: 'web-1', key: 'tab' })).message, /changing hands/);
    assert.equal(transport.actions().length, before, "nothing sent before control is the user's");
    transport.deliver(control('web-1', 'granted'));
    const inputs = [
        [click, { input: 'click', x: 10, y: 20 }],
        [{ type: 'browser.input', id: 'web-1', text: 'hi' }, { input: 'type', text: 'hi' }],
        [{ type: 'browser.key', id: 'web-1', key: 'enter' }, { input: 'key', key: 'enter' }],
        [{ type: 'browser.scroll', id: 'web-1', down: true }, { input: 'scroll', down: true }],
    ];
    for (const [command, input] of inputs) {
        const done = await answered(transport, adapter, command, { result: 'input_done', session_id: 'web-1' });
        assert.deepEqual([done.sent, done.outcome], [{ action: 'browser_input', session_id: 'web-1', input }, { ok: true, message: 'Done. The page after it follows.' }]);
    }
    const failures = { no_page: /No page is open/, unknown_element: /part of the page is gone/, not_possible: /could not take that/, timeout: /did not finish in time/, load_failed: /could not be loaded/, browser_gone: /stopped working/, bad_command: /did not understand/ };
    for (const [failure, words] of Object.entries(failures))
        assert.match((await answered(transport, adapter, click, { result: 'input_failed', session_id: 'web-1', failure })).outcome.message, words);
    assert.match((await answered(transport, adapter, click, { result: 'control_refused', session_id: 'web-1', reason: 'not_allowed' })).outcome.message, /cannot move that way/);
    for (const result of [{ result: 'input_done', session_id: 'other' }, { result: 'input_failed', session_id: 'other', failure: 'timeout' }, { result: 'browser_stopped', session_id: 'web-1' }])
        assert.match((await answered(transport, adapter, click, result)).outcome.message, /something else/);
    // Only a whole, non-negative pixel is a point of the page.
    const sent = transport.actions().length;
    for (const [x, y] of [[-1, 0], [0, -1], [1.5, 0], [0, 2.5]])
        assert.match((await adapter.execute({ ...click, x, y })).message, /could not take that/);
    assert.equal(transport.actions().length, sent);
    transport.deliver(event({ kind: 'browser_session', ...sampleSession('web-1', 'cancelled') }));
    assert.match((await adapter.execute({ type: 'browser.scroll', id: 'web-1', down: false })).message, /has ended/);
});
test('a click on the picture is the point of the page under it, letterboxed or not', () => {
    const fitted = { left: 10, top: 20, width: 640, height: 400 };
    assert.deepEqual(pagePoint(fitted, 10, 20), { x: 0, y: 0 });
    assert.deepEqual(pagePoint(fitted, 649.9, 419.9), { x: 1279, y: 799 });
    assert.deepEqual(pagePoint(fitted, 330, 220), { x: 640, y: 400 });
    for (const [x, y] of [[9, 20], [10, 19], [650, 20], [10, 420]])
        assert.equal(pagePoint(fitted, x, y), undefined, `${x}, ${y}`);
    // Wider than the page: bars left and right.
    const wide = { left: 0, top: 0, width: 800, height: 400 };
    assert.deepEqual(pagePoint(wide, 80, 0), { x: 0, y: 0 });
    assert.equal(pagePoint(wide, 79, 0), undefined);
    // Taller: bars above and below.
    const tall = { left: 0, top: 0, width: 640, height: 600 };
    assert.deepEqual(pagePoint(tall, 0, 100), { x: 0, y: 0 });
    assert.equal(pagePoint(tall, 0, 99), undefined);
    assert.equal(pagePoint({ left: 0, top: 0, width: 0, height: 0 }, 0, 0), undefined, 'nothing shown');
});
test('a handoff the table does not allow ends the connection, and is not counted as applied', () => {
    const { transport, adapter } = connected();
    transport.deliver(control('web-1', 'granted'));
    const connection = adapter.chatSession.getSnapshot().connection;
    assert.equal(connection.kind, 'unavailable');
    assert.match(connection.detail, /handoff \(granted\) that is not allowed while hatchling is driving/);
    assert.equal(task(adapter).controller, 'agent', 'nothing changed');
    adapter.chatSession.reconnect();
    transport.open();
    transport.deliver(SAMPLE_WELCOME);
    assert.deepEqual(transport.last(), { type: 'subscribe', after_seq: SURFACE_EVENTS.length }, 'the refused event will be read again, not skipped');
    // A handoff for a session the app has not seen changes nothing.
    const other = connected();
    other.transport.deliver(control('nobody', 'granted'));
    assert.equal(other.adapter.chatSession.getSnapshot().connection.kind, 'ready');
});
test('browser commands: refusals in plain words, ended and unknown sessions, Stop whoever drives', async () => {
    const { transport, adapter } = connected();
    const refusals = { unknown_session: /no longer available/, finished: /has ended/, switching: /changing hands/, not_allowed: /cannot move that way/, stale_frame: /page changed since you last saw it/ };
    for (const [reason, words] of Object.entries(refusals))
        assert.match((await answered(transport, adapter, { type: 'browser.takeover', id: 'web-1' }, { result: 'control_refused', session_id: 'web-1', reason })).outcome.message, words);
    for (const result of [{ result: 'control_refused', session_id: 'other', reason: 'finished' }, { result: 'browser_stopped', session_id: 'web-1' }])
        assert.match((await answered(transport, adapter, { type: 'browser.takeover', id: 'web-1' }, result)).outcome.message, /something else/);
    transport.deliver(control('web-1', 'takeover'));
    const stopped = await answered(transport, adapter, { type: 'browser.cancel', id: 'web-1' }, { result: 'browser_stopped', session_id: 'web-1' });
    assert.deepEqual([stopped.sent, stopped.outcome], [{ action: 'stop_browser', session_id: 'web-1' }, { ok: true, message: 'Stopped. No more actions will run.' }]);
    assert.match((await answered(transport, adapter, { type: 'browser.cancel', id: 'web-1' }, { result: 'control_refused', session_id: 'web-1', reason: 'finished' })).outcome.message, /has ended/);
    assert.match((await answered(transport, adapter, { type: 'browser.cancel', id: 'web-1' }, { result: 'control_accepted', session_id: 'web-1', handoff: 'takeover' })).outcome.message, /something else/);
    transport.deliver(event({ kind: 'browser_session', ...sampleSession('web-1', 'cancelled') }));
    const before = transport.actions().length;
    for (const type of ['browser.takeover', 'browser.resume', 'browser.cancel'])
        assert.match((await adapter.execute({ type, id: 'web-1' })).message, /has ended/);
    for (const type of ['browser.takeover', 'browser.cancel'])
        assert.match((await adapter.execute({ type, id: 'nobody' })).message, /no longer available/);
    assert.equal(transport.actions().length, before);
    for (const type of ['browser.grant', 'browser.solve', 'browser.start'])
        assert.match((await adapter.execute({ type, id: 'web-1' })).message, /cannot do this from the app yet/);
});
test('the takeover table is hatchling-core\'s, applied exactly', () => {
    assert.deepEqual(CONTROL_TRANSITIONS, [
        { from: 'agent', handoff: 'takeover', to: 'to_user' }, { from: 'user', handoff: 'hand_back', to: 'to_agent' },
        { from: 'to_user', handoff: 'granted', to: 'user' }, { from: 'to_user', handoff: 'resumed', to: 'agent' },
        { from: 'to_agent', handoff: 'granted', to: 'user' }, { from: 'to_agent', handoff: 'resumed', to: 'agent' },
    ]);
    const states = ['agent', 'user', 'to_user', 'to_agent'];
    let allowed = 0;
    for (const from of states)
        for (const handoff of ['takeover', 'hand_back', 'granted', 'resumed']) {
            const to = controlAfter(from, handoff);
            const row = CONTROL_TRANSITIONS.find(item => item.from === from && item.handoff === handoff);
            assert.equal(to, row?.to, `${from} + ${handoff}`);
            allowed += to === undefined ? 0 : 1;
        }
    assert.equal(allowed, 6);
    assert.deepEqual(states.map(state => [drives(state, 'agent'), drives(state, 'user'), switching(state)]), [[true, false, false], [false, true, false], [false, false, true], [false, false, true]]);
});
test('the browser views name the driver, including while control changes hands', () => {
    assert.deepEqual(['agent', 'user', 'switching'].map(controlPill), [{ label: 'Watching hatchling', tone: '' }, { label: 'You have control', tone: 'success' }, { label: 'Switching control', tone: 'attention' }]);
    const base = { state: 'running', controller: 'switching' };
    assert.equal(browserLabel(base), 'Switching control');
    assert.equal(browserLabel({ ...base, controller: 'user' }), 'You have control');
    assert.equal(browserLabel({ ...base, controller: 'agent' }), 'Working');
    const button = switchingControl();
    assert.equal(button.tagName, 'button');
    assert.ok(button.attributes.has('disabled'), 'disabled: nobody drives');
    assert.match(walk(button).text, /Switching control…/);
});
test('edges: nothing listed, nothing fetched, no frame yet, no connection', async () => {
    // A listed artifact with no revisions has nothing to fetch or save.
    const empty = connected([event({ kind: 'artifact_listed', ...sampleArtifact('empty', 'text', 0) })]);
    assert.match((await empty.adapter.execute({ type: 'artifact.download', id: 'empty' })).message, /no longer available/);
    const shelf = new ArtifactShelf();
    assert.deepEqual([shelf.download('x'), shelf.body('x'), shelf.hasRevision('x', 1)], [undefined, undefined, false]);
    shelf.keep('x', 1, { type: 'text', text: 'ignored' });
    assert.equal(shelf.body('x'), undefined, 'content for an unlisted file is not kept');
    // Sharing an artifact the broker never listed sends nothing.
    assert.match((await empty.adapter.execute({ type: 'artifact.publish', id: 'gone', approvedVersion: 1 })).message, /no longer listed/);
    // Handing back before any page snapshot arrived names frame 0.
    const blank = connected([event({ kind: 'browser_session', ...sampleSession('web-9') })]);
    blank.transport.deliver(control('web-9', 'takeover'));
    blank.transport.deliver(control('web-9', 'granted'));
    const back = await answered(blank.transport, blank.adapter, { type: 'browser.resume', id: 'web-9' }, { result: 'control_accepted', session_id: 'web-9', handoff: 'hand_back' });
    assert.equal(back.sent.frame_sequence, 0);
    blank.transport.deliver(control('web-9', 'hand_back', 0));
    blank.transport.deliver(control('web-9', 'resumed', 1));
    assert.equal(task(blank.adapter, 'web-9').controller, 'agent');
    // Without a connection, nothing is sent and the command says so.
    blank.transport.handlers.at(-1).closed('Gone.');
    assert.match((await blank.adapter.execute({ type: 'artifact.open', id: 'notes' })).message, /no longer available/);
    assert.match((await empty.adapter.execute({ type: 'artifact.open', id: 'empty' })).message, /no longer available/);
    const offline = connected();
    offline.transport.handlers.at(-1).closed('Gone.');
    assert.match((await offline.adapter.execute({ type: 'artifact.open', id: 'notes' })).message, /not connected to Hatchling, so nothing was sent/);
    // The runtime check: anyOf needs one match.
    assert.equal(conforms(3, { anyOf: [{ type: 'string' }, { type: 'null' }] }, {}), false);
    assert.equal(conforms(null, { anyOf: [{ type: 'string' }, { type: 'null' }] }, {}), true);
});
