// The Machine view's connection to the broker's operator socket, through
// `hatchling-host`: a hello with the operator token, then every operator query,
// asked again on each tick while the view is open. What comes back is metadata
// about the machine (what runs, who is connected, where cells asked to go),
// never what anyone said. A frame that isn't one the view can read ends the
// connection loudly rather than being shown half-understood.
import type { TransportHandlers, UiTransport } from '../live/client.js';
import type { CellLimits, EgressDecision, OperatorCell, OperatorSession } from '../live/protocol/generated.js';
import { UI_SCHEMA_VERSION } from '../live/protocol/generated.js';
/** The broker itself, as the `broker` query answers. */
export interface Broker {
    readonly version: string;
    readonly startedAt: string;
    readonly uptimeSecs: number;
}
/** Where the connection stands. */
export type Connection = 'connecting' | 'live' | 'ended';
/** Everything the view shows, as last answered. */
export interface Machine {
    readonly connection: Connection;
    /** Why the connection ended, in a sentence; empty while it lasts. */
    readonly detail: string;
    readonly broker: Broker | null;
    readonly cells: readonly OperatorCell[];
    readonly sessions: readonly OperatorSession[];
    readonly egress: readonly EgressDecision[];
}
/** Starts `tick` every `ms` milliseconds; the returned function stops it. */
export type Every = (tick: () => void, ms: number) => () => void;
/** How often the view asks again. */
export const REFRESH_MS = 2000;
export const QUERIES = ['broker', 'cells', 'sessions', 'egress'] as const;
const UNREADABLE = 'Hatchling sent something the Machine view cannot read, so it disconnected. Reload the page, or restart the broker, so both are the same version.';
const REFUSED = 'Hatchling closed the connection: the address may be old (each broker start makes a new one), or the broker has no operator socket.';
const EMPTY: Machine = { connection: 'connecting', detail: '', broker: null, cells: [], sessions: [], egress: [] };
const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === 'object' && value !== null && !Array.isArray(value);
const isString = (value: unknown): value is string => typeof value === 'string';
const isCount = (value: unknown): value is number => typeof value === 'number' && Number.isInteger(value) && value >= 0;
const isLimit = (value: unknown): boolean => value === null || value === undefined || isCount(value);
const LIMITS = ['processes', 'address_space', 'file_size', 'open_files', 'memory', 'cpu_millis'] as const;
const KINDS = ['agent', 'run', 'subagent', 'skill', 'harness'];
const SOCKETS = ['api', 'ui'];
const VERDICTS = ['allowed', 'no_grant', 'not_allowed', 'bad_surrogate', 'policy', 'failed'];
function isLimits(value: unknown): value is CellLimits {
    return isRecord(value) && LIMITS.every(name => isLimit(value[name]));
}
function isCell(value: unknown): value is OperatorCell {
    return isRecord(value) && isString(value['cell_id']) && KINDS.includes(String(value['kind'])) && isString(value['started_at']) && isLimits(value['limits']);
}
function isSession(value: unknown): value is OperatorSession {
    return isRecord(value) && isString(value['connection']) && SOCKETS.includes(String(value['socket'])) && isString(value['opened_at']);
}
function isDecision(value: unknown): value is EgressDecision {
    return isRecord(value) && isString(value['at']) && isString(value['cell_id']) && isString(value['destination']) && VERDICTS.includes(String(value['verdict']));
}
const listOf = <T>(value: unknown, item: (entry: unknown) => entry is T): readonly T[] | null => Array.isArray(value) && value.every(item) ? value : null;
/** `machine` with one answer applied, or `null` if it isn't one the view can read. */
export function apply(machine: Machine, answer: unknown): Machine | null {
    if (!isRecord(answer))
        return null;
    switch (answer['answer']) {
        case 'broker': {
            const { version, started_at: startedAt, uptime_secs: uptimeSecs } = answer;
            return isString(version) && isString(startedAt) && isCount(uptimeSecs) ? { ...machine, broker: { version, startedAt, uptimeSecs } } : null;
        }
        case 'cells': {
            const cells = listOf(answer['cells'], isCell);
            return cells === null ? null : { ...machine, cells };
        }
        case 'sessions': {
            const sessions = listOf(answer['sessions'], isSession);
            return sessions === null ? null : { ...machine, sessions };
        }
        case 'egress': {
            const egress = listOf(answer['decisions'], isDecision);
            return egress === null ? null : { ...machine, egress };
        }
        default:
            return null;
    }
}
/** The browser's interval timer, behind the shape the client uses. */
export const browserEvery: Every = (tick, ms) => {
    const timer = setInterval(tick, ms);
    return () => { clearInterval(timer); };
};
export class OperatorClient {
    readonly #transport: UiTransport;
    readonly #token: string;
    readonly #changed: (machine: Machine) => void;
    readonly #every: Every;
    #machine: Machine = EMPTY;
    #welcomed = false;
    #stop: (() => void) | null = null;
    #asked = 0;
    /** `changed` hears every new state; `every` paces the refresh. */
    constructor(transport: UiTransport, token: string, changed: (machine: Machine) => void, every: Every = browserEvery) {
        this.#transport = transport;
        this.#token = token;
        this.#changed = changed;
        this.#every = every;
    }
    /** What the view shows now. */
    get machine(): Machine {
        return this.#machine;
    }
    /** Connects, says hello, and starts asking. */
    start(): void {
        const handlers: TransportHandlers = {
            opened: () => {
                this.#transport.send(JSON.stringify({ type: 'hello', schema_version: UI_SCHEMA_VERSION, client: 'hatchling-machine', token: this.#token }));
            },
            received: text => { this.#received(text); },
            closed: detail => { this.#end(this.#welcomed ? detail : REFUSED); },
        };
        this.#transport.connect(handlers);
    }
    /** Stops asking and closes the connection. */
    stop(): void {
        this.#transport.close();
    }
    #received(text: string): void {
        let frame: unknown;
        try {
            frame = JSON.parse(text);
        }
        catch {
            frame = null;
        }
        if (!isRecord(frame)) {
            this.#fail(UNREADABLE);
            return;
        }
        if (!this.#welcomed) {
            this.#handshake(frame);
            return;
        }
        const next = frame['type'] === 'answer' ? apply(this.#machine, frame['answer']) : null;
        if (next === null) {
            this.#fail(UNREADABLE);
            return;
        }
        this.#set(next);
    }
    #handshake(frame: Record<string, unknown>): void {
        if (frame['type'] === 'incompatible') {
            this.#fail(isString(frame['message']) ? frame['message'] : UNREADABLE);
            return;
        }
        if (frame['type'] !== 'welcome') {
            this.#fail(UNREADABLE);
            return;
        }
        this.#welcomed = true;
        this.#set({ ...this.#machine, connection: 'live' });
        this.#ask();
        this.#stop = this.#every(() => { this.#ask(); }, REFRESH_MS);
    }
    #ask(): void {
        for (const query of QUERIES)
            this.#transport.send(JSON.stringify({ type: 'query', request_id: `${query}-${++this.#asked}`, query: { query } }));
    }
    #fail(detail: string): void {
        this.#end(detail);
        this.#transport.close();
    }
    #end(detail: string): void {
        this.#stop?.();
        this.#stop = null;
        if (this.#machine.connection !== 'ended')
            this.#set({ ...this.#machine, connection: 'ended', detail });
    }
    #set(machine: Machine): void {
        this.#machine = machine;
        this.#changed(machine);
    }
}
