// The owner's scheduled work, as the broker keeps it: listed when the
// connection opens and after each change, created from the schedule form, and
// cancelled from its card. The list is the broker's, never this app's guess.
//
// The form writes its timing in one fixed form (see `whenSpec`), in the
// browser's own time zone, and a schedule is given the grants this connection
// holds: the ones a chat message's run gets, which the welcome named.
import type { CommandResult, ScheduledJob } from '../../protocol/models.js';
import type { AttentionAction, AttentionItem, AttentionPort } from '../../session/attention.js';
import type { LiveClient } from './client.js';
import type { HeldFire, ParkedSummary, ScheduleGrant, ScheduleOp, ScheduleRefusal, ScheduleResult, ScheduleSpec, ScheduleSummary } from './protocol/generated.js';
const CREATED = 'Scheduled. Its results arrive in this chat.';
const CANCELLED = 'Removed. It won’t run again.';
const GONE = 'This schedule was already gone.';
const OFF = 'Hatchling has no scheduler: the operator has not turned scheduled work on.';
const FAILED = "Hatchling couldn't change its schedules, so nothing changed. `hatchling-broker doctor`, run as root, checks the install.";
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
    // A store's grants read as one phrase, where the first of them stood,
    // said once all of them are known.
    const memory = new Map<string, string[]>();
    let keepsGoals = false;
    const phrases: (string | (() => string))[] = [];
    for (const grant of grants) {
        switch (grant.grant) {
            case 'channel-send':
                phrases.push(grant.channel === 'local' ? 'post in this chat' : grant.channel === 'feed' ? 'post on your Feed page' : `post on ${grant.channel}`);
                break;
            case 'egress':
                phrases.push(`reach ${grant.destination}`);
                break;
            case 'vault':
                phrases.push(`${grant.action} the secret ${grant.name}`);
                break;
            case 'memory': {
                const verb = MEMORY_VERBS[grant.action] ?? grant.action;
                const verbs = memory.get(grant.scope);
                if (verbs !== undefined) {
                    verbs.push(verb);
                    break;
                }
                const said = [verb];
                memory.set(grant.scope, said);
                phrases.push(() => `${inList(said)} ${grant.scope === 'user' ? 'things about you' : `${grant.scope} memory`}`);
                break;
            }
            case 'goals':
                if (!phrases.includes(goalsPhrase))
                    phrases.push(goalsPhrase);
                keepsGoals ||= !grant.action.startsWith('query-');
                break;
        }
    }
    function goalsPhrase(): string {
        return keepsGoals ? 'keep your goals and their tracking' : 'read your goals';
    }
    return `May ${phrases.map(phrase => (typeof phrase === 'string' ? phrase : phrase())).join(', ')}`;
}
/** What each memory action does, as a verb. */
const MEMORY_VERBS: Readonly<Record<string, string>> = { write: 'remember', recall: 'recall', remove: 'forget', 'context-append': 'note', 'context-read': 'read notes of' };
/** `items` as an English list: a, b and c. */
function inList(items: readonly string[]): string {
    return items.length < 2 ? items.join('') : `${items.slice(0, -1).join(', ')} and ${items.at(-1)}`;
}
/** A broker timestamp in Unix seconds, on the person's clock. */
function localTime(seconds: number): string {
    if (!Number.isFinite(seconds) || seconds <= 0)
        return 'Not scheduled';
    return new Date(seconds * 1000).toLocaleString(undefined, { weekday: 'short', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
}
/** Why a held schedule waits for the owner, and what they can do, in words. */
function heldDetail(held: HeldFire): string {
    if (held.stopped === true)
        return held.started
            ? 'Hatchling stopped a run of this: it tried to send a saved secret. It won’t run again until you look. Run it again, or mark it resolved to let its schedule go on.'
            : 'A run that asked for this was stopped: it tried to send a saved secret. This waits for you. Run it now, or mark it resolved to skip it.';
    return held.started
        ? 'A run was cut short when Hatchling stopped: what it did isn’t known. Look, then mark it resolved, or run it again.'
        : 'A run was due when Hatchling stopped, and didn’t start. Run it now, or mark it resolved to skip it.';
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
        lastRun: summary.held ? (summary.held.stopped === true ? 'Hatchling stopped a run that tried to send a saved secret: it waits for you' : 'A run was cut short when Hatchling stopped: it waits for you') : '—',
        nextRun: summary.held ? 'Held' : localTime(summary.next_due),
    };
}
/** What a kept result's reason means, in words; the record's own words otherwise. */
function parkedDetail(parked: ParkedSummary): {
    detail: string;
    result: string | null;
} {
    if (parked.reason === 'the run failed')
        return { detail: 'It didn’t finish. The chat said why when it happened.', result: null };
    if (parked.reason === 'the run did not start')
        return { detail: 'It didn’t start. `hatchling-broker doctor`, run as root, checks the install.', result: null };
    if (parked.reason.startsWith('delivery failed'))
        return { detail: 'Its result couldn’t be delivered when it finished. Here it is.', result: parked.text };
    return { detail: `Kept: ${parked.reason}.`, result: null };
}
export class LiveSchedules implements AttentionPort {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    readonly #zone: () => string;
    #jobs: readonly ScheduledJob[] = [];
    #summaries: readonly ScheduleSummary[] = [];
    #parked: readonly ParkedSummary[] = [];
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
    /** The same, as the broker listed them. */
    summaries(): readonly ScheduleSummary[] {
        return this.#summaries;
    }
    /** Ask the broker for every page of the list, and of what it kept for the owner. */
    async refresh(): Promise<void> {
        const summaries: ScheduleSummary[] = [];
        let after: string | null = null;
        for (;;) {
            const outcome = await this.#ask(after === null ? { op: 'list' } : { op: 'list', after });
            if (outcome?.result !== 'listed')
                return;
            summaries.push(...outcome.schedules);
            if (typeof outcome.next !== 'string')
                break;
            after = outcome.next;
        }
        const parked: ParkedSummary[] = [];
        after = null;
        for (;;) {
            const outcome = await this.#ask(after === null ? { op: 'list-parked' } : { op: 'list-parked', after });
            // A connection that may list schedules but not what was kept
            // still shows its schedules.
            if (outcome?.result !== 'parked')
                break;
            parked.push(...outcome.parked);
            if (typeof outcome.next !== 'string')
                break;
            after = outcome.next;
        }
        this.#summaries = summaries;
        this.#jobs = summaries.map(jobOf);
        this.#parked = parked;
        this.#changed();
    }
    /** Runs held for the owner, cut short by a stop or stopped for a secret, then results kept: each with what the owner can do. */
    items(): readonly AttentionItem[] {
        const title = (scheduleId: string): string => this.#jobs.find(job => job.id === scheduleId)?.title ?? 'Scheduled work';
        const held = this.#summaries.flatMap(summary => summary.held
            ? [{ id: `held:${summary.id}`, title: jobOf(summary).title, detail: heldDetail(summary.held), when: localTime(summary.held.due), result: null, actions: ['retry', 'resolve'] } satisfies AttentionItem]
            : []);
        const kept = this.#parked.map(parked => ({ id: `parked:${parked.id}`, title: title(parked.schedule_id), ...parkedDetail(parked), when: localTime(parked.due), actions: ['dismiss'] } satisfies AttentionItem));
        return [...held, ...kept];
    }
    /** Do `action` about the item `id`: run a held fire again, resolve it, or dismiss a kept result. */
    async act(id: string, action: AttentionAction): Promise<CommandResult> {
        const [kind, target] = [id.slice(0, id.indexOf(':')), id.slice(id.indexOf(':') + 1)];
        const op: ScheduleOp | undefined = kind === 'held' && action === 'retry' ? { op: 'retry', id: target }
            : kind === 'held' && action === 'resolve' ? { op: 'resolve', id: target }
                : kind === 'parked' && action === 'dismiss' ? { op: 'dismiss', id: target }
                    : undefined;
        if (op === undefined)
            return { ok: false, message: 'That can’t be done to this item.' };
        const outcome = await this.#change(op);
        if (typeof outcome === 'string')
            return { ok: false, message: outcome };
        const done: Partial<Record<ScheduleResult['result'], string>> = { retried: 'Running it again now.', resolved: 'Marked resolved. Its schedule goes on as planned.', dismissed: 'Dismissed.' };
        const message = done[outcome.result];
        if (message !== undefined || outcome.result === 'not-found') {
            await this.refresh();
            return message === undefined ? { ok: false, message: 'It was already dealt with.' } : { ok: true, message };
        }
        return { ok: false, message: this.#refusal(outcome) };
    }
    /** Schedule `task` at the form's `when`, with this connection's grants, delivered to this chat. */
    async create(task: string, when: string): Promise<CommandResult> {
        const spec = whenSpec(when, this.#zone());
        if (typeof spec === 'string')
            return { ok: false, message: spec };
        const made = await this.make({ op: 'create', when: spec, task, grants: [...this.#client.grants()], channel: 'local' });
        return made.ok ? { ...made, message: CREATED } : made;
    }
    /** Make the schedule `op` asks for, and read the list again; the message is the caller's to give. */
    async make(op: ScheduleOp): Promise<CommandResult> {
        const outcome = await this.#change(op);
        if (typeof outcome === 'string')
            return { ok: false, message: outcome };
        if (outcome.result !== 'created')
            return { ok: false, message: this.#refusal(outcome) };
        await this.refresh();
        return { ok: true, message: '', id: outcome.id };
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
