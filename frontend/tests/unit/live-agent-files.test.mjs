// Files the agent gives the owner (WO-041), end to end over a fake transport:
// the Library's list and what it says; a download read in order, held to every
// bound the broker said, checked against the hash, and saved as bytes only;
// each way a download ends without saving; removal; and the stream's word of
// a file given, and of the Library changed.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { LiveAdapter } from '../../dist/app/adapters/live/adapter.js';
import { LiveClient } from '../../dist/app/adapters/live/client.js';
import { FILE_OUT_BYTES, LiveAgentFiles, PART_BYTES, browserDigest, saveBytes } from '../../dist/app/adapters/live/agent-files.js';
import { SAMPLE_WELCOME, sampleEvent } from '../../dist/app/adapters/live/fixtures.js';
import { caution, folderPathsRefused, isGivenName, leftOutWords, libraryGroups, runs } from '../../dist/app/session/agent-files.js';
import { holdsGit } from '../../dist/app/adapters/live/agent-files.js';
class FakeTransport {
    sent = [];
    handlers = [];
    respond = () => undefined;
    connect(handlers) { this.handlers.push(handlers); }
    send(text) {
        const frame = JSON.parse(text);
        this.sent.push(frame);
        if (frame.type !== 'action')
            return;
        const result = this.respond(frame.action, frame.request_id);
        if (result !== undefined)
            queueMicrotask(() => this.deliver({ type: 'action_result', request_id: frame.request_id, result }));
    }
    close() { }
    deliver(frame) { this.handlers.at(-1).received(JSON.stringify(frame)); }
    open() { this.handlers.at(-1).opened(); }
    actions(name) { return this.sent.filter(frame => frame.type === 'action' && frame.action.action === name).map(frame => frame.action); }
}
const flush = () => new Promise(resolve => setImmediate(resolve));
const settle = async () => {
    for (let i = 0; i < 60; i++)
        await flush();
};
const sha = (bytes) => createHash('sha256').update(bytes).digest('hex');
const bytesOf = (length) => Uint8Array.from({ length }, (_, at) => at % 251);
const STANDING = { files: 1, file_limit: 64, bytes: 3, byte_limit: 268435456, given: 1, given_limit: 16, off: false, files_in_all: 1, files_in_all_limit: 4096, read: 3, read_limit: 134217728, conversation: 1 };
/** A file as the wire says it. */
const wire = (bytes, fields = {}) => ({ id: 'f1', name: 'report.pdf', bytes: bytes.length, sha256: sha(bytes), kept_at: '2026-11-17T00:00:00Z', conversation: 1, ...fields });
/** A broker keeping `files` (each `{ file, bytes }`), answering as the Library does. */
function keeping(files, fields = {}) {
    let held = [...files];
    let begun = held[0];
    return (action) => {
        switch (action.action) {
            case 'list_library': return { result: 'library', folders: [], removed: [], files: held.map(entry => entry.file), standing: STANDING };
            case 'begin_file_out': {
                const found = held.find(entry => entry.file.id === action.id);
                if (found === undefined)
                    return { result: 'file_out_refused', reason: { refusal: 'gone' } };
                begun = found;
                return { result: 'file_out_begun', download_id: 'out-1', file: found.file, parts: Math.ceil(found.file.bytes / PART_BYTES), ...fields.begun };
            }
            case 'file_out_part': {
                const part = begun.bytes.subarray(action.seq * PART_BYTES, (action.seq + 1) * PART_BYTES);
                return { result: 'file_out_part_read', download_id: action.download_id, seq: action.seq, data: Buffer.from(part).toString('base64'), ...fields.part?.(action) };
            }
            case 'remove_library_file': {
                const before = held.length;
                held = held.filter(entry => entry.file.id !== action.id);
                return held.length < before ? { result: 'library_file_removed', id: action.id } : { result: 'file_out_refused', reason: { refusal: 'gone' } };
            }
            default: return undefined;
        }
    };
}
/** A Library over a connected client, saving into `saved`. */
function library(respond, options = {}) {
    const transport = new FakeTransport();
    transport.respond = respond;
    const client = new LiveClient(transport, 'test-ui', () => undefined);
    const saved = [];
    let drawn = 0;
    let serial = 0;
    const files = new LiveAgentFiles(client, () => drawn++, () => `request-${++serial}`, { save: (parts, name) => saved.push([Buffer.concat(parts.map(part => Buffer.from(part))), name]), digest: async (bytes) => sha(bytes), wait: 1000, ...options });
    client.connect();
    transport.open();
    transport.deliver(SAMPLE_WELCOME);
    return { transport, client, files, saved, drawn: () => drawn };
}
test('a file is read out in order, checked against its hash, and saved as bytes by the broker\'s name', async () => {
    const bytes = bytesOf(PART_BYTES * 2 + 5);
    const { transport, files, saved } = library(keeping([{ file: wire(bytes), bytes }]));
    await settle();
    const going = files.download('f1');
    assert.equal(files.downloading('f1'), true);
    assert.deepEqual(await files.download('f1'), { ok: false, message: 'It is already downloading.' });
    const result = await going;
    assert.deepEqual(result, { ok: true, message: 'Gave your browser report.pdf, 64 KB, to save. The agent made it: open it only if you trust it.' });
    assert.equal(files.downloading('f1'), false);
    assert.deepEqual(transport.actions('file_out_part').map(part => part.seq), [0, 1, 2]);
    assert.equal(saved.length, 1);
    assert.deepEqual([Buffer.from(saved[0][0]).equals(Buffer.from(bytes)), saved[0][1]], [true, 'report.pdf']);
    // A file of no bytes is no parts, and saved all the same.
    const empty = library(keeping([{ file: wire(new Uint8Array(0), { id: 'f2', name: 'empty.txt' }), bytes: new Uint8Array(0) }]));
    assert.equal((await empty.files.download('f2')).ok, true);
    assert.equal(empty.saved[0][0].length, 0);
});
test('a page that can\'t check a hash downloads nothing', async () => {
    const bytes = bytesOf(3);
    const { transport, files, saved } = library(keeping([{ file: wire(bytes), bytes }]), { digest: null });
    assert.deepEqual(await files.download('f1'), { ok: false, message: 'This page can’t check a file’s hash, so it can’t download one. Nothing was saved. Open Hatchling at its own address.' });
    assert.deepEqual([transport.actions('begin_file_out'), saved], [[], []]);
});
test('a download the broker refuses, or doesn\'t answer, is said and saves nothing', async () => {
    const bytes = bytesOf(3);
    const said = async (begun) => {
        const { files, saved } = library((action) => action.action === 'begin_file_out' ? begun : undefined, { wait: 20 });
        const result = await files.download('f1');
        assert.equal(saved.length, 0);
        return result.message;
    };
    assert.equal(await said({ result: 'file_out_refused', reason: { refusal: 'gone' } }), 'It isn’t in Hatchling’s Library any more.');
    assert.equal(await said({ result: 'file_out_refused', reason: { refusal: 'damaged' } }), 'Hatchling’s copy of it is damaged. Nothing was saved. Remove it, and ask the agent for it again.');
    assert.equal(await said({ result: 'file_out_refused', reason: { refusal: 'busy' } }), 'Other downloads are under way. Nothing was saved. Try again in a minute.');
    assert.equal(await said({ result: 'file_out_refused', reason: { refusal: 'unknown_download' } }), 'The file didn’t arrive whole. Nothing was saved. Try again.');
    assert.equal(await said({ result: 'refused', reason: 'not_granted' }), 'This connection may not download from Hatchling’s Library.');
    assert.equal(await said({ result: 'inbox', files: [] }), 'Hatchling answered with something else. Nothing was saved.');
    assert.equal(await said(undefined), 'Hatchling didn’t answer in time. Nothing was saved.');
    // Not connected: said so.
    const transport = new FakeTransport();
    const alone = new LiveAgentFiles(new LiveClient(transport, 'test-ui', () => undefined), () => undefined, () => 'request-1', { save: () => assert.fail('saved'), digest: async () => '' });
    assert.match((await alone.download('f1')).message, /Nothing was saved\.$/u);
    // A file gone reads the list again.
    const { transport: seen, files } = library((action) => action.action === 'begin_file_out' ? { result: 'file_out_refused', reason: { refusal: 'gone' } } : keeping([{ file: wire(bytes), bytes }])(action));
    await settle();
    const reads = seen.actions('list_library').length;
    await files.download('f1');
    await settle();
    assert.equal(seen.actions('list_library').length, reads + 1);
});
test('a broker that says more than its bounds, or another file, is never downloaded from', async () => {
    const bytes = bytesOf(3);
    const begun = async (fields, known = true) => {
        const { files, saved } = library((action) => {
            if (action.action === 'list_library')
                return known ? { result: 'library', folders: [], removed: [], files: [wire(bytes)], standing: STANDING } : { result: 'library', folders: [], removed: [], files: [], standing: STANDING };
            if (action.action === 'begin_file_out')
                return { result: 'file_out_begun', download_id: 'out-1', file: wire(bytes), parts: 1, ...fields };
            return keeping([{ file: wire(bytes), bytes }])(action);
        });
        await settle();
        const result = await files.download('f1');
        return [result.ok, saved.length, result.message];
    };
    const refused = [false, 0, 'Hatchling answered with something else. Nothing was saved.'];
    assert.deepEqual(await begun({ file: wire(bytes, { id: 'f9' }) }), refused);
    assert.deepEqual(await begun({ file: wire(bytes, { name: '../evil.exe' }) }), refused);
    assert.deepEqual(await begun({ file: wire(bytes, { bytes: FILE_OUT_BYTES + 1 }), parts: 257 }), refused);
    assert.deepEqual(await begun({ file: wire(bytes, { bytes: FILE_OUT_BYTES }), parts: 256 }, false), [false, 0, 'The file didn’t arrive whole. Nothing was saved. Try again.']);
    assert.deepEqual(await begun({ parts: 2 }), refused);
    // Another hash than the list's.
    assert.deepEqual(await begun({ file: wire(bytes, { sha256: sha(bytesOf(4)) }) }), refused);
    // With no list to hold it to, the hash checked at the end decides.
    assert.deepEqual((await begun({}, false)).slice(0, 2), [true, 1]);
});
test('a part out of step, too large, past the file or not base64 saves nothing', async () => {
    const bytes = bytesOf(PART_BYTES + 3);
    const ended = async (part, file = wire(bytes)) => {
        const { files, saved } = library((action) => action.action === 'list_library' ? { result: 'library', folders: [], removed: [], files: [], standing: STANDING } : keeping([{ file, bytes }], { part })(action), { wait: 20 });
        const result = await files.download('f1');
        return [result.ok, saved.length, result.message];
    };
    const notWhole = [false, 0, 'The file didn’t arrive whole. Nothing was saved. Try again.'];
    assert.deepEqual(await ended(() => ({ download_id: 'out-9' })), notWhole);
    assert.deepEqual(await ended((action) => ({ seq: action.seq + 1 })), notWhole);
    assert.deepEqual(await ended(() => ({ data: '%%%' })), notWhole);
    assert.deepEqual(await ended(() => ({ data: Buffer.alloc(PART_BYTES + 1).toString('base64') })), notWhole);
    // The last part longer than what is left.
    assert.deepEqual(await ended((action) => action.seq === 1 ? { data: Buffer.alloc(4).toString('base64') } : {}), notWhole);
    // Short of the file's length.
    assert.deepEqual(await ended((action) => action.seq === 1 ? { data: Buffer.alloc(2).toString('base64') } : {}), notWhole);
    // Whole, but another file's bytes.
    assert.deepEqual(await ended((action) => action.seq === 1 ? { data: Buffer.from([9, 9, 9]).toString('base64') } : {}), notWhole);
    assert.deepEqual(await ended(() => ({ result: 'file_out_refused', reason: { refusal: 'out_of_step' } })), notWhole);
    assert.deepEqual(await ended(() => ({ result: 'refused', reason: 'unavailable' })), [false, 0, 'Hatchling couldn’t read its Library. Nothing was saved.']);
    assert.deepEqual(await ended(() => ({ result: 'library_file_removed', id: 'f1' })), [false, 0, 'Hatchling answered with something else. Nothing was saved.']);
    // A part not answered.
    const { files, saved } = library((action) => action.action === 'file_out_part' ? undefined : keeping([{ file: wire(bytes), bytes }])(action), { wait: 20 });
    assert.deepEqual([(await files.download('f1')).message, saved.length], ['Hatchling didn’t answer in time. Nothing was saved.', 0]);
});
test('the Library is read when the connection opens, and said as the broker answers', async () => {
    const listed = async (answer) => {
        const { files } = library((action) => action.action === 'list_library' ? answer : undefined);
        await settle();
        return files.current();
    };
    const bytes = bytesOf(3);
    assert.deepEqual(await listed({ result: 'library', folders: [], removed: [], files: [wire(bytes), wire(bytes, { id: 'f2', name: '.hidden' })], standing: { ...STANDING, off: true } }), {
        kind: 'read',
        files: [{ id: 'f1', name: 'report.pdf', bytes: 3, sha256: sha(bytes), keptAt: '2026-11-17T00:00:00Z', reply: null, conversation: 1 }],
        folders: [],
        unnamed: [{ id: 'f2', bytes: 3, removable: false }],
        removed: [],
        standing: { files: 1, fileLimit: 64, bytes: 3, byteLimit: 268435456, given: 1, givenLimit: 16, off: true, filesInAll: 1, filesInAllLimit: 4096, read: 3, readLimit: 134217728, conversation: 1 },
    });
    assert.deepEqual(await listed({ result: 'refused', reason: 'not_granted' }), { kind: 'failed', detail: 'This connection may not read Hatchling’s Library.' });
    assert.deepEqual(await listed({ result: 'inbox', files: [] }), { kind: 'failed', detail: 'Hatchling answered with something else.' });
    const transport = new FakeTransport();
    const alone = new LiveAgentFiles(new LiveClient(transport, 'test-ui', () => undefined), () => undefined, () => 'request-1');
    assert.deepEqual(alone.current(), { kind: 'unread' });
    await alone.refresh();
    assert.equal(alone.current().kind, 'failed');
    // Read again each time the connection opens, once.
    const { transport: again } = library(() => undefined);
    const reads = () => again.actions('list_library').length;
    const before = reads();
    again.deliver({ type: 'event', seq: 1, ts: '2026-11-16T00:00:00Z', body: { kind: 'agent_status', state: 'idle', detail: '' } });
    assert.equal(reads(), before, 'read again without the connection opening again');
});
test('a file is removed by its id, and a message that showed it says so', async () => {
    const bytes = bytesOf(3);
    const { files, transport } = library(keeping([{ file: wire(bytes), bytes }]));
    await settle();
    files.observe({ kind: 'file_offered', chat_id: 'local', message_id: 'file-a', file: wire(bytes) });
    await settle();
    assert.equal(files.offered('file-a').removed, false);
    assert.deepEqual(await files.remove('f1'), { ok: true, message: 'Removed from Hatchling’s Library.' });
    assert.equal(files.offered('file-a').removed, true);
    assert.deepEqual(transport.actions('remove_library_file'), [{ action: 'remove_library_file', id: 'f1' }]);
    // Removed already, elsewhere: said so, and the message too.
    files.observe({ kind: 'file_offered', chat_id: 'local', message_id: 'file-b', file: wire(bytes, { id: 'f3' }) });
    assert.deepEqual(await files.remove('f3'), { ok: true, message: 'It was already removed.' });
    assert.equal(files.offered('file-b').removed, true);
    const refused = async (answer) => (await library(() => answer).files.remove('f1')).message;
    assert.equal(await refused({ result: 'refused', reason: 'not_granted' }), 'This connection may not remove files from Hatchling’s Library.');
    assert.equal(await refused({ result: 'file_out_refused', reason: { refusal: 'busy' } }), 'Hatchling answered with something else.');
    const alone = new LiveAgentFiles(new LiveClient(new FakeTransport(), 'test-ui', () => undefined), () => undefined, () => 'request-1');
    assert.equal((await alone.remove('f1')).ok, false);
});
test('a file a message showed reads as removed once a later list no longer holds it, and never by its own name', async () => {
    const bytes = bytesOf(3);
    let held = [{ file: wire(bytes), bytes }];
    const { files } = library((action) => keeping(held)(action));
    await settle();
    assert.equal(files.offered('file-a'), undefined);
    // A name the broker never gives is no file.
    files.observe({ kind: 'file_offered', chat_id: 'local', message_id: 'file-x', file: wire(bytes, { name: '‮exe.txt' }) });
    assert.equal(files.offered('file-x'), undefined);
    files.observe({ kind: 'file_offered', chat_id: 'local', message_id: 'file-a', file: wire(bytes) });
    assert.deepEqual(files.offered('file-a'), { id: 'f1', name: 'report.pdf', bytes: 3, sha256: sha(bytes), keptAt: '2026-11-17T00:00:00Z', reply: null, conversation: 1, removed: false });
    await settle();
    held = [];
    await files.refresh();
    assert.equal(files.offered('file-a').removed, true);
    // Seen again in a replay, it stays as known.
    files.observe({ kind: 'file_offered', chat_id: 'local', message_id: 'file-a', file: wire(bytes) });
    assert.equal(files.offered('file-a').removed, true);
});
test('the adapter keeps a file the stream says was given, and reads the Library again when it changed', async () => {
    const bytes = bytesOf(3);
    const transport = new FakeTransport();
    transport.respond = keeping([{ file: wire(bytes), bytes }]);
    const adapter = new LiveAdapter(transport, 'test-ui');
    transport.open();
    transport.deliver(SAMPLE_WELCOME);
    await settle();
    transport.deliver(sampleEvent(700, 30, { kind: 'file_offered', chat_id: 'local', message_id: 'file-a', file: wire(bytes) }));
    await settle();
    assert.equal(adapter.agentFiles.offered('file-a').name, 'report.pdf');
    const reads = transport.actions('list_library').length;
    transport.deliver(sampleEvent(701, 31, { kind: 'library_changed' }));
    await settle();
    assert.equal(transport.actions('list_library').length, reads + 1);
});
test('a file is saved as bytes, typed as bytes whatever its name, and its address let go a minute later', (context) => {
    context.mock.timers.enable({ apis: ['setTimeout'] });
    const made = [];
    const appended = [];
    const revoked = [];
    const realCreate = URL.createObjectURL;
    const realRevoke = URL.revokeObjectURL;
    let blob;
    URL.createObjectURL = (made_) => { blob = made_; return 'blob:test-1'; };
    URL.revokeObjectURL = (url) => revoked.push(url);
    globalThis.document = {
        createElement: (tag) => {
            const element = { tag, attributes: {}, clicked: 0, removed: false, setAttribute(name, value) { this.attributes[name] = value; }, click() { this.clicked++; }, remove() { this.removed = true; } };
            made.push(element);
            return element;
        },
        body: { append: (element) => appended.push(element) },
    };
    try {
        saveBytes([Uint8Array.from([60, 104]), Uint8Array.from([49, 62])], 'page.html');
        assert.equal(blob.size, 4);
        assert.equal(blob.type, 'application/octet-stream');
        const [link] = made;
        assert.deepEqual([link.tag, link.attributes, link.clicked, link.removed, appended.length], ['a', { href: 'blob:test-1', download: 'page.html' }, 1, true, 1]);
        context.mock.timers.tick(59999);
        assert.deepEqual(revoked, []);
        context.mock.timers.tick(1);
        assert.deepEqual(revoked, ['blob:test-1']);
    }
    finally {
        URL.createObjectURL = realCreate;
        URL.revokeObjectURL = realRevoke;
        delete globalThis.document;
    }
});
test('the browser\'s hash is SHA-256 in lowercase hex, and a page without one has none', async () => {
    const digest = browserDigest();
    assert.equal(await digest(new TextEncoder().encode('abc')), 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
    const real = Object.getOwnPropertyDescriptor(globalThis, 'crypto');
    Object.defineProperty(globalThis, 'crypto', { value: undefined, configurable: true });
    try {
        assert.equal(browserDigest(), null);
    }
    finally {
        Object.defineProperty(globalThis, 'crypto', real);
    }
});
test('what opening a file can do is read from its last extension, in any case', () => {
    for (const [name, kind] of [['setup.EXE', 'program'], ['report.pdf.exe', 'program'], ['run.sh', 'program'], ['tool.mjs', 'program'], ['budget.xlsm', 'macros'], ['old.DOC', 'macros'], ['sheet.xlsb', 'macros'], ['index.HTML', 'page'], ['logo.svg', 'page'], ['feed.xml', 'page'], ['report.pdf', null], ['notes', null], ['exe', null], ['archive.zip', null]])
        assert.equal(runs(name), kind, name);
    assert.match(caution('a.exe'), /^A program: it runs when opened\./u);
    assert.match(caution('a.docm'), /^A document that can run macros\./u);
    assert.match(caution('a.html'), /^A page that can run script/u);
    assert.equal(caution('a.pdf'), 'Made by the agent: open it only if you trust what it made.');
});
test('a download is held to the hash a message said, where the list has no word of the file', async () => {
    const bytes = bytesOf(3);
    const begun = (action) => action.action === 'begin_file_out' ? { result: 'file_out_begun', download_id: 'out-1', file: wire(bytes), parts: 1 } : keeping([{ file: wire(bytes), bytes }])(action);
    // The list was refused: the message's word is all there is.
    const { files, saved } = library((action) => action.action === 'list_library' ? { result: 'refused', reason: 'unavailable' } : begun(action));
    await settle();
    assert.equal(files.current().kind, 'failed');
    files.observe({ kind: 'file_offered', chat_id: 'local', message_id: 'file-a', file: wire(bytes, { sha256: sha(bytesOf(4)) }) });
    assert.equal(files.offered('file-a').removed, false, 'a file read as removed with no list to say so');
    assert.deepEqual([(await files.download('f1')).message, saved.length], ['Hatchling answered with something else. Nothing was saved.', 0]);
    // The same hash as the message's: saved.
    const same = library((action) => action.action === 'list_library' ? { result: 'library', folders: [], removed: [], files: [], standing: STANDING } : begun(action));
    await settle();
    same.files.observe({ kind: 'file_offered', chat_id: 'local', message_id: 'file-a', file: wire(bytes) });
    assert.equal((await same.files.download('f1')).ok, true);
});
test('a name the broker would never give is never offered, listed or downloaded', async () => {
    for (const name of ['report.pdf', 'a', 'out_1-2.tar.gz', 'console.log', 'com', 'nul_file', 'desktop.ini.txt', 'url'])
        assert.equal(isGivenName(name), true, name);
    for (const name of ['setup.exe.', 'CON', 'con.txt', 'Prn.doc', 'aux', 'nul.tar.gz', 'COM0', 'lpt9.x', 'desktop.ini', 'Desktop.INI', 'share.url', 'a.Website', 'x.lnk', 'a.scf', 'a.pif', 'y.library-ms', 'z.search-ms', 'q.searchConnector-ms', 't.theme', 't.themepack', 't.deskthemepack', '.hidden', '../a', ''])
        assert.equal(isGivenName(name), false, name);
    const bytes = bytesOf(3);
    const dotted = wire(bytes, { name: 'setup.exe.' });
    const { files, saved } = library(keeping([{ file: dotted, bytes }]));
    await settle();
    assert.deepEqual(files.current().files, []);
    files.observe({ kind: 'file_offered', chat_id: 'local', message_id: 'file-a', file: dotted });
    assert.equal(files.offered('file-a'), undefined);
    assert.deepEqual([(await files.download('f1')).message, saved.length], ['Hatchling answered with something else. Nothing was saved.', 0]);
});
test('one file downloads at a time: another waits to be asked again', async () => {
    const a = bytesOf(PART_BYTES + 1);
    const b = bytesOf(5);
    const { files, saved } = library(keeping([{ file: wire(a), bytes: a }, { file: wire(b, { id: 'f2', name: 'b.txt' }), bytes: b }]));
    await settle();
    const first = files.download('f1');
    assert.deepEqual(await files.download('f2'), { ok: false, message: 'Another file is downloading. Try again when it’s done.' });
    assert.equal((await first).ok, true);
    // Once it's done, the other downloads.
    assert.equal((await files.download('f2')).ok, true);
    assert.equal(saved.length, 2);
});
test('a list answer older than one already taken changes nothing', async () => {
    const bytes = bytesOf(3);
    const transport = new FakeTransport();
    const asked = [];
    transport.respond = (action, requestId) => {
        if (action.action === 'list_library')
            asked.push(requestId);
        return undefined;
    };
    const client = new LiveClient(transport, 'test-ui', () => undefined);
    let serial = 0;
    const files = new LiveAgentFiles(client, () => undefined, () => `request-${++serial}`, { save: () => undefined, digest: async () => '' });
    client.connect();
    transport.open();
    transport.deliver(SAMPLE_WELCOME);
    await settle();
    const older = files.refresh();
    const newer = files.refresh();
    await settle();
    const [, first, second] = asked;
    const listing = (listed) => ({ result: 'library', folders: [], removed: [], files: listed, standing: STANDING });
    transport.deliver({ type: 'action_result', request_id: second, result: listing([wire(bytes)]) });
    await newer;
    transport.deliver({ type: 'action_result', request_id: first, result: listing([]) });
    await older;
    assert.deepEqual(files.current().files.map(file => file.id), ['f1'], 'an older answer replaced a newer one');
});
test('a file given since the list was read isn\'t said removed until a list read after it', async () => {
    const bytes = bytesOf(3);
    let listed = [];
    const { files } = library((action) => keeping(listed)(action));
    await settle();
    listed = [{ file: wire(bytes), bytes }];
    files.observe({ kind: 'file_offered', chat_id: 'local', message_id: 'file-a', file: wire(bytes) });
    // The list read before it lacks it: not removed for that.
    assert.equal(files.offered('file-a').removed, false);
    await settle();
    assert.equal(files.offered('file-a').removed, false);
});
test('a part larger than a part, or a file short of its length, saves nothing, whatever the hash says', async () => {
    // The whole file in the first part, and nothing in the second: right
    // length and hash, but a part past the bound.
    const whole = bytesOf(PART_BYTES + 1);
    const big = library((action) => action.action === 'list_library' ? { result: 'library', folders: [], removed: [], files: [], standing: STANDING }
        : keeping([{ file: wire(whole), bytes: whole }], { part: (asked) => ({ data: asked.seq === 0 ? Buffer.from(whole).toString('base64') : '' }) })(action));
    assert.deepEqual([(await big.files.download('f1')).message, big.saved.length], ['The file didn’t arrive whole. Nothing was saved. Try again.', 0]);
    // A file ending in zeros, its tail never sent: the hash would match.
    const zeros = new Uint8Array(PART_BYTES + 3);
    zeros.set(bytesOf(PART_BYTES));
    const short = library((action) => action.action === 'list_library' ? { result: 'library', folders: [], removed: [], files: [], standing: STANDING }
        : keeping([{ file: wire(zeros), bytes: zeros }], { part: (asked) => (asked.seq === 1 ? { data: '' } : {}) })(action));
    assert.deepEqual([(await short.files.download('f1')).message, short.saved.length], ['The file didn’t arrive whole. Nothing was saved. Try again.', 0]);
});
test('a message that showed a file removed says so, even when the list can\'t be read again', async () => {
    const bytes = bytesOf(3);
    let listing = true;
    const kept = keeping([{ file: wire(bytes), bytes }]);
    const { files } = library((action) => action.action === 'list_library' && !listing ? { result: 'refused', reason: 'unavailable' } : kept(action));
    await settle();
    files.observe({ kind: 'file_offered', chat_id: 'local', message_id: 'file-a', file: wire(bytes) });
    await settle();
    listing = false;
    assert.equal((await files.remove('f1')).ok, true);
    assert.equal(files.current().kind, 'failed');
    assert.equal(files.offered('file-a').removed, true);
});
test('every file downloads as one .zip, each checked as a download is, and saved once', async () => {
    const { ALL_NAME } = await import('../../dist/app/adapters/live/agent-files.js');
    const a = bytesOf(PART_BYTES + 5);
    const b = bytesOf(7);
    const { files, saved, transport } = library(keeping([{ file: wire(a, { name: 'a.bin' }), bytes: a }, { file: wire(b, { id: 'f2', name: 'a.bin', kept_at: '2026-11-17T01:00:00Z' }), bytes: b }]));
    await settle();
    const going = files.downloadAll();
    assert.deepEqual([files.downloadProgress(), files.downloading('f1'), files.downloading('f9')], [{ done: 0, total: 2 }, true, true]);
    assert.deepEqual(await files.download('f1'), { ok: false, message: 'Another file is downloading. Try again when it’s done.' });
    assert.deepEqual(await files.downloadAll(), { ok: false, message: 'A download is under way. Try again when it’s done.' });
    const result = await going;
    assert.equal(result.ok, true);
    assert.match(result.message, /^Gave your browser hatchling-library\.zip, 2 files, .*\. 2 of them can run something when opened: open them only if you trust what the agent made\.$/u);
    assert.equal(files.downloadProgress(), null);
    assert.deepEqual(saved.map(([, name]) => name), [ALL_NAME]);
    assert.deepEqual(transport.actions('begin_file_out').map(action => action.id), ['f1', 'f2']);
    // The .zip holds each file whole, by its name.
    const zip = Buffer.from(saved[0][0]);
    assert.equal(zip.readUInt32LE(zip.length - 22), 0x06054b50);
    assert.equal(zip.readUInt16LE(zip.length - 12), 2);
    assert.ok(zip.includes(Buffer.from(a)) && zip.includes(Buffer.from(b)));
    // Two of one name are told apart, and each keeps when it was kept.
    assert.ok(zip.includes(Buffer.from('a.bin')) && zip.includes(Buffer.from('a-2.bin')));
    const { dosTime } = await import('../../dist/app/adapters/live/zip.js');
    const [time, date] = dosTime('2026-11-17T01:00:00Z');
    const second = zip.indexOf(Buffer.from('a-2.bin')) - 30;
    assert.deepEqual([zip.readUInt16LE(second + 10), zip.readUInt16LE(second + 12)], [time, date]);
    // Files that only show themselves say only that the agent made them.
    const plain = library(keeping([{ file: wire(b, { name: 'b.txt' }), bytes: b }, { file: wire(b, { id: 'f2', name: 'c.txt' }), bytes: b }]));
    await settle();
    assert.match((await plain.files.downloadAll()).message, /The agent made them: open them only if you trust them\.$/u);
});
test('every file\'s download counts each file done, so the owner sees it go', async () => {
    const a = bytesOf(3);
    const counts = [];
    const transport = new FakeTransport();
    transport.respond = keeping([{ file: wire(a), bytes: a }, { file: wire(a, { id: 'f2', name: 'b.txt' }), bytes: a }]);
    const client = new LiveClient(transport, 'test-ui', () => undefined);
    let files;
    let serial = 0;
    files = new LiveAgentFiles(client, () => counts.push(files?.downloadProgress() ?? null), () => `request-${++serial}`, { save: () => undefined, digest: async (bytes) => sha(bytes), wait: 1000 });
    client.connect();
    transport.open();
    transport.deliver(SAMPLE_WELCOME);
    await settle();
    counts.length = 0;
    await files.downloadAll();
    const seen = counts.filter(count => count !== null).map(count => `${count.done}/${count.total}`);
    assert.deepEqual([...new Set(seen)], ['0/2', '1/2', '2/2']);
    assert.equal(counts.at(-1), null);
});
test('a download that throws saves nothing and says so', async () => {
    const a = bytesOf(3);
    const throwing = () => { throw new Error('out of memory'); };
    const one = library(keeping([{ file: wire(a), bytes: a }, { file: wire(a, { id: 'f2', name: 'b.txt' }), bytes: a }]), { save: throwing });
    await settle();
    assert.deepEqual(await one.files.download('f1'), { ok: false, message: 'The file couldn’t be downloaded. Nothing was saved. Try again.' });
    assert.deepEqual(await one.files.downloadAll(), { ok: false, message: 'The files couldn’t be put together: too many for one .zip, or too much for this page’s memory. Nothing was saved. Choose fewer, or download them one at a time.' });
    assert.deepEqual([one.files.downloading('f1'), one.files.downloadProgress()], [false, null]);
});
test('a download of every file that fails anywhere saves nothing, and says which', async () => {
    const a = bytesOf(3);
    const b = bytesOf(4);
    const { files, saved } = library(keeping([{ file: wire(a), bytes: a }, { file: wire(b, { id: 'f2', name: 'b.txt', sha256: sha(bytesOf(5)) }), bytes: b }]));
    await settle();
    assert.deepEqual(await files.downloadAll(), { ok: false, message: 'b.txt: The file didn’t arrive whole. Nothing was saved. Try again.' });
    assert.deepEqual(saved, []);
    // Nothing listed, nothing read yet, or no hash to check with: nothing asked.
    const empty = library(keeping([]));
    await settle();
    assert.deepEqual(await empty.files.downloadAll(), { ok: false, message: 'Hatchling’s Library holds nothing to download.' });
    const unread = new LiveAgentFiles(new LiveClient(new FakeTransport(), 'test-ui', () => undefined), () => undefined, () => 'request-1', { save: () => assert.fail('saved'), digest: async () => '' });
    assert.deepEqual(await unread.downloadAll(), { ok: false, message: 'Hatchling’s Library hasn’t been read yet. Refresh it, then try again.' });
    const blind = library(keeping([{ file: wire(a), bytes: a }]), { digest: null });
    await settle();
    assert.match((await blind.files.downloadAll()).message, /^This page can’t check a file’s hash/u);
    assert.deepEqual(blind.transport.actions('begin_file_out'), []);
    // While one file downloads, every file waits.
    const busy = library(keeping([{ file: wire(a), bytes: a }]));
    await settle();
    const one = busy.files.download('f1');
    assert.deepEqual(await busy.files.downloadAll(), { ok: false, message: 'A download is under way. Try again when it’s done.' });
    await one;
});
test('a folder is listed by its name and counts; an item whose name or counts the broker never gives is listed by its size alone, by an id that can be one', async () => {
    const folder = (fields) => ({ id: 'd'.repeat(64), name: 'site', files: 3, bytes: 4096, sha256: 'ab', kept_at: '2026-11-18T00:00:00Z', conversation: 1, left_out: { links: 0, mounts: 0, others: 0 }, ...fields });
    const bytes = bytesOf(3);
    const answer = {
        result: 'library',
        files: [wire(bytes), wire(bytes, { id: 'a'.repeat(64), name: 'CON.txt' }), wire(bytes, { id: 'not-an-id', name: '.hidden' }), wire(bytes, { id: '1'.repeat(64), name: 'big.bin', bytes: 8 * 1024 * 1024 + 1 })],
        folders: [
            folder({}),
            folder({ id: 'b'.repeat(64), name: '../up' }),
            folder({ id: 'c'.repeat(64), files: 0 }),
            folder({ id: 'e'.repeat(64), files: 513 }),
            folder({ id: 'f'.repeat(64), bytes: 32 * 1024 * 1024 + 1 }),
            folder({ id: '0'.repeat(64), name: 'one', files: 1, bytes: 32 * 1024 * 1024 }),
            folder({ id: '2'.repeat(64), name: 'most', files: 512 }),
        ],
        removed: [],
        standing: STANDING,
    };
    const { files } = library((action) => action.action === 'list_library' ? answer : undefined);
    await settle();
    const state = files.current();
    assert.equal(state.kind, 'read');
    assert.deepEqual(state.files.map(file => file.id), ['f1']);
    assert.deepEqual(state.folders, [
        { id: 'd'.repeat(64), name: 'site', files: 3, bytes: 4096, sha256: 'ab', leftOut: { links: 0, mounts: 0, others: 0 }, keptAt: '2026-11-18T00:00:00Z', reply: null, conversation: 1 },
        { id: '0'.repeat(64), name: 'one', files: 1, bytes: 32 * 1024 * 1024, sha256: 'ab', leftOut: { links: 0, mounts: 0, others: 0 }, keptAt: '2026-11-18T00:00:00Z', reply: null, conversation: 1 },
        { id: '2'.repeat(64), name: 'most', files: 512, bytes: 4096, sha256: 'ab', leftOut: { links: 0, mounts: 0, others: 0 }, keptAt: '2026-11-18T00:00:00Z', reply: null, conversation: 1 },
    ]);
    // Every item it can't name is shown; one whose id the broker never gives can't be removed from here.
    assert.deepEqual(state.unnamed, [
        { id: 'a'.repeat(64), bytes: 3, removable: true },
        { id: 'not-an-id', bytes: 3, removable: false },
        { id: '1'.repeat(64), bytes: 8 * 1024 * 1024 + 1, removable: true },
        { id: 'b'.repeat(64), bytes: 4096, removable: true },
        { id: 'c'.repeat(64), bytes: 4096, removable: true },
        { id: 'e'.repeat(64), bytes: 4096, removable: true },
        { id: 'f'.repeat(64), bytes: 32 * 1024 * 1024 + 1, removable: true },
    ]);
    assert.deepEqual(state.standing, { files: 1, fileLimit: 64, bytes: 3, byteLimit: 268435456, given: 1, givenLimit: 16, off: false, filesInAll: 1, filesInAllLimit: 4096, read: 3, readLimit: 134217728, conversation: 1 });
});
test('a folder asked for as a file, or a place it doesn\'t hold, is said and saves nothing', async () => {
    const bytes = bytesOf(3);
    for (const [refusal, said] of [
        ['a_folder', 'Hatchling answered for a folder as if it were a file.'],
        ['not_a_folder', 'Hatchling answered for a file as if it were a folder.'],
        ['out_of_range', 'Hatchling answered for a file its folder doesn’t hold.'],
    ]) {
        const { files, saved } = library((action) => action.action === 'begin_file_out' ? { result: 'file_out_refused', reason: { refusal } } : keeping([{ file: wire(bytes), bytes }])(action));
        await settle();
        const result = await files.download('f1');
        assert.deepEqual([result.ok, saved.length], [false, 0]);
        assert.match(result.message, new RegExp(said.replace(/[.*+?^${}()|[\]\\]/gu, '\\$&'), 'u'));
    }
});
/** A run id of the broker's grammar. */
const runId = (n) => `run-${String(n).padStart(24, '0')}`;
/** A chat showing `answers`, each a message id and the run that wrote it. */
const chatRuns = (answers) => {
    const runs = new Map(answers);
    return { of: id => runs.get(id), shown: () => new Set(runs.values()) };
};
test('what the agent gave sits under the reply its run wrote: a file, a folder, one removed since; a live notice isn\'t repeated, nor an item from before replies', async () => {
    const bytes = bytesOf(3);
    const folder = { id: 'd'.repeat(64), name: 'site', files: 2, bytes: 7, sha256: 'ab', kept_at: '2026-11-18T00:00:00Z', conversation: 1, reply: runId(1), left_out: { links: 0, mounts: 0, others: 0 } };
    const answer = {
        result: 'library',
        files: [wire(bytes, { reply: runId(1) }), wire(bytes, { id: 'f2', name: 'b.txt', reply: runId(2) }), wire(bytes, { id: 'f3', name: 'old.txt' }), wire(bytes, { id: 'f4', name: 'last.txt', reply: runId(4), conversation: 0 }), wire(bytes, { id: 'f5', name: 'before.txt', reply: runId(5), conversation: 0 })],
        folders: [folder],
        removed: [{ id: 'e'.repeat(64), name: 'gone.txt', folder: false, files: 1, bytes: 2, reply: runId(1) }, { id: 'c'.repeat(64), name: 'away', folder: true, files: 3, bytes: 9, reply: runId(3) }, { id: 'b'.repeat(64), name: '.hidden', folder: false, files: 1, bytes: 1, reply: runId(1) }, { id: 'not-an-id', name: 'odd.txt', folder: false, files: 1, bytes: 1, reply: runId(1) }],
        standing: STANDING,
    };
    const { files } = library((action) => action.action === 'list_library' ? answer : undefined, { runs: chatRuns([['m1', runId(1)], ['m4', runId(4)]]) });
    // Before the list is read, nothing.
    assert.deepEqual([files.givenIn('m1'), files.givenElsewhere()], [[], []]);
    await settle();
    const ids = (cards) => cards.map(card => card.kind === 'file' ? card.file.id : card.kind === 'folder' ? card.folder.id : card.removed.id);
    assert.deepEqual(ids(files.givenIn('m1')), ['d'.repeat(64), 'f1', 'e'.repeat(64)]);
    assert.deepEqual(files.givenIn('m1').map(card => card.kind), ['folder', 'file', 'removed']);
    // A reply the chat knows no run of has none; one from another conversation keeps its own.
    assert.deepEqual(files.givenIn('m-unknown'), []);
    assert.deepEqual(ids(files.givenIn('m4')), ['f4']);
    // Given in this conversation, under no reply shown: never an item from before replies, nor another conversation's.
    assert.deepEqual(ids(files.givenElsewhere()), ['f2', 'c'.repeat(64)]);
    // A file a live notice showed isn't shown again under its reply.
    files.observe({ kind: 'file_offered', chat_id: 'local', message_id: 'file-1', file: wire(bytes, { reply: runId(1) }) });
    await settle();
    assert.deepEqual(ids(files.givenIn('m1')), ['d'.repeat(64), 'e'.repeat(64)]);
});
test('a folder the stream says was given is kept by its message, and reads as removed once a later list no longer holds it', async () => {
    const folder = { id: 'd'.repeat(64), name: 'site', files: 2, bytes: 7, sha256: 'ab', kept_at: '2026-11-18T00:00:00Z', conversation: 1, reply: runId(1), left_out: { links: 0, mounts: 0, others: 0 } };
    let held = [folder];
    const { files } = library((action) => action.action === 'list_library' ? { result: 'library', files: [], folders: held, removed: [], standing: STANDING } : undefined);
    await settle();
    assert.equal(files.offeredFolder('fold-1'), undefined);
    files.observeFolder({ kind: 'folder_offered', chat_id: 'local', message_id: 'fold-1', folder });
    await settle();
    assert.deepEqual(files.offeredFolder('fold-1'), { id: 'd'.repeat(64), name: 'site', files: 2, bytes: 7, sha256: 'ab', leftOut: { links: 0, mounts: 0, others: 0 }, keptAt: '2026-11-18T00:00:00Z', reply: runId(1), conversation: 1, removed: false });
    // Shown again, nothing doubles.
    files.observeFolder({ kind: 'folder_offered', chat_id: 'local', message_id: 'fold-1', folder });
    await settle();
    held = [];
    await files.refresh();
    assert.equal(files.offeredFolder('fold-1').removed, true);
    // One whose name the broker never gives is never kept.
    files.observeFolder({ kind: 'folder_offered', chat_id: 'local', message_id: 'fold-2', folder: { ...folder, name: '../x' } });
    assert.equal(files.offeredFolder('fold-2'), undefined);
    // With no chat to say runs, nothing sits under a reply, and all is elsewhere.
    assert.deepEqual(files.givenIn('m1'), []);
    assert.deepEqual(files.givenElsewhere(), []);
});
test('a folder removed since its live notice is not said again under its reply, and one under no reply shown sits apart', async () => {
    const folder = (n, reply) => ({ id: String(n).repeat(64), name: `site${n}`, files: 2, bytes: 7, sha256: 'ab', kept_at: '2026-11-18T00:00:00Z', conversation: 1, reply, left_out: { links: 0, mounts: 0, others: 0 } });
    const gone = folder(1, runId(1));
    const answer = { result: 'library', files: [], folders: [folder(2, runId(9))], removed: [{ id: gone.id, name: gone.name, folder: true, files: 2, bytes: 7, reply: runId(1) }], standing: STANDING };
    const { files } = library((action) => action.action === 'list_library' ? answer : undefined, { runs: chatRuns([['m1', runId(1)]]) });
    files.observeFolder({ kind: 'folder_offered', chat_id: 'local', message_id: 'fold-1', folder: gone });
    await settle();
    assert.deepEqual(files.givenIn('m1'), []);
    assert.equal(files.offeredFolder('fold-1').removed, true);
    assert.deepEqual(files.givenElsewhere().map(card => card.kind === 'folder' ? card.folder.name : card.kind), ['site2']);
});
/** The files of `zip`, by the name each has, read by its central directory. */
function unzipped(zip) {
    const end = zip.length - 22;
    assert.equal(zip.readUInt32LE(end), 0x06054b50);
    const out = new Map();
    let at = zip.readUInt32LE(end + 16);
    for (let entry = 0; entry < zip.readUInt16LE(end + 10); entry++) {
        assert.equal(zip.readUInt32LE(at), 0x02014b50);
        const size = zip.readUInt32LE(at + 24);
        const nameLength = zip.readUInt16LE(at + 28);
        const local = zip.readUInt32LE(at + 42);
        const name = zip.subarray(at + 46, at + 46 + nameLength).toString('latin1');
        const start = local + 30 + zip.readUInt16LE(local + 26);
        out.set(name, { bytes: Buffer.from(zip.subarray(start, start + size)), attributes: zip.readUInt32LE(at + 38) });
        at += 46 + nameLength;
    }
    return out;
}
/** The list a folder of `entries` (each `{ path, bytes }`) has, as the Library keeps it. */
const listOf = (entries) => entries.map(({ path, bytes }) => ({ path, bytes: bytes.length, sha256: sha(bytes) }));
/** A folder of `entries` as the wire says it, its list's hash the Library's. */
function folderWire(entries, fields = {}) {
    return { id: 'd'.repeat(64), name: 'site', files: entries.length, bytes: entries.reduce((all, entry) => all + entry.bytes.length, 0), sha256: sha(Buffer.from(JSON.stringify(listOf(entries)))), kept_at: '2026-11-18T00:00:00Z', conversation: 1, reply: runId(1), left_out: { links: 0, mounts: 0, others: 0 }, ...fields };
}
/**
 * A broker keeping the folder `folder` of `entries` and the files `files`,
 * answering as the Library does; `fields` change a page, a file's start or a part.
 */
function keepingFolder(folder, entries, files = [], fields = {}) {
    const asFiles = keeping(files);
    let begun = null;
    return (action) => {
        switch (action.action) {
            case 'list_library': return { result: 'library', folders: [folder], removed: [], files: files.map(entry => entry.file), standing: STANDING };
            case 'list_folder': {
                const list = listOf(entries);
                return { result: 'folder', id: action.id, from: action.from, total: list.length, entries: list.slice(action.from, action.from + 32), ...fields.page?.(action) };
            }
            case 'begin_file_out': {
                if (action.file === undefined) {
                    begun = null;
                    return asFiles(action);
                }
                const entry = entries[action.file];
                begun = entry.bytes;
                return { result: 'folder_file_out_begun', download_id: 'out-2', id: action.id, file: action.file, path: entry.path, bytes: entry.bytes.length, sha256: sha(entry.bytes), parts: Math.ceil(entry.bytes.length / PART_BYTES), ...fields.begun?.(action) };
            }
            case 'file_out_part': {
                if (begun === null)
                    return asFiles(action);
                const part = begun.subarray(action.seq * PART_BYTES, (action.seq + 1) * PART_BYTES);
                return { result: 'file_out_part_read', download_id: action.download_id, seq: action.seq, data: Buffer.from(part).toString('base64'), ...fields.part?.(action) };
            }
            default: return asFiles(action);
        }
    };
}
test('a folder downloads as one .zip under its name, its folders as they were, every file read out, checked and whole', async () => {
    // At least 100 files, nested: every one comes back byte for byte.
    const entries = [
        { path: 'index.html', bytes: bytesOf(10) },
        { path: 'assets/big.bin', bytes: bytesOf(PART_BYTES * 2 + 3) },
        { path: 'assets/empty', bytes: new Uint8Array(0) },
        ...Array.from({ length: 110 }, (_, n) => ({ path: `pages/${n % 4}/deep/p${n}.md`, bytes: Buffer.from(`page ${n}`) })),
    ];
    const folder = folderWire(entries);
    const { files, saved, transport } = library(keepingFolder(folder, entries));
    await settle();
    const going = files.download(folder.id);
    assert.deepEqual([files.downloading(folder.id), files.downloadProgress()], [true, { done: 0, total: 113 }]);
    const result = await going;
    assert.equal(result.ok, true, result.message);
    assert.match(result.message, /^Gave your browser site\.zip, 113 files, .*, to save\. 2 of them can run something when opened: open them only if you trust what the agent made\.$/u);
    assert.deepEqual(transport.actions('list_folder').map(action => action.from), [0, 32, 64, 96]);
    assert.deepEqual(transport.actions('begin_file_out').map(action => action.file), entries.map((_, at) => at));
    assert.deepEqual([saved.length, saved[0][1]], [1, 'site.zip']);
    const zip = unzipped(saved[0][0]);
    for (const entry of entries)
        assert.ok(zip.get(`site/${entry.path}`)?.bytes.equals(Buffer.from(entry.bytes)), entry.path);
    // Each folder is an entry of its own, once, marked as a folder; nothing else is in it.
    const folders = [...zip.keys()].filter(name => name.endsWith('/'));
    assert.deepEqual(folders.slice(0, 2), ['site/', 'site/assets/']);
    assert.equal(new Set(folders).size, folders.length);
    assert.ok(folders.every(name => zip.get(name).attributes === 0x10 && zip.get(name).bytes.length === 0));
    assert.equal(zip.size, entries.length + folders.length);
    assert.equal(files.downloadProgress(), null);
    assert.equal(files.downloading(folder.id), false);
});
test('a folder the stream gave downloads before the list holds it; one being read can\'t be asked again', async () => {
    const entries = [{ path: 'a.txt', bytes: bytesOf(4) }];
    const folder = folderWire(entries);
    const respond = keepingFolder(folder, entries);
    const { files, saved } = library((action) => action.action === 'list_library' ? { result: 'library', files: [], folders: [], removed: [], standing: STANDING } : respond(action));
    await settle();
    files.observeFolder({ kind: 'folder_offered', chat_id: 'local', message_id: 'fold-1', folder });
    const going = files.download(folder.id);
    assert.deepEqual(await files.download(folder.id), { ok: false, message: 'It is already downloading.' });
    assert.equal((await going).ok, true);
    assert.equal(saved[0][1], 'site.zip');
});
test('a folder\'s list is held to the folder chosen, page by page and to its hash, and each file to its entry; any that isn\'t saves nothing', async () => {
    const entries = [{ path: 'a/one.txt', bytes: bytesOf(5) }, { path: 'two.txt', bytes: bytesOf(PART_BYTES + 1) }];
    const said = async (folderFields, fields, wire_ = entries) => {
        const folder = folderWire(wire_, folderFields);
        const { files, saved } = library(keepingFolder(folder, entries, [], fields));
        await settle();
        const result = await files.download(folder.id);
        assert.deepEqual([result.ok, saved.length], [false, 0], result.message);
        return result.message;
    };
    const UNEXPECTED = 'site: Hatchling answered with something else. Nothing was saved.';
    const NOT_LISTED = 'site: Hatchling’s list of its files doesn’t match the folder. Nothing was saved.';
    // A page of another folder, from another place, of another count, or short.
    assert.equal(await said({}, { page: () => ({ id: 'e'.repeat(64) }) }), UNEXPECTED);
    assert.equal(await said({}, { page: action => ({ from: action.from + 1 }) }), UNEXPECTED);
    assert.equal(await said({}, { page: () => ({ total: 3 }) }), UNEXPECTED);
    assert.equal(await said({}, { page: () => ({ entries: [] }) }), UNEXPECTED);
    assert.equal(await said({}, { page: () => ({ result: 'inbox', files: [] }) }), UNEXPECTED);
    assert.equal(await said({}, { page: () => ({ result: 'file_out_refused', reason: { refusal: 'damaged' } }) }), 'site: Hatchling’s copy of it is damaged. Nothing was saved. Remove it, and ask the agent for it again.');
    // A list that isn't the folder's: another hash, another size, a hash that can't be one, or a file larger than one can be.
    assert.equal(await said({ sha256: sha(bytesOf(1)) }, {}), NOT_LISTED);
    assert.equal(await said({ bytes: 1 }, {}), NOT_LISTED);
    const upper = listOf(entries).map(entry => ({ ...entry, sha256: entry.sha256.toUpperCase() }));
    assert.equal(await said({ sha256: sha(Buffer.from(JSON.stringify(upper))) }, { page: action => ({ entries: upper.slice(action.from) }) }), NOT_LISTED);
    const large = listOf(entries).map((entry, at) => at === 0 ? { ...entry, bytes: FILE_OUT_BYTES + 1 } : entry);
    assert.equal(await said({ sha256: sha(Buffer.from(JSON.stringify(large))), bytes: large.reduce((all, entry) => all + entry.bytes, 0) }, { page: action => ({ entries: large.slice(action.from) }) }), NOT_LISTED);
    // A path Hatchling never keeps, whatever the list's hash says.
    const bad = [{ path: '../out.txt', bytes: bytesOf(1) }];
    const climbing = folderWire(bad);
    const climb = library(keepingFolder(climbing, bad));
    await settle();
    assert.deepEqual(await climb.files.download(climbing.id), { ok: false, message: 'site: A path in it isn’t one Hatchling keeps (a step of a path is empty, . or ..). Nothing was saved.' });
    // A file that starts as another's: its folder, place, path, size, hash or parts.
    for (const begun of [{ id: 'e'.repeat(64) }, { file: 9 }, { path: 'x.txt' }, { bytes: 4 }, { sha256: sha(bytesOf(9)) }, { parts: 7 }])
        assert.equal(await said({}, { begun: () => begun }), UNEXPECTED, JSON.stringify(begun));
    assert.equal(await said({}, { begun: () => ({ result: 'file_out_refused', reason: { refusal: 'out_of_range' } }) }), 'site: Hatchling answered for a file its folder doesn’t hold.');
    // A part that isn't the file's.
    assert.equal(await said({}, { part: () => ({ data: Buffer.from('changed').toString('base64') }) }), 'site: The file didn’t arrive whole. Nothing was saved. Try again.');
    // No answer at all, to a page or a file's start.
    const quiet = async (name) => {
        const folder = folderWire(entries);
        const respond = keepingFolder(folder, entries);
        const { files } = library((action) => action.action === name ? undefined : respond(action), { wait: 20 });
        await settle();
        return (await files.download(folder.id)).message;
    };
    assert.equal(await quiet('list_folder'), 'site: Hatchling didn’t answer in time. Nothing was saved.');
    assert.equal(await quiet('begin_file_out'), 'site: Hatchling didn’t answer in time. Nothing was saved.');
});
test('what a folder\'s walk left out, a .git in it, and paths too long for Explorer are said when it is saved', async () => {
    const long = `${'d'.repeat(120)}/${'e'.repeat(80)}.txt`;
    const entries = [{ path: '.git/hooks/pre-commit', bytes: bytesOf(3) }, { path: long, bytes: bytesOf(2) }, { path: 'run.sh', bytes: bytesOf(1) }];
    const folder = folderWire(entries, { left_out: { links: 2, mounts: 1, others: 1 } });
    const { files } = library(keepingFolder(folder, entries));
    await settle();
    const { message } = await files.download(folder.id);
    assert.equal(message, `Gave your browser site.zip, 3 files, ${message.match(/3 files, ([^,]+),/u)[1]}, to save. 1 of them can run something when opened: open them only if you trust what the agent made. It holds a git repository: git run in it can run that repository’s hooks and settings, so look at them before you use git there. 1 path in it is long: unpack it near the top of a drive, or Windows’ Explorer may not unpack it. site: Left out when it was given: 2 links, 1 other drive, 1 special file.`);
    // Two long paths are said as two.
    const two = [{ path: long, bytes: bytesOf(1) }, { path: `${long}x`, bytes: bytesOf(1) }];
    const both = folderWire(two);
    const second = library(keepingFolder(both, two));
    await settle();
    assert.match((await second.files.download(both.id)).message, / 2 paths in it are long: unpack it near the top of a drive, or Windows’ Explorer may not unpack them\.$/u);
});
test('items chosen download as one .zip, in the order asked: files by name, folders under theirs, names at the top made distinct', async () => {
    const entries = [{ path: 'inner.txt', bytes: bytesOf(6) }];
    const folder = folderWire(entries, { name: 'notes.txt' });
    const a = bytesOf(3);
    const { files, saved, transport } = library(keepingFolder(folder, entries, [{ file: wire(a, { name: 'notes.txt' }), bytes: a }]));
    await settle();
    const going = files.downloadMany(['f1', 'gone', folder.id], 'hatchling-chosen.zip');
    assert.deepEqual([files.downloading('f1'), files.downloading(folder.id), files.downloadProgress()], [true, true, { done: 0, total: 2 }]);
    assert.deepEqual(await files.downloadMany(['f1'], 'x.zip'), { ok: false, message: 'A download is under way. Try again when it’s done.' });
    const result = await going;
    assert.equal(result.ok, true, result.message);
    assert.match(result.message, /^Gave your browser hatchling-chosen\.zip, 2 files,/u);
    assert.deepEqual(transport.actions('begin_file_out').map(action => [action.id, action.file]), [['f1', undefined], [folder.id, 0]]);
    const zip = unzipped(saved[0][0]);
    assert.deepEqual([...zip.keys()], ['notes.txt', 'notes-2.txt/', 'notes-2.txt/inner.txt']);
    assert.equal(saved[0][1], 'hatchling-chosen.zip');
    // Nothing listed among them: said, nothing asked.
    assert.deepEqual(await files.downloadMany(['gone'], 'x.zip'), { ok: false, message: 'Nothing chosen is in Hatchling’s Library now. Refresh it, then choose again.' });
    const blind = library(keepingFolder(folder, entries), { digest: null });
    await settle();
    assert.match((await blind.files.downloadMany([folder.id], 'x.zip')).message, /^This page can’t check a file’s hash/u);
    assert.match((await blind.files.download(folder.id)).message, /^This page can’t check a file’s hash/u);
    // Download all takes the folders too.
    const all = library(keepingFolder(folder, entries, [{ file: wire(a, { name: 'notes.txt' }), bytes: a }]));
    await settle();
    assert.match((await all.files.downloadAll()).message, /^Gave your browser hatchling-library\.zip, 2 files,/u);
    // A file in the choice that fails names itself.
    const failing = library(keepingFolder(folder, entries, [{ file: wire(a, { sha256: sha(bytesOf(9)) }), bytes: a }]));
    await settle();
    assert.deepEqual(await failing.files.downloadMany(['f1'], 'x.zip'), { ok: false, message: 'report.pdf: The file didn’t arrive whole. Nothing was saved. Try again.' });
});
test('the items chosen are those the Library was last listed with; removing one lets it go', async () => {
    const a = bytesOf(3);
    const { files } = library(keeping([{ file: wire(a), bytes: a }, { file: wire(a, { id: 'f2', name: 'b.txt' }), bytes: a }]));
    assert.deepEqual([...files.chosen()], []);
    await settle();
    files.choose('f2', true);
    files.choose('f1', true);
    files.choose('nowhere', true);
    assert.deepEqual([...files.chosen()], ['f2', 'f1']);
    files.choose('f2', false);
    assert.deepEqual([...files.chosen()], ['f1']);
    assert.equal((await files.remove('f1')).ok, true);
    assert.deepEqual([...files.chosen()], []);
    files.choose('f1', true);
    assert.deepEqual([...files.chosen()], []);
    // Removed, then listed again: not chosen, though it once was.
    const relisting = library((action) => action.action === 'remove_library_file' ? { result: 'library_file_removed', id: action.id } : action.action === 'list_library' ? { result: 'library', files: [wire(a)], folders: [], removed: [], standing: STANDING } : undefined);
    await settle();
    relisting.files.choose('f1', true);
    assert.equal((await relisting.files.remove('f1')).ok, true);
    await relisting.files.refresh();
    assert.deepEqual([...relisting.files.chosen()], []);
});
test('a folder\'s paths are held to the broker\'s rules here, each one refused for the right reason', () => {
    assert.equal(folderPathsRefused(['index.html', 'a/b/c.txt', '.github/workflows/ci.yml', 'with space.txt', '-dash', 'a/b/d']), null);
    const refused = [
        [['a//b'], 'a step of a path is empty, . or ..'],
        [['/abs'], 'a step of a path is empty, . or ..'],
        [['a/./b'], 'a step of a path is empty, . or ..'],
        [['a/../b'], 'a step of a path is empty, . or ..'],
        [['x'.repeat(256)], 'a name is longer than 255 bytes'],
        [['caf\u00e9'], 'a name isn’t plain ASCII, or holds a character Windows forbids'],
        [['tab\there'], 'a name isn’t plain ASCII, or holds a character Windows forbids'],
        [['del\u007f'], 'a name isn’t plain ASCII, or holds a character Windows forbids'],
        ...['<', '>', ':', '"', '\\', '|', '?', '*'].map(char => [[`a${char}b`], 'a name isn’t plain ASCII, or holds a character Windows forbids']),
        [[' lead'], 'a name starts or ends with a space, or ends with a dot'],
        [['trail '], 'a name starts or ends with a space, or ends with a dot'],
        [['dot.'], 'a name starts or ends with a space, or ends with a dot'],
        ...['con', 'PRN', 'aux.txt', 'nul .txt', 'COM1', 'lpt9.tar.gz', 'conin$', 'CONOUT$.log', 'clock$'].map(name => [[`d/${name}`], 'a name is one Windows keeps for a device']),
        ...['desktop.ini', 'x.lnk', 'x.URL', 'a.library-ms', 'a.scf', 'a.theme'].map(name => [[name], 'a name is a kind of file Windows acts on when its folder is shown']),
        [[`${'a/'.repeat(16)}b`], 'a path is more than 16 steps deep'],
        [[Array.from({ length: 5 }, () => 'y'.repeat(250)).join('/')], 'a path is longer than 1,024 bytes'],
        [['a.txt', 'A.TXT'], 'two paths are the same but for case'],
        [['a', 'a/b'], 'a path is a file where another has a folder'],
        [['x/y/z', 'X/Y'], 'a path is a file where another has a folder'],
    ];
    for (const [paths, why] of refused)
        assert.equal(folderPathsRefused(paths), why, JSON.stringify(paths));
    // At the bounds, kept.
    assert.equal(folderPathsRefused([`${'a/'.repeat(15)}b`, 'x'.repeat(255), 'com10', 'lpt', 'conin', 'nul_x']), null);
    // A path of 1,024 bytes is kept, one of 1,025 isn't.
    const at = (length) => [...Array.from({ length: 4 }, () => 'p'.repeat(250)), 'q'.repeat(length - 1004)].join('/');
    assert.equal(at(1024).length, 1024);
    assert.equal(folderPathsRefused([at(1024)]), null);
    assert.equal(folderPathsRefused([at(1025)]), 'a path is longer than 1,024 bytes');
    assert.equal(leftOutWords({ links: 0, mounts: 0, others: 0 }), '');
    assert.equal(leftOutWords({ links: 1, mounts: 2, others: 0 }), 'Left out when it was given: 1 link, 2 other drives.');
});
test('the Library is grouped by conversation, the open one first, then newest; by reply in the order given, none last', () => {
    const file = (id, conversation, reply, hour) => ({ id, name: `${id}.txt`, bytes: 1, sha256: 'ab', keptAt: `2026-11-18T0${hour}:00:00Z`, reply, conversation });
    const groups = libraryGroups([file('a', 1, 'r1', 3), file('b', 2, null, 0), file('c', 2, 'r2', 2), file('d', 3, 'r3', 1), file('e', 2, 'r3', 4), file('f', 2, 'r2', 5)], [], 2);
    assert.deepEqual(groups.map(group => [group.conversation, group.open, group.replies.map(reply => [reply.reply, reply.items.map(item => item.file.id)])]), [
        [2, true, [['r2', ['c', 'f']], ['r3', ['e']], [null, ['b']]]],
        [3, false, [['r3', ['d']]]],
        [1, false, [['r1', ['a']]]],
    ]);
    assert.deepEqual(libraryGroups([], [], 1), []);
});
test('a folder\'s download counts each of its files done, so the owner sees it go', async () => {
    const entries = [{ path: 'a.txt', bytes: bytesOf(1) }, { path: 'b/c.txt', bytes: bytesOf(2) }, { path: 'd.txt', bytes: bytesOf(3) }];
    const folder = folderWire(entries);
    const transport = new FakeTransport();
    transport.respond = keepingFolder(folder, entries);
    const client = new LiveClient(transport, 'test-ui', () => undefined);
    const counts = [];
    let files;
    let serial = 0;
    files = new LiveAgentFiles(client, () => counts.push(files?.downloadProgress() ?? null), () => `request-${++serial}`, { save: () => undefined, digest: async (bytes) => sha(bytes), wait: 1000 });
    client.connect();
    transport.open();
    transport.deliver(SAMPLE_WELCOME);
    await settle();
    counts.length = 0;
    assert.equal((await files.download(folder.id)).ok, true);
    const seen = counts.filter(count => count !== null).map(count => `${count.done}/${count.total}`);
    assert.deepEqual([...new Set(seen)], ['0/3', '1/3', '2/3', '3/3']);
    assert.equal(counts.at(-1), null);
});
test('a git repository is seen in a .git, whatever its case, or bare, by its HEAD, objects and refs', () => {
    assert.equal(holdsGit(['site/.git/config']), true);
    assert.equal(holdsGit(['site/.GIT/HEAD']), true);
    assert.equal(holdsGit(['.git']), true);
    assert.equal(holdsGit(['repo.git/HEAD', 'repo.git/objects/ab/cdef', 'repo.git/refs/heads/main']), true);
    assert.equal(holdsGit(['HEAD', 'objects/pack/p.pack', 'refs/tags/v1']), true);
    // Not one without all three, nor with them in different folders.
    assert.equal(holdsGit(['a/HEAD', 'a/objects/x']), false);
    assert.equal(holdsGit(['a/HEAD', 'a/refs/x']), false);
    assert.equal(holdsGit(['a/HEAD', 'b/objects/x', 'b/refs/x']), false);
    assert.equal(holdsGit(['a/AHEAD', 'a/objects/x', 'a/refs/x']), false);
    // A name that only ends in HEAD isn't one, even beside objects and refs that would make it one.
    assert.equal(holdsGit(['xHEAD', 'xobjects/a', 'xrefs/b']), false);
    assert.equal(holdsGit(['site/git/x', 'site/.gitignore', 'notes.git']), false);
});
test('a path is said to be long from 200 bytes, as the .zip has it', async () => {
    const said = async (length) => {
        const path = `${'d'.repeat(100)}/${'e'.repeat(length - 'site/'.length - 101)}`;
        const entries = [{ path, bytes: bytesOf(1) }];
        const folder = folderWire(entries);
        const { files } = library(keepingFolder(folder, entries));
        await settle();
        return (await files.download(folder.id)).message;
    };
    assert.doesNotMatch(await said(199), /long/u);
    assert.match(await said(200), /1 path in it is long/u);
});
test('a folder\'s files keep when the folder was kept; an item asked twice is saved once', async () => {
    const entries = [{ path: 'a/b.txt', bytes: bytesOf(2) }];
    const folder = folderWire(entries, { kept_at: '2026-11-18T09:30:00Z' });
    const a = bytesOf(3);
    const { files, saved } = library(keepingFolder(folder, entries, [{ file: wire(a), bytes: a }]));
    await settle();
    assert.equal((await files.downloadMany([folder.id, 'f1', folder.id, 'f1'], 'x.zip')).ok, true);
    const zip = saved[0][0];
    const { dosTime } = await import('../../dist/app/adapters/live/zip.js');
    const [time, date] = dosTime('2026-11-18T09:30:00Z');
    const local = zip.indexOf(Buffer.from('site/a/b.txt')) - 30;
    assert.deepEqual([zip.readUInt16LE(local + 10), zip.readUInt16LE(local + 12)], [time, date]);
    assert.deepEqual([...unzipped(zip).keys()], ['site/', 'site/a/', 'site/a/b.txt', 'report.pdf']);
});
test('a folder removed here reads as removed in the chat at once, before the list is read again', async () => {
    const entries = [{ path: 'a.txt', bytes: bytesOf(1) }];
    const folder = folderWire(entries);
    // The list read after still holds it, as a list read before the removal landed would.
    const { files } = library((action) => {
        if (action.action === 'remove_library_file')
            return { result: 'library_file_removed', id: action.id };
        if (action.action === 'list_library')
            return { result: 'library', files: [], folders: [folder], removed: [], standing: STANDING };
        return undefined;
    });
    await settle();
    files.observeFolder({ kind: 'folder_offered', chat_id: 'local', message_id: 'fold-1', folder });
    await settle();
    assert.equal(files.offeredFolder('fold-1').removed, false);
    assert.equal((await files.remove(folder.id)).ok, true);
    assert.equal(files.offeredFolder('fold-1').removed, true);
});
