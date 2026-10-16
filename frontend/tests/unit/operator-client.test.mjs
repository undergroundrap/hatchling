// The Machine view's client: hello with the operator token, every query on each
// tick, answers checked before they are shown, and any frame it can't read ending
// the connection loudly. Starting it from the host's address takes the token
// from the fragment and clears it.
import test from 'node:test';
import assert from 'node:assert/strict';
import { OperatorClient, QUERIES, REFRESH_MS, apply, browserEvery } from '../../dist/app/adapters/operator/client.js';
import { OPERATOR_FRAGMENT, operatorChannelUrl, operatorFromPlace, operatorTokenIn } from '../../dist/app/adapters/operator/launch.js';
import { UI_SCHEMA_VERSION } from '../../dist/app/adapters/live/protocol/generated.js';
const TOKEN = '0123456789abcdef0123456789abcdef';
const LIMITS = { processes: 4096, address_space: null, file_size: null, open_files: null, memory: 1073741824, cpu_millis: 500 };
const CELL = { cell_id: 'run-1f2e', kind: 'run', started_at: '2026-10-16T09:00:00Z', limits: LIMITS };
const SESSION = { connection: 'api-0f3c', socket: 'ui', opened_at: '2026-10-16T09:00:01Z' };
const DECISION = { at: '2026-10-16T09:00:02Z', cell_id: 'run-1f2e', destination: 'localhost:18089', verdict: 'allowed' };
/** A transport the client drives, recording what it sent. */
class FakeTransport {
    handlers = null;
    sent = [];
    closes = 0;
    connect(handlers) { this.handlers = handlers; }
    send(text) { this.sent.push(JSON.parse(text)); }
    close() {
        this.closes++;
        this.handlers?.closed('closed by the app');
    }
    receive(frame) { this.handlers.received(typeof frame === 'string' ? frame : JSON.stringify(frame)); }
}
/** A timer the test ticks by hand. */
function clock() {
    const timer = { tick: null, ms: 0, stopped: 0 };
    const every = (tick, ms) => { timer.tick = tick; timer.ms = ms; return () => { timer.stopped++; }; };
    return { timer, every };
}
function started() {
    const transport = new FakeTransport();
    const seen = [];
    const { timer, every } = clock();
    const client = new OperatorClient(transport, TOKEN, machine => seen.push(machine), every);
    client.start();
    return { transport, seen, timer, client };
}
const answer = (answer) => ({ type: 'answer', request_id: 'q', answer });
test('it says hello with the token once open, and asks every query once welcomed and on each tick', () => {
    const { transport, seen, timer, client } = started();
    assert.equal(client.machine.connection, 'connecting');
    assert.deepEqual(transport.sent, []);
    transport.handlers.opened();
    assert.deepEqual(transport.sent, [{ type: 'hello', schema_version: UI_SCHEMA_VERSION, client: 'hatchling-machine', token: TOKEN }]);
    transport.receive({ type: 'welcome', schema_version: UI_SCHEMA_VERSION });
    assert.equal(seen.at(-1).connection, 'live');
    const asked = () => transport.sent.slice(1).map(frame => frame.query.query);
    assert.deepEqual(asked(), [...QUERIES]);
    assert.equal(timer.ms, REFRESH_MS);
    timer.tick();
    assert.deepEqual(asked(), [...QUERIES, ...QUERIES]);
    // Each request is named apart.
    const ids = transport.sent.slice(1).map(frame => frame.request_id);
    assert.equal(new Set(ids).size, ids.length);
    assert.deepEqual(transport.sent[1], { type: 'query', request_id: 'broker-1', query: { query: 'broker' } });
});
test('answers fill the view, each its own part', () => {
    const { transport, seen } = started();
    transport.handlers.opened();
    transport.receive({ type: 'welcome' });
    transport.receive(answer({ answer: 'broker', version: '0.4.0', started_at: '2026-10-16T09:00:00Z', uptime_secs: 61 }));
    transport.receive(answer({ answer: 'cells', cells: [CELL] }));
    transport.receive(answer({ answer: 'sessions', sessions: [SESSION] }));
    transport.receive(answer({ answer: 'egress', decisions: [DECISION] }));
    const machine = seen.at(-1);
    assert.deepEqual(machine.broker, { version: '0.4.0', startedAt: '2026-10-16T09:00:00Z', uptimeSecs: 61 });
    assert.deepEqual(machine.cells, [CELL]);
    assert.deepEqual(machine.sessions, [SESSION]);
    assert.deepEqual(machine.egress, [DECISION]);
    assert.equal(machine.connection, 'live');
    assert.equal(machine.detail, '');
});
test('an answer the view cannot read ends the connection, and the ticks with it', () => {
    const unreadable = [
        'not json',
        'null',
        '[]',
        { type: 'event' },
        answer(null),
        answer({ answer: 'other' }),
        answer({ answer: 'broker', version: 4, started_at: 'x', uptime_secs: 1 }),
        answer({ answer: 'broker', version: 'v', started_at: 'x', uptime_secs: -1 }),
        answer({ answer: 'broker', version: 'v', started_at: 'x', uptime_secs: 1.5 }),
        answer({ answer: 'cells', cells: {} }),
        answer({ answer: 'cells', cells: [{ ...CELL, kind: 'robot' }] }),
        answer({ answer: 'cells', cells: [{ ...CELL, limits: { ...LIMITS, memory: 'lots' } }] }),
        answer({ answer: 'cells', cells: [{ ...CELL, limits: [] }] }),
        answer({ answer: 'sessions', sessions: [{ ...SESSION, socket: 'operator' }] }),
        answer({ answer: 'egress', decisions: [{ ...DECISION, verdict: 'maybe' }] }),
        answer({ answer: 'egress', decisions: [{ ...DECISION, destination: 7 }] }),
    ];
    for (const frame of unreadable) {
        const { transport, seen, timer } = started();
        transport.handlers.opened();
        transport.receive({ type: 'welcome' });
        transport.receive(frame);
        const machine = seen.at(-1);
        assert.equal(machine.connection, 'ended', JSON.stringify(frame));
        assert.match(machine.detail, /cannot read/);
        assert.equal(transport.closes, 1);
        assert.equal(timer.stopped, 1);
    }
});
test('limits a broker leaves out are read as unset', () => {
    const sparse = { ...CELL, limits: { processes: 1 } };
    assert.deepEqual(apply({ connection: 'live', detail: '', broker: null, cells: [], sessions: [], egress: [] }, { answer: 'cells', cells: [sparse] })?.cells, [sparse]);
});
test('a refused or mismatched hello says why, and asks nothing', () => {
    // Closed before any welcome: the address is old, or there is no operator socket.
    {
        const { transport, seen } = started();
        transport.handlers.opened();
        transport.handlers.closed('The connection to Hatchling ended.');
        assert.equal(seen.at(-1).connection, 'ended');
        assert.match(seen.at(-1).detail, /address may be old/);
        assert.equal(transport.sent.length, 1);
    }
    // Another version: the broker's own sentence.
    {
        const { transport, seen } = started();
        transport.handlers.opened();
        transport.receive({ type: 'incompatible', frontend: 5, backend: 4, message: 'reload the frontend to update' });
        assert.equal(seen.at(-1).detail, 'reload the frontend to update');
        assert.equal(transport.closes, 1);
    }
    // An incompatible frame without its sentence, or anything else first.
    for (const first of [{ type: 'incompatible' }, { type: 'answer' }]) {
        const { transport, seen } = started();
        transport.handlers.opened();
        transport.receive(first);
        assert.match(seen.at(-1).detail, /cannot read/);
        assert.equal(transport.sent.length, 1, 'nothing asked');
    }
});
test('a connection that ends after its welcome keeps its last answers and says why', () => {
    const { transport, seen, timer, client } = started();
    transport.handlers.opened();
    transport.receive({ type: 'welcome' });
    transport.receive(answer({ answer: 'cells', cells: [CELL] }));
    transport.handlers.closed('The connection to Hatchling ended.');
    assert.equal(seen.at(-1).connection, 'ended');
    assert.equal(seen.at(-1).detail, 'The connection to Hatchling ended.');
    assert.deepEqual(seen.at(-1).cells, [CELL]);
    assert.equal(timer.stopped, 1);
    // Ending again changes nothing more.
    const count = seen.length;
    client.stop();
    assert.equal(seen.length, count);
});
test('the browser timer starts and stops', () => {
    let ticks = 0;
    const stop = browserEvery(() => { ticks++; }, 1);
    stop();
    assert.equal(ticks, 0);
});
test('the operator token is only a fragment that says so, in hexadecimal', () => {
    assert.equal(OPERATOR_FRAGMENT, '#hatchling-operator=');
    assert.equal(operatorTokenIn(`#hatchling-operator=${TOKEN}`), TOKEN);
    for (const hash of ['', `#hatchling-ui=${TOKEN}`, '#hatchling-operator=', `#hatchling-operator=${TOKEN.toUpperCase()}`, `#hatchling-operator=${TOKEN}&x`])
        assert.equal(operatorTokenIn(hash), undefined, hash);
});
test('the operator channel is the page\'s own origin, secure when the page is', () => {
    assert.equal(operatorChannelUrl({ protocol: 'http:', host: '127.0.0.1:7340' }), 'ws://127.0.0.1:7340/operator');
    assert.equal(operatorChannelUrl({ protocol: 'https:', host: 'ops.example' }), 'wss://ops.example/operator');
});
test('started from the host\'s address, the token is taken, the fragment cleared, and the channel opened', () => {
    const place = { protocol: 'http:', host: '127.0.0.1:7340', pathname: '/machine.html', search: '', hash: `#hatchling-operator=${TOKEN}` };
    const replaced = [];
    const sockets = [];
    const open = url => { const socket = { url, onopen: null, onmessage: null, onclose: null, sent: [], send(text) { this.sent.push(text); }, close() { } }; sockets.push(socket); return socket; };
    const { every } = clock();
    const client = operatorFromPlace(place, address => replaced.push(address), () => { }, open, every);
    assert.ok(client);
    assert.deepEqual(replaced, ['/machine.html']);
    assert.equal(sockets[0].url, 'ws://127.0.0.1:7340/operator');
    sockets[0].onopen();
    assert.equal(JSON.parse(sockets[0].sent[0]).token, TOKEN);
    // Without a token: nothing starts, and the address is left alone.
    const none = operatorFromPlace({ ...place, hash: '' }, address => replaced.push(address), () => { }, open, every);
    assert.equal(none, undefined);
    assert.deepEqual(replaced, ['/machine.html']);
    assert.equal(sockets.length, 1);
});
