// A .zip of what the agent gave the owner, made in the app (#500, WO-043):
// each file stored as it is, uncompressed, with its CRC-32, under the path
// it is given, each folder a path is in once before its first file. Made
// only of bytes already checked against their hashes, under paths already
// checked, so nothing in it can unpack outside the folder it is unpacked
// into; it holds no link, no device and no mode, being written as by MS-DOS.
/** A file to put in a .zip: its path in it, its bytes, and when it was made. */
export interface ZipEntry {
    /** Its path, steps by `/`: checked before it is put here. */
    readonly name: string;
    readonly bytes: Uint8Array;
    /** As RFC 3339; a time that doesn't parse is kept as 1980-01-01. */
    readonly at: string;
}
/** CRC-32's table, each value read by its byte's place: every place has one. */
const CRC_TABLE = new DataView(Uint32Array.from({ length: 256 }, (_, at) => {
    let crc = at;
    for (let bit = 0; bit < 8; bit++)
        crc = crc & 1 ? 0xedb88320 ^ (crc >>> 1) : crc >>> 1;
    return crc >>> 0;
}).buffer);
/** The CRC-32 of `bytes`, as a .zip keeps it. */
export function crc32(bytes: Uint8Array): number {
    let crc = 0xffffffff;
    for (const byte of bytes)
        crc = CRC_TABLE.getUint32(((crc ^ byte) & 0xff) * 4, true) ^ (crc >>> 8);
    return (crc ^ 0xffffffff) >>> 0;
}
/**
 * `at` as an MS-DOS date and time, the only kind a .zip's header holds, in
 * this computer's time zone, as unzip tools read it.
 */
export function dosTime(at: string): readonly [
    number,
    number
] {
    const when = new Date(at);
    if (Number.isNaN(when.getTime()) || when.getFullYear() < 1980 || when.getFullYear() > 2107)
        return [0, (1 << 5) | 1];
    const time = (when.getHours() << 11) | (when.getMinutes() << 5) | Math.floor(when.getSeconds() / 2);
    const date = ((when.getFullYear() - 1980) << 9) | ((when.getMonth() + 1) << 5) | when.getDate();
    return [time, date];
}
/**
 * Names for `names` that are each a file's once: a name seen before gets
 * `-2`, `-3`… before its last extension, so a .zip never holds two files
 * by one name.
 */
export function distinct(names: readonly string[]): string[] {
    return named(names, name => name).map(({ name }) => name);
}
/** Each of `items` with a name made distinct from `name`'s, as `distinct` makes them. */
export function named<T>(items: readonly T[], name: (item: T) => string): {
    readonly item: T;
    readonly name: string;
}[] {
    const taken = new Set<string>();
    return items.map(item => ({ item, name: unique(name(item), taken) }));
}
/** `name`, or the first of `name-2`, `name-3`… not in `taken`, whatever the case; then taken. */
function unique(name: string, taken: Set<string>): string {
    let candidate = name;
    const dot = name.lastIndexOf('.');
    const stem = dot > 0 ? name.slice(0, dot) : name;
    const extension = dot > 0 ? name.slice(dot) : '';
    for (let count = 2; taken.has(candidate.toLowerCase()); count++)
        candidate = `${stem}-${count}${extension}`;
    taken.add(candidate.toLowerCase());
    return candidate;
}
/**
 * The values a .zip's end record keeps to say "see Zip64": its count of
 * entries, and its directory's size and place, are each held under them.
 */
const ZIP_ENTRIES = 0xffff;
const ZIP_BYTES = 0xffffffff;
/** The MS-DOS attribute of a folder. */
const DIRECTORY = 0x10;
/** One record to write: a file, or a folder (its name ending in `/`, no bytes). */
interface ZipRecord {
    readonly name: Uint8Array;
    readonly bytes: Uint8Array;
    readonly crc: number;
    readonly time: number;
    readonly date: number;
    readonly attributes: number;
}
/**
 * `entries` as the records of a .zip: each folder their paths are in, once,
 * before the first file in it, then the file. Throws on a path given twice
 * whatever the case, or a file where a folder is: they are checked before.
 */
function records(entries: readonly ZipEntry[]): ZipRecord[] {
    const encoder = new TextEncoder();
    const folders = new Set<string>();
    const files = new Set<string>();
    const out: ZipRecord[] = [];
    for (const entry of entries) {
        const [time, date] = dosTime(entry.at);
        const steps = entry.name.split('/');
        for (let at = 1; at < steps.length; at++) {
            const folder = `${steps.slice(0, at).join('/')}/`;
            if (files.has(folder.slice(0, -1).toLowerCase()))
                throw new Error('a file where a folder is');
            if (!folders.has(folder.toLowerCase())) {
                folders.add(folder.toLowerCase());
                out.push({ name: encoder.encode(folder), bytes: new Uint8Array(0), crc: 0, time, date, attributes: DIRECTORY });
            }
        }
        const lower = entry.name.toLowerCase();
        if (files.has(lower) || folders.has(`${lower}/`))
            throw new Error('a path given twice');
        files.add(lower);
        out.push({ name: encoder.encode(entry.name), bytes: entry.bytes, crc: crc32(entry.bytes), time, date, attributes: 0 });
    }
    return out;
}
/**
 * `entries` as one .zip, each stored, under the path it is given, in the
 * parts it is saved from as one: never put together here, so a large one is
 * held once. Throws when it would need more than a .zip without Zip64 holds.
 */
export function zipStored(entries: readonly ZipEntry[]): Uint8Array[] {
    const parts: Uint8Array[] = [];
    const centrals: Uint8Array[] = [];
    let offset = 0;
    const written = records(entries);
    if (written.length >= ZIP_ENTRIES)
        throw new Error('too many entries for a .zip');
    for (const record of written) {
        const size = record.bytes.length;
        const local = new Uint8Array(30 + record.name.length);
        const head = new DataView(local.buffer);
        head.setUint32(0, 0x04034b50, true);
        head.setUint16(4, 20, true);
        head.setUint16(6, 0x0800, true);
        head.setUint16(8, 0, true);
        head.setUint16(10, record.time, true);
        head.setUint16(12, record.date, true);
        head.setUint32(14, record.crc, true);
        head.setUint32(18, size, true);
        head.setUint32(22, size, true);
        head.setUint16(26, record.name.length, true);
        local.set(record.name, 30);
        const central = new Uint8Array(46 + record.name.length);
        const view = new DataView(central.buffer);
        view.setUint32(0, 0x02014b50, true);
        view.setUint16(4, 20, true);
        view.setUint16(6, 20, true);
        view.setUint16(8, 0x0800, true);
        view.setUint16(10, 0, true);
        view.setUint16(12, record.time, true);
        view.setUint16(14, record.date, true);
        view.setUint32(16, record.crc, true);
        view.setUint32(20, size, true);
        view.setUint32(24, size, true);
        view.setUint16(28, record.name.length, true);
        view.setUint32(38, record.attributes, true);
        view.setUint32(42, offset, true);
        central.set(record.name, 46);
        parts.push(local, record.bytes);
        centrals.push(central);
        offset += local.length + size;
    }
    const directory = centrals.reduce((size, central) => size + central.length, 0);
    // Each record's place and size is less than where the directory starts.
    if (offset + directory >= ZIP_BYTES)
        throw new Error('too large for a .zip');
    const end = new Uint8Array(22);
    const tail = new DataView(end.buffer);
    tail.setUint32(0, 0x06054b50, true);
    tail.setUint16(8, written.length, true);
    tail.setUint16(10, written.length, true);
    tail.setUint32(12, directory, true);
    tail.setUint32(16, offset, true);
    return [...parts, ...centrals, end];
}
