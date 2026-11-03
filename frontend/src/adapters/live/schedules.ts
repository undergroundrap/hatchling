// The owner's scheduled work, as the broker keeps it: listed when the
// connection opens and after each change, created from the schedule form, and
// cancelled from its card. The list is the broker's, never this app's guess.
//
// The form writes its timing in one fixed form (see `whenSpec`), in the
// browser's own time zone, and a schedule is given the grants this connection
// holds: the ones a chat message's run gets, which the welcome named.
import type { CommandResult, ScheduledJob } from '../../protocol/models.js';
import type { LiveClient } from './client.js';
import type { ScheduleGrant, ScheduleOp, ScheduleRefusal, ScheduleResult, ScheduleSpec, ScheduleSummary } from './protocol/generated.js';
const CREATED = 'Scheduled. Its results arrive in this chat.';
const CANCELLED = 'Removed. It won’t run again.';
const GONE = 'This schedule was already gone.';
const OFF = 'Hatchling has no scheduler: the operator has not turned scheduled work on.';
const FAILED = "Hatchling couldn't change its schedules, so nothing changed. `sudo hatchling-broker doctor` checks the install.";
const UNCONFIRMED = 'Hatchling answered with something else, so the change is not confirmed. Check the list again.';
const HIDDEN_TASK = 'Scheduled work (its task isn’t shown: you didn’t write it here)';
const DAYS = ['sun', 'mon', 'tue', 'wed', 'thu', 'fri', 'sat'] as const;
const DAY_NAMES = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'] as const;
/** Why the broker refused a schedule, in words. */
const REFUSED: Readonly<Record<ScheduleRefusal, string>> = {
    'exceeds-grants': 'This connection may not give a schedule those permissions.',
    'missing-timezone': 'The schedule needs a time zone.',
    'unknown-timezone': 'Hatchling doesn’t know this browser’s time zone.',
    'invalid-spec': 'That timing never happens. Check the time and repeat.',
    'not-in-future': 'That time has passed, or doesn’t exist here (a daylight-saving change). Pick a later one.',
    'channel-not-granted': 'This connection may not post in the chat, so it can’t schedule work.',
    'bad-task': 'Say what should happen, in fewer words.',
    'too-many-grants': 'This connection holds more permissions than one schedule may carry.',
    full: 'Hatchling keeps as many schedules as it can. Remove one first.',
    'unknown-source': 'Hatchling can’t watch that source.',
    'max-followup-depth': 'This schedule can’t start another.',
    'followup-taken': 'This schedule can’t start another.',
    'bad-delay': 'That delay is out of range.',
};
/** The browser's IANA time zone. */
export function browserTimeZone(): string {
    return Intl.DateTimeFormat().resolvedOptions().timeZone;
}
/**
 * The schedule form's timing, `text`, as the broker's spec in time zone `tz`, or a
 * sentence saying what's wrong. The form writes one of:
 * `once 2026-11-04 08:00`, `every 5 minutes`, `daily 08:00`, `weekdays 08:00`,
 * `weekly mon 08:00`, or `cron <five fields>`.
 */
export function whenSpec(text: string, tz: string): ScheduleSpec | string {
    const words = text.trim().split(/\s+/);
    const clock = (value: string | undefined): [
        number,
        number
    ] | undefined => {
        const match = /^([01]\d|2[0-3]):([0-5]\d)$/.exec(value ?? '');
        return match ? [Number(match[1]), Number(match[2])] : undefined;
    };
    const cron = (expr: string): ScheduleSpec => ({ every: 'cron', expr, tz });
    const [kind, first, second] = words;
    switch (kind) {
        case 'once': {
            const at = clock(second);
            if (words.length !== 3 || !/^\d{4}-\d{2}-\d{2}$/.test(first ?? '') || at === undefined)
                return 'Pick a date and a time.';
            return { every: 'once', at: `${first}T${second}`, tz };
        }
        case 'every': {
            const minutes = Number(first);
            if (words.length !== 3 || second !== 'minutes' || !Number.isInteger(minutes) || minutes < 1 || minutes > 59)
                return 'Repeat every 1 to 59 minutes.';
            return cron(minutes === 1 ? '* * * * *' : `*/${minutes} * * * *`);
        }
        case 'daily':
        case 'weekdays': {
            const at = clock(first);
            if (words.length !== 2 || at === undefined)
                return 'Pick a time.';
            return cron(`${at[1]} ${at[0]} * * ${kind === 'daily' ? '*' : '1-5'}`);
        }
        case 'weekly': {
            const day = DAYS.findIndex(name => name === first);
            const at = clock(second);
            if (words.length !== 3 || day < 0 || at === undefined)
                return 'Pick a day and a time.';
            return cron(`${at[1]} ${at[0]} * * ${day}`);
        }
        case 'cron':
            if (words.length !== 6)
                return 'A cron expression has five fields: minute, hour, day of month, month and day of week.';
            return cron(words.slice(1).join(' '));
        default:
            return 'Pick when it should happen.';
    }
}
/** `spec` in words: the form's own timings read back, any other cron as itself. */
export function describeWhen(spec: ScheduleSpec): string {
    if (spec.every === 'once')
        return `Once, ${spec.at.replace('T', ' at ')} (${spec.tz})`;
    const fields = spec.expr.split(' ');
    const at = (index: number): string => fields[index] ?? '';
    const [minute, hour, dom, month, dow] = [at(0), at(1), at(2), at(3), at(4)];
    const time = /^\d+$/.test(minute) && /^\d+$/.test(hour) ? `${hour.padStart(2, '0')}:${minute.padStart(2, '0')}` : null;
    const zone = ` (${spec.tz})`;
    if (fields.length === 5 && hour === '*' && dom === '*' && month === '*' && dow === '*') {
        if (minute === '*')
            return `Every minute${zone}`;
        const step = /^\*\/(\d+)$/.exec(minute);
        if (step)
            return `Every ${step[1]} minutes${zone}`;
    }
    if (fields.length === 5 && time !== null && dom === '*' && month === '*') {
        if (dow === '*')
            return `Every day at ${time}${zone}`;
        if (dow === '1-5')
            return `Weekdays at ${time}${zone}`;
        const day = /^[0-6]$/.test(dow) ? DAY_NAMES[Number(dow)] : undefined;
        if (day !== undefined)
            return `Every ${day} at ${time}${zone}`;
    }
    return `cron ${spec.expr}${zone}`;
}
/** What `grants` let a scheduled run do, in words. */
export function grantsInWords(grants: readonly ScheduleGrant[]): string {
    if (grants.length === 0)
        return 'May do nothing beyond answering';
    const words = grants.map(grant => {
        switch (grant.grant) {
            case 'channel-send': return grant.channel === 'local' ? 'post in this chat' : `post on ${grant.channel}`;
            case 'egress': return `reach ${grant.destination}`;
            case 'vault': return `${grant.action} the secret ${grant.name}`;
            case 'memory': return `${grant.action} ${grant.scope} memory`;
            case 'goals': return `${grant.action.replace(/-/g, ' ')}`;
        }
    });
    return `May ${words.join(', ')}`;
}
/** A broker timestamp in Unix seconds, on the person's clock. */
function localTime(seconds: number): string {
    if (!Number.isFinite(seconds) || seconds <= 0)
        return 'Not scheduled';
    return new Date(seconds * 1000).toLocaleString(undefined, { weekday: 'short', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
}
/** A listed schedule as the app's card shows it. */
export function jobOf(summary: ScheduleSummary): ScheduledJob {
    return {
        id: summary.id,
        title: summary.task ?? HIDDEN_TASK,
        kind: 'time',
        schedule: describeWhen(summary.when),
        recurring: summary.when.every === 'cron',
        limit: grantsInWords(summary.grants),
        source: '',
        event: '',
        goalId: null,
        health: summary.held ? 'errored' : 'ok',
        lastRun: summary.held ? 'A run was cut short when Hatchling stopped: it waits for you' : '—',
        nextRun: summary.held ? 'Held' : localTime(summary.next_due),
    };
}
export class LiveSchedules {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    readonly #zone: () => string;
    #jobs: readonly ScheduledJob[] = [];
    #ready = false;
    /** `zone` is the time zone the form's times are in: the browser's, unless a test says. */
    constructor(client: LiveClient, changed: () => void, nextId: () => string, zone: () => string = browserTimeZone) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        this.#zone = zone;
        client.subscribe(() => this.#watch());
    }
    /** The schedules this connection may see, in the broker's order. */
    jobs(): readonly ScheduledJob[] {
        return this.#jobs;
    }
    /** Ask the broker for every page of the list. */
    async refresh(): Promise<void> {
        const jobs: ScheduledJob[] = [];
        let after: string | null = null;
        for (;;) {
            const outcome = await this.#ask(after === null ? { op: 'list' } : { op: 'list', after });
            if (outcome?.result !== 'listed')
                return;
            jobs.push(...outcome.schedules.map(jobOf));
            if (typeof outcome.next !== 'string')
                break;
            after = outcome.next;
        }
        this.#jobs = jobs;
        this.#changed();
    }
    /** Schedule `task` at the form's `when`, with this connection's grants, delivered to this chat. */
    async create(task: string, when: string): Promise<CommandResult> {
        const spec = whenSpec(when, this.#zone());
        if (typeof spec === 'string')
            return { ok: false, message: spec };
        const op: ScheduleOp = { op: 'create', when: spec, task, grants: [...this.#client.grants()], channel: 'local' };
        const outcome = await this.#change(op);
        if (typeof outcome === 'string')
            return { ok: false, message: outcome };
        if (outcome.result !== 'created')
            return { ok: false, message: this.#refusal(outcome) };
        await this.refresh();
        return { ok: true, message: CREATED, id: outcome.id };
    }
    /** Cancel the schedule `id`: it won't run again. */
    async cancel(id: string): Promise<CommandResult> {
        const outcome = await this.#change({ op: 'cancel', id });
        if (typeof outcome === 'string')
            return { ok: false, message: outcome };
        switch (outcome.result) {
            case 'cancelled':
                await this.refresh();
                return { ok: true, message: CANCELLED };
            case 'not-found':
                await this.refresh();
                return { ok: false, message: GONE };
            default:
                return { ok: false, message: this.#refusal(outcome) };
        }
    }
    #refusal(outcome: ScheduleResult): string {
        switch (outcome.result) {
            case 'refused': return REFUSED[outcome.reason];
            case 'denied': return OFF;
            case 'schedule-error': return FAILED;
            default: return UNCONFIRMED;
        }
    }
    /** Load the list each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh();
        this.#ready = ready;
    }
    /** The broker's answer to `op`, or a sentence when there was none. */
    async #change(op: ScheduleOp): Promise<ScheduleResult | string> {
        const reply = await this.#client.request(this.#nextId(), { action: 'manage_schedule', op });
        if (reply.kind !== 'answered')
            return reply.detail;
        if (reply.result.result === 'refused')
            return 'This connection may not change schedules.';
        return reply.result.result === 'schedule' ? reply.result.outcome : UNCONFIRMED;
    }
    async #ask(op: ScheduleOp): Promise<ScheduleResult | undefined> {
        const outcome = await this.#change(op);
        return typeof outcome === 'string' ? undefined : outcome;
    }
}
