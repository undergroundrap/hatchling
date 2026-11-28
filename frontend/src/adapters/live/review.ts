// What background work may not apply on its own, as the broker's review queue
// keeps it (WO-054): replacing or removing one of the owner's preferences,
// boundaries or commitments, a fact about who they are, something a loop
// couldn't settle, new guidance, and a memory the agent asked for after reading
// from outside, or in a background run (WO-057). Read when the connection opens
// and whenever the owner opens the Memory page; approved or rejected there, one
// at a time. A decision is recorded, not applied: the loop that proposed it
// applies an approved one, after checking that what it would change hasn't
// changed. A memory is the exception: the broker keeps it as it's approved.
import type { CommandResult } from '../../protocol/models.js';
import type { ReviewEntry, ReviewList, ReviewPort } from '../../session/review.js';
import type { LiveClient } from './client.js';
import type { ReviewItem, ReviewKind, ReviewOp, ReviewResult } from './protocol/generated.js';
/** Each kind of proposal, as the page names it. */
const KINDS: Readonly<Record<ReviewKind, string>> = {
    replace: 'Would change a preference, boundary or commitment',
    remove: 'Would remove a preference, boundary or commitment',
    identity: 'About who you are',
    unresolved: 'Something it couldn’t settle on its own',
    guidance: 'New guidance for how the agent works',
    remember: 'Something to remember about you',
};
/** Where a proposal stands, said under it. */
const OUTCOMES: Readonly<Record<ReviewItem['state'], string>> = {
    waiting: '',
    approved: 'You approved it. It’s applied the next time that work runs, once Hatchling has checked nothing it would change has changed.',
    rejected: 'You rejected it. Nothing changed.',
    stale: 'What it would change has changed since, so it won’t be applied.',
};
const APPROVED = 'Approved. It’s applied the next time that work runs.';
/** A memory approved: the broker kept it as it was approved. */
const REMEMBERED_OUTCOME = 'You approved it, so it’s remembered: the agent is given it from now on.';
const REMEMBERED = 'Approved. It’s remembered.';
const REJECTED = 'Rejected. Nothing changes.';
const GONE = 'That is no longer waiting for you.';
const DECIDED = 'That was already decided.';
const REFUSED = 'This connection may not read or decide what waits for you.';
const UNEXPECTED = 'Hatchling answered with something else, so nothing is confirmed. Open Memory again to check.';
const OFF = 'Hatchling keeps nothing for you to review here: scheduled work is off.';
const FAILED = 'Hatchling couldn’t read or change what waits for you. `hatchling-broker doctor`, run as root, checks the install.';
/** A proposal as the page shows it. */
export function entryOf(item: ReviewItem): ReviewEntry {
    const at = new Date(item.proposed_at);
    return {
        id: item.item_id,
        kind: KINDS[item.kind],
        change: item.change,
        why: item.why,
        date: Number.isNaN(at.getTime()) ? '' : `Proposed ${at.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' })}`,
        state: item.state,
        outcome: item.kind === 'remember' && item.state === 'approved' ? REMEMBERED_OUTCOME : OUTCOMES[item.state],
        atOnce: item.kind === 'remember',
    };
}
export class LiveReview implements ReviewPort {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    #list: ReviewList = { kind: 'unread' };
    #ready = false;
    constructor(client: LiveClient, changed: () => void, nextId: () => string) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        client.subscribe(() => this.#watch());
    }
    current(): ReviewList {
        return this.#list;
    }
    opened(): void {
        if (this.#ready)
            void this.refresh();
    }
    async refresh(): Promise<void> {
        const outcome = await this.#ask({ op: 'list' });
        if (typeof outcome === 'string')
            this.#list = { kind: 'failed', detail: outcome };
        else if (outcome.result === 'listed')
            this.#list = { kind: 'read', entries: outcome.items.map(entryOf) };
        // With scheduled work off, nothing runs in the background to propose anything.
        else if (outcome.result === 'off')
            this.#list = { kind: 'read', entries: [] };
        else
            this.#list = { kind: 'failed', detail: this.#says(outcome) };
        this.#changed();
    }
    async decide(id: string, approve: boolean): Promise<CommandResult> {
        const outcome = await this.#ask({ op: 'decide', item_id: id, approve });
        if (typeof outcome === 'string')
            return { ok: false, message: outcome };
        // Whatever the answer, the list may have moved on: read it again.
        if (outcome.result === 'decided' || outcome.result === 'not_waiting' || outcome.result === 'not_found')
            await this.refresh();
        if (outcome.result === 'decided')
            return { ok: true, message: approve ? (outcome.item.kind === 'remember' ? REMEMBERED : APPROVED) : REJECTED };
        return { ok: false, message: this.#says(outcome) };
    }
    #says(outcome: ReviewResult): string {
        switch (outcome.result) {
            case 'not_found': return GONE;
            case 'not_waiting': return DECIDED;
            case 'off': return OFF;
            case 'failed': return FAILED;
            default: return UNEXPECTED;
        }
    }
    /** The broker's answer to `op`, or a sentence when there was none. */
    async #ask(op: ReviewOp): Promise<ReviewResult | string> {
        const reply = await this.#client.request(this.#nextId(), { action: 'manage_review', op });
        if (reply.kind !== 'answered')
            return reply.detail;
        if (reply.result.result === 'refused')
            return REFUSED;
        return reply.result.result === 'review' ? reply.result.outcome : UNEXPECTED;
    }
    /** Read it each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh();
        this.#ready = ready;
    }
}
