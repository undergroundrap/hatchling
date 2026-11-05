// The model Hatchling uses, over a fake client: read when the connection opens
// and on refresh, chosen by the owner, and every broker answer as a state or
// message the view can say.
import test from 'node:test';
import assert from 'node:assert/strict';
import { LiveModel, modelOf } from '../../dist/app/adapters/live/model.js';
import { fitLine, inUse, sourceLine, switchAsk, whereLine } from '../../dist/app/features/settings/model.js';
import { hostOf, onThisMachine, whereShort } from '../../dist/app/session/model.js';
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
const WIRE = { endpoint: 'http://localhost:11434/v1', configured: 'qwen3.5:9b', available: ['gemma4:12b', 'qwen3.5:9b'], window: 32768, size: 6600000000, on_cpu: 0 };
const model = wire => ({ kind: 'answered', result: { result: 'model', model: wire } });
const refused = reason => ({ kind: 'answered', result: { result: 'refused', reason } });
const flush = () => new Promise(resolve => setImmediate(resolve));
const surface = answers => {
    const client = new FakeClient(answers);
    let changes = 0;
    let serial = 0;
    const live = new LiveModel(client, () => { changes++; }, () => `r${++serial}`);
    return { client, live, changes: () => changes };
};
test('the broker\'s word reads in the app\'s, each part absent as none', () => {
    assert.deepEqual(modelOf({ ...WIRE, chosen: 'gemma4:12b' }), { endpoint: 'http://localhost:11434/v1', configured: 'qwen3.5:9b', chosen: 'gemma4:12b', available: ['gemma4:12b', 'qwen3.5:9b'], window: 32768, size: 6600000000, onCpu: 0, other: null, switched: false });
    assert.deepEqual(modelOf({}), { endpoint: null, configured: null, chosen: null, available: null, window: null, size: null, onCpu: null, other: null, switched: false });
    assert.deepEqual(modelOf({ other: { endpoint: 'https://openrouter.ai/api/v1', name: 'n' }, switched: true }).other, { endpoint: 'https://openrouter.ai/api/v1', name: 'n' });
    assert.equal(modelOf({ switched: true }).switched, true);
    assert.deepEqual(modelOf({ other: {} }).other, { endpoint: null, name: null });
});
test('the model is read once each time the connection opens, and on refresh', async () => {
    const { client, live, changes } = surface([model(WIRE), model({ ...WIRE, window: 65536 }), { kind: 'failed', detail: 'Not connected.' }, refused('not_granted'), { kind: 'answered', result: { result: 'usage', usage: {} } }]);
    assert.deepEqual(live.current(), { kind: 'unread' });
    client.set('ready');
    client.set('ready');
    await flush();
    assert.deepEqual(client.sent, [{ action: 'read_model' }], 'once while it stays open');
    assert.equal(live.current().kind, 'read');
    assert.equal(live.current().model.window, 32768);
    await live.refresh();
    assert.equal(live.current().model.window, 65536);
    await live.refresh();
    assert.deepEqual(live.current(), { kind: 'failed', detail: 'Not connected.' });
    await live.refresh();
    assert.deepEqual(live.current(), { kind: 'failed', detail: 'This connection may not read the model.' });
    await live.refresh();
    assert.deepEqual(live.current(), { kind: 'failed', detail: 'Hatchling answered with something else.' });
    assert.equal(changes(), 5);
});
test('a choice is answered with the model as it stands, and none goes back to the installed one', async () => {
    const { client, live, changes } = surface([model({ ...WIRE, chosen: 'gemma4:12b' }), model(WIRE), model({ available: [] })]);
    assert.deepEqual(await live.choose('gemma4:12b'), { ok: true, message: 'Hatchling uses gemma4:12b from your next message.' });
    assert.equal(live.current().model.chosen, 'gemma4:12b');
    assert.deepEqual(await live.choose(null), { ok: true, message: 'Back to qwen3.5:9b, from your next message.' });
    assert.deepEqual(await live.choose(null), { ok: true, message: 'Back to the installed model, from your next message.' });
    assert.deepEqual(client.sent, [{ action: 'set_model', name: 'gemma4:12b' }, { action: 'set_model' }, { action: 'set_model' }]);
    assert.equal(changes(), 3);
});
test('a choice not kept says why, and changes nothing', async () => {
    const { live, changes } = surface([refused('bad_request'), refused('unavailable'), refused('not_granted'), refused('not_connected'), { kind: 'failed', detail: 'Not connected.' }, { kind: 'answered', result: { result: 'usage', usage: {} } }]);
    assert.deepEqual(await live.choose('mistral'), { ok: false, message: 'Hatchling didn’t take that model: its server has none by that name.' });
    assert.match((await live.choose('m')).message, /only when scheduling is on/);
    assert.equal((await live.choose('m')).message, 'This connection may not choose the model.');
    assert.equal((await live.choose('m')).message, 'Not connected.');
    assert.deepEqual(await live.choose('m'), { ok: false, message: 'Not connected.' });
    assert.deepEqual(await live.choose('m'), { ok: false, message: 'Hatchling answered with something else.' });
    assert.deepEqual(live.current(), { kind: 'unread' });
    assert.equal(changes(), 0);
});
test('the section says the model in use, where it came from, and how it fits', () => {
    const view = { endpoint: 'http://localhost:11434/v1', configured: 'qwen3.5:9b', chosen: null, available: ['qwen3.5:9b'], window: 32768, size: 6600000000, onCpu: 0 };
    assert.equal(inUse(view), 'qwen3.5:9b');
    assert.equal(inUse({ ...view, chosen: 'gemma4:12b' }), 'gemma4:12b');
    assert.equal(sourceLine(view), 'The model Hatchling was installed with.');
    assert.equal(sourceLine({ ...view, chosen: 'gemma4:12b' }), 'You chose it; Hatchling was installed with qwen3.5:9b.');
    assert.equal(sourceLine({ ...view, chosen: 'gemma4:12b', configured: null }), 'You chose it.');
    assert.equal(fitLine(view), 'A 32,768-token window. All on the GPU (6.6 GB).');
    assert.equal(fitLine({ ...view, size: 10700000000, onCpu: 900000000, window: 131072 }), 'A 131,072-token window. 0.9 GB of its 10.7 GB runs on the CPU, more slowly: free the GPU’s memory, or give it a smaller window.');
    assert.equal(fitLine({ ...view, window: null, size: null, onCpu: null }), 'Its window is known once the model has loaded.');
    assert.equal(fitLine({ ...view, onCpu: null }), 'A 32,768-token window.');
});
test('a switch to the other server is answered with the model as it stands, and says where it answers', async () => {
    const hosted = { endpoint: 'https://openrouter.ai/api/v1', configured: 'nemotron', other: { endpoint: 'http://localhost:11434/v1', name: 'qwen3.5:9b' }, switched: true };
    const { client, live, changes } = surface([model(hosted), model(WIRE), model({})]);
    assert.deepEqual(await live.switchServer(true), { ok: true, message: 'Hatchling uses nemotron, hosted at openrouter.ai, from your next message.' });
    assert.equal(live.current().model.switched, true);
    assert.deepEqual(await live.switchServer(false), { ok: true, message: 'Hatchling uses qwen3.5:9b, on this machine, from your next message.' });
    assert.deepEqual(await live.switchServer(false), { ok: true, message: 'Hatchling uses the other model, on its server, from your next message.' });
    assert.deepEqual(client.sent, [{ action: 'switch_model_server', other: true }, { action: 'switch_model_server', other: false }, { action: 'switch_model_server', other: false }]);
    assert.equal(changes(), 3);
});
test('a switch refused says why, and changes nothing', async () => {
    const { live, changes } = surface([refused('bad_request'), refused('unavailable'), refused('not_granted'), refused('not_connected'), { kind: 'failed', detail: 'Not connected.' }, { kind: 'answered', result: { result: 'usage', usage: {} } }]);
    assert.match((await live.switchServer(true)).message, /no other model set up/);
    assert.match((await live.switchServer(true)).message, /doctor says which/);
    assert.equal((await live.switchServer(true)).message, 'This connection may not switch the model.');
    assert.equal((await live.switchServer(true)).message, 'Not connected.');
    assert.deepEqual(await live.switchServer(true), { ok: false, message: 'Not connected.' });
    assert.deepEqual(await live.switchServer(true), { ok: false, message: 'Hatchling answered with something else.' });
    assert.deepEqual(live.current(), { kind: 'unread' });
    assert.equal(changes(), 0);
});
test('the section says where the owner's words go, and asks before a switch sends them elsewhere', () => {
    const view = { endpoint: 'http://localhost:11434/v1', configured: 'qwen3.5:9b', chosen: null, available: null, window: null, size: null, onCpu: null, other: null, switched: false };
    assert.equal(whereLine(view), 'It answers on this machine: your words stay here.');
    assert.equal(whereLine({ ...view, endpoint: 'https://openrouter.ai/api/v1' }), 'It answers at openrouter.ai: your words go to its provider.');
    assert.equal(whereLine({ ...view, endpoint: null }), 'No server is set for it.');
    assert.equal(sourceLine({ ...view, switched: true }), 'The model set up on this server.');
    assert.equal(sourceLine({ ...view, switched: true, chosen: 'm' }), 'You chose it; this server was set up with qwen3.5:9b.');
    const away = switchAsk({ endpoint: 'https://openrouter.ai/api/v1', name: 'nemotron' });
    assert.equal(away.title, 'Send your words to openrouter.ai?');
    assert.match(away.description, /goes to openrouter.ai for nemotron to answer/);
    assert.match(away.description, /switch back here any time/);
    const home = switchAsk({ endpoint: 'http://127.0.0.1:11434/v1', name: null });
    assert.equal(home.title, 'Switch to the other model, on this machine?');
    assert.match(home.description, /your words stay here/);
    assert.equal(switchAsk({ endpoint: null, name: 'm' }).title, 'Send your words to its server?');
});
test('a server on this machine is localhost's, in plain HTTP', () => {
    for (const endpoint of ['http://localhost:11434/v1', 'http://127.0.0.1:8080', 'http://localhost/'])
        assert.ok(onThisMachine(endpoint), endpoint);
    for (const endpoint of [null, 'https://localhost:11434/v1', 'http://localhost.evil.example/v1', 'http://127.0.0.1.nip.io/v1', 'http://models.lan:11434/v1'])
        assert.ok(!onThisMachine(endpoint), String(endpoint));
    assert.equal(hostOf('https://openrouter.ai/api/v1'), 'openrouter.ai');
    assert.equal(hostOf('not a url'), 'not a url');
    assert.equal(whereShort(null), 'on its server');
});
