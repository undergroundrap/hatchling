// The .zip of the agent's files the app makes (#500): stored, each with its
// CRC-32, under a plain name made distinct, read back by a reader of the
// format's own records.
import test from 'node:test';
import assert from 'node:assert/strict';
import { crc32 as nodeCrc32 } from 'node:zlib';
import { crc32, distinct, dosTime, zipStored } from '../../dist/app/adapters/live/zip.js';
/** The files in `zip`, read by its central directory, each checked against its local header. */
function read(zip) {
    const view = new DataView(zip.buffer, zip.byteOffset, zip.byteLength);
    const end = zip.length - 22;
    assert.equal(view.getUint32(end, true), 0x06054b50, 'no end of central directory where one ends');
    const count = view.getUint16(end + 10, true);
    let at = view.getUint32(end + 16, true);
    const files = [];
    assert.equal(view.getUint16(end + 4, true), 0, 'another disk');
    assert.equal(view.getUint16(end + 6, true), 0, 'another disk');
    assert.equal(view.getUint16(end + 8, true), count, 'entries on this disk');
    assert.equal(view.getUint32(end + 12, true), end - view.getUint32(end + 16, true), 'the directory\'s size');
    for (let entry = 0; entry < count; entry++) {
        assert.equal(view.getUint32(at, true), 0x02014b50);
        assert.deepEqual([view.getUint16(at + 4, true), view.getUint16(at + 6, true)], [20, 20], 'versions');
        assert.equal(view.getUint16(at + 8, true), 0x0800, 'names flagged as UTF-8');
        assert.equal(view.getUint16(at + 10, true), 0, 'compressed');
        assert.equal(view.getUint32(at + 20, true), view.getUint32(at + 24, true), 'stored: compressed size is its size');
        const crc = view.getUint32(at + 16, true);
        const size = view.getUint32(at + 24, true);
        const nameLength = view.getUint16(at + 28, true);
        const local = view.getUint32(at + 42, true);
        const name = new TextDecoder().decode(zip.subarray(at + 46, at + 46 + nameLength));
        assert.equal(view.getUint32(local, true), 0x04034b50);
        assert.equal(view.getUint16(local + 4, true), 20, 'version needed');
        assert.equal(view.getUint16(local + 6, true), 0x0800, 'names flagged as UTF-8');
        assert.equal(view.getUint16(local + 8, true), 0, 'compressed');
        assert.deepEqual([view.getUint16(local + 10, true), view.getUint16(local + 12, true)], [view.getUint16(at + 12, true), view.getUint16(at + 14, true)], 'times differ');
        assert.equal(view.getUint32(local + 14, true), crc);
        assert.equal(view.getUint32(local + 18, true), size, 'compressed size');
        assert.equal(view.getUint32(local + 22, true), size);
        const start = local + 30 + view.getUint16(local + 26, true);
        const bytes = zip.subarray(start, start + size);
        assert.equal(nodeCrc32(bytes), crc, name);
        files.push({ name, bytes: Buffer.from(bytes).toString(), time: view.getUint16(at + 12, true), date: view.getUint16(at + 14, true) });
        at += 46 + nameLength;
    }
    return files;
}
test('a .zip holds each file whole, stored, by its name, with its CRC-32 and when it was kept', () => {
    const encode = (text) => new TextEncoder().encode(text);
    const zip = zipStored([
        { name: 'report.pdf', bytes: encode('the report'), at: '2026-11-17T20:27:18Z' },
        { name: 'empty.txt', bytes: new Uint8Array(0), at: '2026-11-17T00:00:00Z' },
        { name: 'report.pdf', bytes: encode('another report'), at: 'not a time' },
    ]);
    const files = read(zip);
    assert.deepEqual(files.map(file => [file.name, file.bytes]), [['report.pdf', 'the report'], ['empty.txt', ''], ['report-2.pdf', 'another report']]);
    assert.deepEqual([files[0].time, files[0].date], dosTime('2026-11-17T20:27:18Z'));
    assert.deepEqual([files[2].time, files[2].date], [0, 33]);
    // No folder in any name, so nothing unpacks outside its folder.
    for (const file of files)
        assert.doesNotMatch(file.name, /[\\/]/u);
    assert.equal(read(zipStored([])).length, 0);
});
test('the CRC-32 is the one every .zip reader checks', () => {
    assert.equal(crc32(new TextEncoder().encode('123456789')), 0xcbf43926);
    assert.equal(crc32(new Uint8Array(0)), 0);
    const bytes = Uint8Array.from({ length: 70000 }, (_, at) => (at * 7) % 256);
    assert.equal(crc32(bytes), nodeCrc32(bytes));
});
test('a time is kept to the two seconds a .zip can say, in this computer\'s time, and one it can\'t say as 1980-01-01', () => {
    const at = '2026-11-17T20:27:19Z';
    const when = new Date(at);
    const [time, date] = dosTime(at);
    assert.deepEqual([time >> 11, (time >> 5) & 63, (time & 31) * 2], [when.getHours(), when.getMinutes(), 18]);
    assert.deepEqual([(date >> 9) + 1980, (date >> 5) & 15, date & 31], [when.getFullYear(), when.getMonth() + 1, when.getDate()]);
    for (const out of ['', 'soon', '1979-06-30T12:00:00', '2108-06-30T12:00:00'])
        assert.deepEqual(dosTime(out), [0, 33], out);
    assert.deepEqual(dosTime('1980-01-01T00:00:00'), [0, 33]);
    assert.notDeepEqual(dosTime('2107-06-30T12:00:00'), [0, 33]);
});
test('names are made distinct, whatever their case, before the last extension', () => {
    assert.deepEqual(distinct(['a.txt', 'A.TXT', 'a.txt', 'a-2.txt', 'notes', 'notes', 'x.tar.gz', 'x.tar.gz']), ['a.txt', 'A-2.TXT', 'a-3.txt', 'a-2-2.txt', 'notes', 'notes-2', 'x.tar.gz', 'x.tar-2.gz']);
});
