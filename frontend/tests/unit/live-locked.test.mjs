import assert from 'node:assert/strict';
import test from 'node:test';
import { askTheHost, hostState, LiveClient } from '../../dist/app/adapters/live/client.js';
class Transport {
    handlers;
    connect(handlers) { this.handlers = handlers; }
    send() { }
    close() { }
}
const settle = () => new Promise(resolve => setTimeout(resolve, 0));
test('the host says, and nothing else counts as the host', async () => {
    const said = (value) => () => Promise.resolve({ json: () => Promise.resolve(value) });
    assert.equal(await hostState(said({ locked: true })), 'locked');
    assert.equal(await hostState(said({ locked: false })), 'unlocked');
    for (const value of [{ locked: 'true' }, {}, null, 'locked']) {
        assert.equal(await hostState(said(value)), 'no-host');
    }
    assert.equal(await hostState(() => Promise.reject(new Error('no host'))), 'no-host');
    assert.equal(await hostState(() => Promise.resolve({ json: () => Promise.reject(new Error('not json')) })), 'no-host');
});
test('a connection lost to a locked vault says how to unlock it', async () => {
    const transport = new Transport();
    const client = new LiveClient(transport, 'app', () => undefined, undefined, () => Promise.resolve(true));
    client.connect();
    transport.handlers.closed('closed');
    assert.equal(client.connection().kind, 'offline');
    await settle();
    assert.equal(client.connection().kind, 'locked');
    assert.match(client.connection().detail, /sudo hatchling-broker unlock/u);
});
test('a host that says it isn\'t locked leaves the connection as it was', async () => {
    const transport = new Transport();
    const client = new LiveClient(transport, 'app', () => undefined, undefined, () => Promise.resolve(false));
    client.connect();
    transport.handlers.closed('closed');
    await settle();
    assert.equal(client.connection().kind, 'offline');
});
test('an answer that comes after a reconnect began changes nothing', async () => {
    const transport = new Transport();
    let answer;
    const client = new LiveClient(transport, 'app', () => undefined, undefined, () => new Promise(resolve => { answer = resolve; }));
    client.connect();
    transport.handlers.closed('closed');
    client.connect();
    answer(true);
    await settle();
    assert.equal(client.connection().kind, 'reconnecting');
});
test('a client told nothing about locks says nothing of them', async () => {
    const transport = new Transport();
    const client = new LiveClient(transport, 'app', () => undefined);
    client.connect();
    transport.handlers.closed('closed');
    await settle();
    assert.equal(client.connection().kind, 'offline');
});
test('with nothing to ask, the host is taken not to be there', async () => {
    const real = globalThis.fetch;
    globalThis.fetch = () => Promise.reject(new Error('no host'));
    assert.equal(await askTheHost(), 'no-host');
    globalThis.fetch = real;
});
