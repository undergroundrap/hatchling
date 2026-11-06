import assert from 'node:assert/strict';
import test from 'node:test';
import { LiveClient, lockedNow } from '../../dist/app/adapters/live/client.js';
class Transport {
    handlers;
    connect(handlers) { this.handlers = handlers; }
    send() { }
    close() { }
}
const settle = () => new Promise(resolve => setTimeout(resolve, 0));
test('the host says locked, and nothing else counts as locked', async () => {
    const said = (value) => () => Promise.resolve({ json: () => Promise.resolve(value) });
    assert.equal(await lockedNow(said({ locked: true })), true);
    for (const value of [{ locked: false }, { locked: 'true' }, {}, null, 'locked']) {
        assert.equal(await lockedNow(said(value)), false);
    }
    assert.equal(await lockedNow(() => Promise.reject(new Error('no host'))), false);
    assert.equal(await lockedNow(() => Promise.resolve({ json: () => Promise.reject(new Error('not json')) })), false);
});
test('a connection lost to a locked vault says how to unlock it', async () => {
    const transport = new Transport();
    const client = new LiveClient(transport, 'app', () => undefined, undefined, () => Promise.resolve(true));
    client.connect();
    transport.handlers.closed('closed');
    assert.equal(client.connection().kind, 'offline');
    await settle();
    assert.equal(client.connection().kind, 'sign-in-required');
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
