// The owner's Ideas page, as the broker keeps it: what the agent suggested,
// every word its own, read when the connection opens, whenever a scheduled run
// ends, whenever any run, a chat's among them, has suggested an idea, and
// whenever the owner opens the page.
// The owner starts one (the page then fills their chat box with it), sets it
// aside, or brings it back. One that waited two weeks, or whose goal was closed,
// is kept with why: those are the broker's findings, shown in the app's words,
// among the ideas set aside, where Restore is. One whose conversation was
// answered stays among those started, and says so: the broker knows an answer
// was shown, not that the idea was carried out. "Suggest ideas" is a one-off run for the page, a few
// seconds out: it may reach the model, read their goals and recall their
// memory, and suggest; nothing else.
import type { CommandResult, IconName, Idea, IdeaDomain } from '../../protocol/models.js';
import type { IdeasPort, IdeasState } from '../../session/ideas.js';
import type { LiveClient } from './client.js';
import type { ActivityEvent, ScheduleNotice, ScheduleSummary, Suggestion, SuggestionArea, SuggestionOp, SuggestionResult } from './protocol/generated.js';
import type { FeedSchedules } from './feed.js';
import { feedGrants, localDateTime } from './feed.js';
import { browserTimeZone } from './schedules.js';
/** The page as a schedule's delivery target: the broker's `IDEAS_PAGE`. */
export const IDEAS_PAGE = 'ideas';
/** How far out ideas asked for now are scheduled, in seconds. */
const SOON_SECONDS = 15;
/** What a run for ideas now is asked, shown in Schedule while it waits. */
const ASK = 'Ideas for me, from my goals and what I’ve asked you to remember.';
const SIGNAL = 'Suggested by Hatchling, from what it could read about you. These are its words: nothing runs unless you start it.';
const REFUSED = 'This connection may not read the Ideas page.';
const UNEXPECTED = 'Hatchling answered with something else.';
const OFF = 'Hatchling keeps no Ideas page here: scheduled work is off.';
const FAILED = 'Hatchling couldn’t read or change the Ideas page. `hatchling-broker doctor`, run as root, checks the install.';
const GONE = 'This idea is gone.';
const STILL_GONE = 'Its goal is still closed, so it stays here.';
/** Each state the broker keeps an idea in, as the page shows it: one the broker retired is among those set aside. */
const STATES: Readonly<Record<Suggestion['state'], Idea['state']>> = { suggested: 'suggested', started: 'activated', answered: 'activated', dismissed: 'dismissed', stale: 'dismissed', source_gone: 'dismissed' };
/** What the broker found of an idea, said under it: why it left For you without the owner setting it aside, or that its conversation was answered. */
const FOUND: Readonly<Partial<Record<Suggestion['state'], string>>> = {
    stale: 'It waited two weeks without being started. Restore it if you still want it.',
    source_gone: 'The goal it came from is closed or gone.',
    answered: 'Hatchling answered in the conversation you started for it.',
};
const SUGGESTING = 'Thinking of ideas for you. They appear here when they’re ready, in a minute or two.';
/** Each area: the page's section, and its picture; in the page's order. */
const AREAS: Readonly<Record<SuggestionArea, {
    readonly domain: IdeaDomain;
    readonly icon: IconName;
}>> = {
    productivity: { domain: 'Productivity', icon: 'clipboard' },
    career: { domain: 'Career', icon: 'briefcase' },
    learning: { domain: 'Learning & hobbies', icon: 'book' },
    health: { domain: 'Health & fitness', icon: 'run' },
    creation: { domain: 'Creation & media', icon: 'palette' },
    devices: { domain: 'Devices', icon: 'laptop' },
    finance: { domain: 'Finance', icon: 'currency' },
    family: { domain: 'Family', icon: 'people' },
    relationships: { domain: 'Relationships', icon: 'heart' },
    shopping: { domain: 'Shopping', icon: 'cart' },
};
const ORDER = Object.keys(AREAS);
/** What the owner may do to an idea, as the broker's op. */
export type IdeaAction = 'start' | 'dismiss' | 'restore';
/** A kept suggestion as the page shows it: the agent's words, in its area's section. */
export function ideaOf(suggestion: Suggestion): Idea {
    const area = AREAS[suggestion.area];
    // Where it came from is the broker's finding, said before the note that the rest is the agent's.
    const from = suggestion.source ? `From your goal “${suggestion.source.title}”. ` : '';
    return {
        id: suggestion.suggestion_id,
        section: area.domain,
        title: suggestion.title,
        summary: suggestion.summary,
        domain: area.domain,
        icon: area.icon,
        rationale: suggestion.why,
        signal: `${from}${SIGNAL}`,
        items: [],
        state: STATES[suggestion.state],
        outcome: FOUND[suggestion.state] ?? '',
        artifactId: null,
    };
}
/** The suggestions in the page's order: by area, the newest first in each. */
export function ordered(suggestions: readonly Suggestion[]): Suggestion[] {
    return [...suggestions].sort((a, b) => ORDER.indexOf(a.area) - ORDER.indexOf(b.area));
}
export class LiveIdeas implements IdeasPort {
    readonly #client: LiveClient;
    readonly #schedules: FeedSchedules;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    readonly #zone: () => string;
    readonly #now: () => Date;
    #ideas: readonly Idea[] = [];
    #failed: string | null = null;
    #read = false;
    #ready = false;
    #reading = false;
    #stale = false;
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
    /** The ideas, in the page's order. */
    ideas(): readonly Idea[] {
        return this.#ideas;
    }
    current(): IdeasState {
        if (this.#failed !== null)
            return { kind: 'failed', detail: this.#failed };
        if (!this.#read)
            return { kind: 'unread' };
        return { kind: 'read', suggesting: this.#schedules.summaries().some((summary: ScheduleSummary) => summary.channel === IDEAS_PAGE && summary.when.every === 'once') };
    }
    async refresh(): Promise<void> {
        const outcome = await this.#ask({ op: 'list' });
        if (typeof outcome === 'string')
            this.#failed = outcome;
        else if (outcome.result === 'listed') {
            this.#ideas = ordered(outcome.suggestions).map(ideaOf);
            this.#failed = null;
            this.#read = true;
        }
        else
            this.#failed = this.#says(outcome);
        this.#changed();
    }
    /** A run ended: the page may have new ideas. */
    observe(notice: ScheduleNotice): void {
        if (notice.outcome === 'completed' || notice.outcome === 'failed')
            void this.refresh();
    }
    /** A run's record: one that suggested an idea put it on the page, a chat's run too. */
    observeRun(event: ActivityEvent): void {
        if (event.event === 'run_summary' && event.tools.some(use => use.tool === 'idea_suggest' && use.done > 0))
            this.#again();
    }
    /** The owner opened the page: read it again when there is a connection to read it over. */
    opened(): void {
        if (this.#ready)
            this.#again();
    }
    /** Start, set aside or bring back the idea `id`. */
    async act(id: string, action: IdeaAction): Promise<CommandResult> {
        const outcome = await this.#ask({ op: action, id });
        if (typeof outcome === 'string')
            return { ok: false, message: outcome };
        if (outcome.result !== 'changed')
            return { ok: false, message: outcome.result === 'not_found' ? GONE : this.#says(outcome) };
        await this.refresh();
        // Brought back while its goal is closed, it stays as it was.
        if (action === 'restore' && outcome.suggestion.state === 'source_gone')
            return { ok: false, message: STILL_GONE };
        const done: Readonly<Record<IdeaAction, string>> = { start: 'It’s in your chat box: read it, change it, and send it when you’re ready.', dismiss: 'Set aside.', restore: 'It’s back in For you.' };
        return { ok: true, message: done[action] };
    }
    async suggestNow(): Promise<CommandResult> {
        const tz = this.#zone();
        const at = new Date(this.#now().getTime() + SOON_SECONDS * 1000);
        const grants = [...feedGrants(this.#client.grants()).filter(grant => grant.grant !== 'channel-send'), { grant: 'channel-send' as const, channel: IDEAS_PAGE }];
        const made = await this.#schedules.make({ op: 'create', when: { every: 'once', at: localDateTime(at, tz), tz }, task: ASK, grants, channel: IDEAS_PAGE });
        if (!made.ok)
            return made;
        this.#changed();
        return { ok: true, message: SUGGESTING };
    }
    /** Read the page again: now, or once more after the read under way, however many asked meanwhile. */
    #again(): void {
        if (this.#reading) {
            this.#stale = true;
            return;
        }
        this.#reading = true;
        void this.refresh().then(() => {
            this.#reading = false;
            if (this.#stale) {
                this.#stale = false;
                this.#again();
            }
        });
    }
    #says(outcome: SuggestionResult): string {
        switch (outcome.result) {
            case 'off': return OFF;
            case 'failed': return FAILED;
            default: return UNEXPECTED;
        }
    }
    /** The broker's answer to `op`, or a sentence when there was none. */
    async #ask(op: SuggestionOp): Promise<SuggestionResult | string> {
        const reply = await this.#client.request(this.#nextId(), { action: 'manage_suggestions', op });
        if (reply.kind !== 'answered')
            return reply.detail;
        if (reply.result.result === 'refused')
            return REFUSED;
        return reply.result.result === 'suggestions' ? reply.result.outcome : UNEXPECTED;
    }
    /** Read it each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh();
        this.#ready = ready;
    }
}
