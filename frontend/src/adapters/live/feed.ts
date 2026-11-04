// The owner's Feed page, as the broker keeps it. The feed prompt is a schedule
// that posts on the page every day at the owner's time; "write one now" is a
// one-off of the same, a few seconds out. Its runs may read what this connection
// may read (their goals, what they approved it to remember, the model) and post
// on the page, nothing else: not the chat, and no writes. The posts are the
// broker's record of what those runs wrote, read when the connection opens and
// whenever a run ends.
import type { CommandResult, FeedPost } from '../../protocol/models.js';
import type { FeedPort, FeedSetup, FeedState } from '../../session/feed.js';
import type { LiveClient } from './client.js';
import type { FeedPost as KeptPost, ScheduleGrant, ScheduleNotice, ScheduleOp, ScheduleSummary } from './protocol/generated.js';
import { browserTimeZone, describeWhen } from './schedules.js';
/** The page as a schedule's delivery target: the broker's `FEED_PAGE`. */
export const FEED_PAGE = 'feed';
/** How far out a post asked for now is scheduled, in seconds. */
const SOON_SECONDS = 15;
const REASON = 'Hatchling wrote this from your feed prompt, with what it could read about you: your open goals, and what you approved it to remember. It wasn’t sent anywhere.';
const REFUSED = 'This connection may not read the Feed page.';
const UNEXPECTED = 'Hatchling answered with something else.';
const NO_PROMPT = 'Write your feed prompt first: it says what each post is about.';
const NO_TIME = 'Pick a time.';
const WRITING = 'Writing your post now. It appears here when it’s done, in a minute or two.';
const STOPPED = 'Stopped. Your posts stay here.';
const NOTHING_TO_STOP = 'There’s no daily post to stop.';
/** The schedules the page's are among: the live schedule list. */
export interface FeedSchedules {
    summaries(): readonly ScheduleSummary[];
    /** Make `op`'s schedule, and read the list again. */
    make(op: ScheduleOp): Promise<CommandResult>;
    cancel(id: string): Promise<CommandResult>;
}
/** A kept post as the page shows it: its first line the title, the rest the post. */
export function postOf(post: KeptPost): FeedPost {
    const text = post.text.trim();
    const newline = text.indexOf('\n');
    const first = (newline < 0 ? text : text.slice(0, newline)).replace(/^#+\s*/, '').replace(/^\*\*(.+)\*\*$/, '$1').replace(/^title:\s*/i, '').trim();
    const rest = newline < 0 ? '' : text.slice(newline + 1).trim();
    const written = new Date(post.written_at);
    return {
        id: post.post_id,
        title: rest ? first : 'Your post',
        body: rest || first,
        kicker: 'Your feed',
        category: 'Written for you',
        icon: 'feed',
        reason: REASON,
        date: Number.isNaN(written.getTime()) ? 'Undated' : written.toLocaleDateString(undefined, { weekday: 'long', month: 'long', day: 'numeric' }),
        reacted: false,
    };
}
/**
 * The grants a post's run gets, from what this connection `held`: reaching the
 * model, reading the owner's goals and recalling their memory, and posting on
 * the page. Never writing, and never the chat.
 */
export function feedGrants(held: readonly ScheduleGrant[]): ScheduleGrant[] {
    const reads = held.filter(grant => grant.grant === 'egress'
        || (grant.grant === 'goals' && grant.action === 'query-goals')
        || (grant.grant === 'memory' && grant.action === 'recall'));
    return [...reads, { grant: 'channel-send', channel: FEED_PAGE }];
}
/** `at` on the clock of time zone `tz`, as the broker reads a one-off's time: `YYYY-MM-DDTHH:MM:SS`. */
export function localDateTime(at: Date, tz: string): string {
    const parts = new Intl.DateTimeFormat('en-CA', { timeZone: tz, year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' }).formatToParts(at);
    const part = Object.fromEntries(parts.map(one => [one.type, one.value]));
    return `${part['year']}-${part['month']}-${part['day']}T${part['hour']}:${part['minute']}:${part['second']}`;
}
/** The page's daily schedules: the newest is the one that stands. */
function dailies(summaries: readonly ScheduleSummary[]): ScheduleSummary[] {
    return summaries.filter(summary => summary.channel === FEED_PAGE && summary.when.every === 'cron');
}
/** A daily schedule as the page shows it. */
export function setupOf(summary: ScheduleSummary): FeedSetup {
    const daily = summary.when.every === 'cron' ? /^(\d{1,2}) (\d{1,2}) \* \* \*$/.exec(summary.when.expr) : null;
    return {
        prompt: summary.task ?? '',
        time: daily ? `${String(daily[2]).padStart(2, '0')}:${String(daily[1]).padStart(2, '0')}` : '',
        when: describeWhen(summary.when),
    };
}
export class LiveFeed implements FeedPort {
    readonly #client: LiveClient;
    readonly #schedules: FeedSchedules;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    readonly #zone: () => string;
    readonly #now: () => Date;
    #posts: readonly FeedPost[] = [];
    #failed: string | null = null;
    #read = false;
    #ready = false;
    /** `zone` and `now` are the browser's, unless a test says. */
    constructor(client: LiveClient, schedules: FeedSchedules, changed: () => void, nextId: () => string, zone: () => string = browserTimeZone, now: () => Date = () => new Date()) {
        this.#client = client;
        this.#schedules = schedules;
        this.#changed = changed;
        this.#nextId = nextId;
        this.#zone = zone;
        this.#now = now;
        client.subscribe(() => this.#watch());
    }
    /** The posts, the newest first. */
    posts(): readonly FeedPost[] {
        return this.#posts;
    }
    current(): FeedState {
        if (this.#failed !== null)
            return { kind: 'failed', detail: this.#failed };
        if (!this.#read)
            return { kind: 'unread' };
        const summaries = this.#schedules.summaries();
        const daily = dailies(summaries).at(-1);
        return {
            kind: 'read',
            setup: daily === undefined ? null : setupOf(daily),
            writing: summaries.some(summary => summary.channel === FEED_PAGE && summary.when.every === 'once'),
        };
    }
    async refresh(): Promise<void> {
        const reply = await this.#client.request(this.#nextId(), { action: 'read_feed' });
        if (reply.kind !== 'answered')
            this.#failed = reply.detail;
        else if (reply.result.result === 'feed') {
            this.#posts = reply.result.posts.map(postOf);
            this.#failed = null;
            this.#read = true;
        }
        else
            this.#failed = reply.result.result === 'refused' ? REFUSED : UNEXPECTED;
        this.#changed();
    }
    /** A run ended: the page may have a new post. */
    observe(notice: ScheduleNotice): void {
        if (notice.outcome === 'completed' || notice.outcome === 'failed')
            void this.refresh();
    }
    async save(prompt: string, time: string): Promise<CommandResult> {
        const task = prompt.trim();
        if (!task)
            return { ok: false, message: NO_PROMPT };
        const at = /^([01]\d|2[0-3]):([0-5]\d)$/.exec(time);
        if (at === null)
            return { ok: false, message: NO_TIME };
        const earlier = dailies(this.#schedules.summaries());
        const made = await this.#schedules.make({ op: 'create', when: { every: 'cron', expr: `${Number(at[2])} ${Number(at[1])} * * *`, tz: this.#zone() }, task, grants: feedGrants(this.#client.grants()), channel: FEED_PAGE });
        if (!made.ok)
            return made;
        // The new one stands before the old ones go: a cancel that fails
        // leaves two, never none.
        for (const old of earlier)
            await this.#schedules.cancel(old.id);
        return { ok: true, message: `Saved. Hatchling writes your post every day at ${time}.` };
    }
    async writeNow(): Promise<CommandResult> {
        const daily = dailies(this.#schedules.summaries()).at(-1);
        if (daily === undefined || !daily.task)
            return { ok: false, message: NO_PROMPT };
        const tz = daily.when.tz;
        const at = new Date(this.#now().getTime() + SOON_SECONDS * 1000);
        const made = await this.#schedules.make({ op: 'create', when: { every: 'once', at: localDateTime(at, tz), tz }, task: daily.task, grants: [...daily.grants], channel: FEED_PAGE });
        return made.ok ? { ok: true, message: WRITING } : made;
    }
    async stop(): Promise<CommandResult> {
        const daily = dailies(this.#schedules.summaries());
        if (daily.length === 0)
            return { ok: false, message: NOTHING_TO_STOP };
        for (const one of daily) {
            const cancelled = await this.#schedules.cancel(one.id);
            if (!cancelled.ok)
                return cancelled;
        }
        return { ok: true, message: STOPPED };
    }
    /** Read it each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh();
        this.#ready = ready;
    }
}
