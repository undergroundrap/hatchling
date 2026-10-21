// How the Machine view words what the broker answered. Pure, so it is tested
// on its own; the view only places these strings, always as text.
import type { CellKind, CellLimits, EgressVerdict, SessionSocket } from '../adapters/live/protocol/generated.js';
/** How long the broker has run, coarsely: minutes, hours, then days. */
export function uptime(seconds: number): string {
    if (seconds < 60)
        return 'under a minute';
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60)
        return `${minutes} min`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24)
        return `${hours} h ${String(minutes % 60).padStart(2, '0')} min`;
    return `${Math.floor(hours / 24)} d ${hours % 24} h`;
}
const UNITS = ['bytes', 'KiB', 'MiB', 'GiB', 'TiB'] as const;
/** A size in bytes, in the largest unit it fills, to one decimal place at most. */
export function size(bytes: number): string {
    let value = bytes;
    let unit = 0;
    while (value >= 1024 && unit < UNITS.length - 1) {
        value /= 1024;
        unit++;
    }
    return `${Number(value.toFixed(1))} ${UNITS[unit]}`;
}
/** A CPU allowance in thousandths of a core. */
export function cores(millis: number): string {
    const value = Number((millis / 1000).toFixed(2));
    return `${value} ${value === 1 ? 'core' : 'cores'}`;
}
/** A cell's limits, each in words; the ones it doesn't set are left out. */
export function limits(held: CellLimits): string[] {
    const words: string[] = [];
    const add = (value: number | null | undefined, word: (set: number) => string): void => {
        if (typeof value === 'number')
            words.push(word(value));
    };
    add(held.processes, set => `${set} processes`);
    add(held.memory, set => `${size(set)} memory`);
    add(held.cpu_millis, cores);
    add(held.open_files, set => `${set} open files`);
    add(held.file_size, set => `${size(set)} per file`);
    add(held.address_space, set => `${size(set)} address space`);
    return words;
}
const KINDS: Readonly<Record<CellKind, string>> = {
    agent: 'Agent',
    run: 'Run',
    subagent: 'Subagent',
    skill: 'Skill',
    harness: 'Coding CLI',
    browser: 'Browser',
};
/** What a cell is for, in a word or two. */
export function kind(cell: CellKind): string {
    return KINDS[cell];
}
const SOCKETS: Readonly<Record<SessionSocket, string>> = { api: 'Local API', ui: 'App' };
/** Which socket a connection came in on. */
export function socket(on: SessionSocket): string {
    return SOCKETS[on];
}
/** How a decision reads, and which state it is: allowed, refused, or failed. */
export interface Verdict {
    readonly label: string;
    readonly tone: 'success' | 'danger' | 'attention';
}
const VERDICTS: Readonly<Record<EgressVerdict, Verdict>> = {
    allowed: { label: 'Allowed', tone: 'success' },
    no_grant: { label: 'Refused: no grant', tone: 'danger' },
    not_allowed: { label: 'Refused: not allowed', tone: 'danger' },
    bad_surrogate: { label: 'Refused: bad surrogate', tone: 'danger' },
    policy: { label: 'Refused by policy', tone: 'danger' },
    failed: { label: 'Failed', tone: 'attention' },
};
/** A decision's verdict, worded. */
export function verdict(of: EgressVerdict): Verdict {
    return VERDICTS[of];
}
/** The time of day in an RFC 3339 timestamp, in UTC: `09:00:00 UTC`. */
export function clock(timestamp: string): string {
    const time = /^\d{4}-\d{2}-\d{2}T(\d{2}:\d{2}:\d{2})/.exec(timestamp);
    return time?.[1] === undefined ? timestamp : `${time[1]} UTC`;
}
