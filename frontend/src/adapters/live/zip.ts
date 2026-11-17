// A .zip of files the agent gave the owner, made in the app (#500): each
// file stored as it is, uncompressed, with its CRC-32, under a plain name
// with no folder, so nothing in it can unpack outside the folder it is
// unpacked into. Made only of bytes already checked against their hashes.
/** A file to put in a .zip: its name in it, its bytes, and when it was made. */
export interface ZipEntry {
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
    const taken = new Set<string>();
    return names.map(name => unique(name, taken));
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
/** `entries` as one .zip, each stored, under a name made distinct. */
export function zipStored(entries: readonly ZipEntry[]): Uint8Array {
    const encoder = new TextEncoder();
    const taken = new Set<string>();
    const locals: Uint8Array[] = [];
    const centrals: Uint8Array[] = [];
    let offset = 0;
    for (const entry of entries) {
        const name = encoder.encode(unique(entry.name, taken));
        const crc = crc32(entry.bytes);
        const [time, date] = dosTime(entry.at);
        const local = new Uint8Array(30 + name.length);
        const head = new DataView(local.buffer);
        head.setUint32(0, 0x04034b50, true);
        head.setUint16(4, 20, true);
        head.setUint16(6, 0x0800, true);
        head.setUint16(8, 0, true);
        head.setUint16(10, time, true);
        head.setUint16(12, date, true);
        head.setUint32(14, crc, true);
        head.setUint32(18, entry.bytes.length, true);
        head.setUint32(22, entry.bytes.length, true);
        head.setUint16(26, name.length, true);
        local.set(name, 30);
        const central = new Uint8Array(46 + name.length);
        const record = new DataView(central.buffer);
        record.setUint32(0, 0x02014b50, true);
        record.setUint16(4, 20, true);
        record.setUint16(6, 20, true);
        record.setUint16(8, 0x0800, true);
        record.setUint16(10, 0, true);
        record.setUint16(12, time, true);
        record.setUint16(14, date, true);
        record.setUint32(16, crc, true);
        record.setUint32(20, entry.bytes.length, true);
        record.setUint32(24, entry.bytes.length, true);
        record.setUint16(28, name.length, true);
        record.setUint32(42, offset, true);
        central.set(name, 46);
        locals.push(local, entry.bytes);
        centrals.push(central);
        offset += local.length + entry.bytes.length;
    }
    const directory = centrals.reduce((size, central) => size + central.length, 0);
    const end = new Uint8Array(22);
    const tail = new DataView(end.buffer);
    tail.setUint32(0, 0x06054b50, true);
    tail.setUint16(8, entries.length, true);
    tail.setUint16(10, entries.length, true);
    tail.setUint32(12, directory, true);
    tail.setUint32(16, offset, true);
    const parts = [...locals, ...centrals, end];
    const zip = new Uint8Array(parts.reduce((size, part) => size + part.length, 0));
    let at = 0;
    for (const part of parts) {
        zip.set(part, at);
        at += part.length;
    }
    return zip;
}
