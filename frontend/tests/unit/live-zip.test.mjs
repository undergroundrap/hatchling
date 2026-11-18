// The .zip of the agent's files the app makes (#500, WO-043): stored, each
// with its CRC-32, under the path it is given, each folder once before its
// first file, as MS-DOS writes them, read back by a reader of the format's
// own records; and the bounds of a .zip without Zip64.
import test from 'node:test';
import assert from 'node:assert/strict';
import { crc32 as nodeCrc32 } from 'node:zlib';
import { crc32, distinct, dosTime, zipStored } from '../../dist/app/adapters/live/zip.js';
/** The files in `zip`, read by its central directory, each checked against its local header. */
function read(parts) {
    const zip = Buffer.concat(parts.map(part => Buffer.from(part)));
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
        assert.deepEqual([view.getUint16(at + 4, true), view.getUint16(at + 6, true)], [20, 20], 'versions: made by MS-DOS, so no mode');
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
        assert.deepEqual([view.getUint16(at + 30, true), view.getUint16(at + 32, true), view.getUint16(at + 34, true), view.getUint16(at + 36, true)], [0, 0, 0, 0], 'no extra field, comment or other disk');
        assert.equal(view.getUint16(local + 28, true), 0, 'no extra field');
        files.push({ name, bytes: Buffer.from(bytes).toString(), time: view.getUint16(at + 12, true), date: view.getUint16(at + 14, true), attributes: view.getUint32(at + 38, true) });
        at += 46 + nameLength;
    }
    return files;
}
const encode = (text) => new TextEncoder().encode(text);
test('a .zip holds each file whole, stored, by its path, with its CRC-32 and when it was kept; each folder once, before its first file', () => {
    const zip = zipStored([
        { name: 'report.pdf', bytes: encode('the report'), at: '2026-11-17T20:27:18Z' },
        { name: 'site/a/b.txt', bytes: encode('b'), at: '2026-11-18T00:00:00Z' },
        { name: 'site/a/c.txt', bytes: encode('c'), at: '2026-11-18T00:00:00Z' },
        { name: 'site/d/e/f.txt', bytes: encode('f'), at: '2026-11-18T00:00:00Z' },
        { name: 'empty.txt', bytes: new Uint8Array(0), at: 'not a time' },
    ]);
    const files = read(zip);
    assert.deepEqual(files.map(file => [file.name, file.bytes, file.attributes]), [
        ['report.pdf', 'the report', 0],
        ['site/', '', 0x10],
        ['site/a/', '', 0x10],
        ['site/a/b.txt', 'b', 0],
        ['site/a/c.txt', 'c', 0],
        ['site/d/', '', 0x10],
        ['site/d/e/', '', 0x10],
        ['site/d/e/f.txt', 'f', 0],
        ['empty.txt', '', 0],
    ]);
    assert.deepEqual([files[0].time, files[0].date], dosTime('2026-11-17T20:27:18Z'));
    assert.deepEqual([files.at(-1).time, files.at(-1).date], [0, 33]);
    // A folder takes the time of the first file in it.
    assert.deepEqual([files[1].time, files[1].date], dosTime('2026-11-18T00:00:00Z'));
    assert.equal(read(zipStored([])).length, 0);
    // Its parts are its local records and bytes in turn, then its directory, never put together.
    assert.equal(zip.length, 9 * 2 + 9 + 1);
});
test('a path given twice, whatever its case, or a file where a folder is, is never written', () => {
    const one = (name) => ({ name, bytes: encode('x'), at: '' });
    assert.throws(() => zipStored([one('a.txt'), one('A.TXT')]), /a path given twice/u);
    assert.throws(() => zipStored([one('d/x'), one('D')]), /a path given twice/u);
    assert.throws(() => zipStored([one('d'), one('D/x')]), /a file where a folder is/u);
    assert.doesNotThrow(() => zipStored([one('d/x'), one('d/y'), one('D/z')]));
});
test('a .zip holds no more than its records can say without Zip64, never the values that mean "see Zip64"', () => {
    const empty = (name) => ({ name, bytes: new Uint8Array(0), at: '' });
    assert.throws(() => zipStored(Array.from({ length: 65535 }, (_, at) => empty(`f${at}`))), /too many entries/u);
    assert.doesNotThrow(() => zipStored(Array.from({ length: 65534 }, (_, at) => empty(`f${at}`))));
    // Bytes that only say their length: none is read past it.
    const sized = (length) => ({ length, [Symbol.iterator]: function* () { } });
    assert.throws(() => zipStored([{ name: 'big', bytes: sized(2 ** 32), at: '' }]), /too large for a \.zip/u);
    // A local record of 33 bytes and a directory of 49 around the bytes: the end of the directory reaching 0xffffffff is too far, one byte short of it is not.
    assert.throws(() => zipStored([{ name: 'big', bytes: sized(2 ** 32 - 1 - 33 - 49), at: '' }]), /too large for a \.zip/u);
    assert.doesNotThrow(() => zipStored([{ name: 'big', bytes: sized(2 ** 32 - 2 - 33 - 49), at: '' }]));
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
