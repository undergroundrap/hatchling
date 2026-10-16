// Starting live from `hatchling-host`: the token taken from the address's
// fragment and cleared, the WebSocket transport to the host's channel, and the
// broker's local channel shown as the app's main chat.
import test from 'node:test';
import assert from 'node:assert/strict';
import { LiveAdapter } from '../../dist/app/adapters/live/adapter.js';
import { BROKER_MAIN_CHAT, MAIN_CHAT, appChat, wireChat } from '../../dist/app/adapters/live/chat.js';
import { TOKEN_FRAGMENT, channelUrl, liveFromPlace, tokenIn } from '../../dist/app/adapters/live/launch.js';
import { WebSocketTransport, browserSocket } from '../../dist/app/adapters/live/websocket.js';
const TOKEN = '0123456789abcdef0123456789abcdef';
/** A socket the transport drives, recording what it was told. */
class FakeSocket {
    onopen = null;
    onmessage = null;
    onclose = null;
    sent = [];
    closes = 0;
    failSend = false;
    constructor(url) { this.url = url; }
    send(text) {
        if (this.failSend)
            throw new Error('not open');
        this.sent.push(text);
    }
    close() { this.closes++; }
}
/** Handlers that record what the transport reported. */
function recorder() {
    const seen = [];
    return { seen, handlers: { opened: () => seen.push('opened'), received: text => seen.push(`received ${text}`), closed: detail => seen.push(`closed ${detail}`) } };
}
function transport() {
    const sockets = [];
    const made = new WebSocketTransport('ws://127.0.0.1:7340/ui', url => { const socket = new FakeSocket(url); sockets.push(socket); return socket; });
    return { made, sockets };
}
test('the token is only a fragment that says so, in hexadecimal', () => {
    assert.equal(TOKEN_FRAGMENT, '#hatchling-ui=');
    assert.equal(tokenIn(`#hatchling-ui=${TOKEN}`), TOKEN);
    for (const hash of ['', '#', `#${TOKEN}`, '#chat/main', '#hatchling-ui=', '#hatchling-ui=0123456789abcde', `#hatchling-ui=${TOKEN.toUpperCase()}`, `#hatchling-ui=${TOKEN}&x`, `#hatchling-ui=${'a'.repeat(129)}`])
        assert.equal(tokenIn(hash), undefined, hash);
    assert.equal(tokenIn(`#hatchling-ui=${'a'.repeat(128)}`), 'a'.repeat(128));
});
test('the channel is the page\'s own origin, secure when the page is', () => {
    assert.equal(channelUrl({ protocol: 'http:', host: '127.0.0.1:7340' }), 'ws://127.0.0.1:7340/ui');
    assert.equal(channelUrl({ protocol: 'https:', host: 'ui.example' }), 'wss://ui.example/ui');
});
test('a page without a token starts as before, its address untouched', () => {
    const replaced = [];
    const place = { protocol: 'http:', host: '127.0.0.1:7340', pathname: '/', search: '', hash: '#chat/main' };
    assert.equal(liveFromPlace(place, address => replaced.push(address)), undefined);
    assert.deepEqual(replaced, []);
});
test('a page with a token starts live and clears the fragment at once', () => {
    const replaced = [];
    const sockets = [];
    const original = globalThis.WebSocket;
    globalThis.WebSocket = class {
        constructor(url) { this.url = url; sockets.push(this); }
        addEventListener() { }
        send() { }
        close() { }
    };
    try {
        const place = { protocol: 'http:', host: '127.0.0.1:7340', pathname: '/', search: '?x=1', hash: `#hatchling-ui=${TOKEN}` };
        const adapter = liveFromPlace(place, address => replaced.push(address));
        assert.ok(adapter instanceof LiveAdapter);
        assert.equal(adapter.mode, 'live');
        assert.deepEqual(replaced, ['/?x=1'], 'the token leaves the address');
        assert.deepEqual(sockets.map(socket => socket.url), ['ws://127.0.0.1:7340/ui']);
    }
    finally {
        globalThis.WebSocket = original;
    }
});
test('the transport reports the socket\'s life and carries text both ways', () => {
    const { made, sockets } = transport();
    const { seen, handlers } = recorder();
    made.send('dropped: not connected yet');
    made.connect(handlers);
    const [socket] = sockets;
    assert.equal(socket.url, 'ws://127.0.0.1:7340/ui');
    socket.onopen();
    made.send('{"type":"hello"}');
    assert.deepEqual(socket.sent, ['{"type":"hello"}']);
    socket.onmessage('{"type":"welcome"}');
    socket.onclose();
    socket.onclose();
    assert.deepEqual(seen, ['opened', 'received {"type":"welcome"}', 'closed The connection to Hatchling ended.'], 'one close, once');
    made.send('after the end');
    assert.deepEqual(socket.sent, ['{"type":"hello"}']);
});
test('anything but text, or a send that fails, ends the connection', () => {
    const { made, sockets } = transport();
    const first = recorder();
    made.connect(first.handlers);
    sockets[0].onmessage(new Uint8Array([1, 2]));
    assert.deepEqual(first.seen, ['closed Hatchling sent something other than text, so the app disconnected.']);
    assert.equal(sockets[0].closes, 1);
    const second = recorder();
    made.connect(second.handlers);
    sockets[1].failSend = true;
    made.send('x');
    assert.deepEqual(second.seen, ['closed The app could not send to Hatchling, so it disconnected.']);
    const third = recorder();
    made.connect(third.handlers);
    made.close();
    made.close();
    assert.deepEqual(third.seen, ['closed The connection to Hatchling ended.']);
    assert.equal(sockets[2].closes, 1);
    // A socket a reconnect replaced can still end, without touching the new one.
    const fourth = recorder();
    made.connect(fourth.handlers);
    sockets[2].onclose();
    made.send('to the newest');
    assert.deepEqual(sockets[3].sent, ['to the newest']);
});
test('the browser\'s socket is adapted to the transport\'s shape', () => {
    const original = globalThis.WebSocket;
    const made = [];
    globalThis.WebSocket = class {
        listeners = new Map();
        sent = [];
        closes = 0;
        constructor(url) { this.url = url; made.push(this); }
        addEventListener(kind, listener) { this.listeners.set(kind, listener); }
        send(text) { this.sent.push(text); }
        close() { this.closes++; }
        fire(kind, event = {}) { this.listeners.get(kind)(event); }
    };
    try {
        const like = browserSocket('ws://h/ui');
        const [socket] = made;
        assert.equal(socket.url, 'ws://h/ui');
        // Events before anyone listens are harmless.
        for (const kind of ['open', 'message', 'close', 'error'])
            socket.fire(kind, { data: 'x' });
        const seen = [];
        like.onopen = () => seen.push('open');
        like.onmessage = data => seen.push(`message ${data}`);
        like.onclose = () => seen.push('close');
        socket.fire('open');
        socket.fire('message', { data: 'hi' });
        socket.fire('error');
        socket.fire('close');
        assert.deepEqual(seen, ['open', 'message hi', 'close', 'close']);
        like.send('out');
        like.close();
        assert.deepEqual([socket.sent, socket.closes], [['out'], 1]);
    }
    finally {
        globalThis.WebSocket = original;
    }
});
test('the broker\'s local channel is the app\'s main chat, both ways', () => {
    assert.equal([MAIN_CHAT, BROKER_MAIN_CHAT].join(' '), 'main local');
    assert.equal(appChat('local'), 'main');
    assert.equal(appChat('side-1'), 'side-1');
    assert.equal(wireChat('main'), 'local');
    assert.equal(wireChat('side-1'), 'side-1');
});
