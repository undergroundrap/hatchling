// The owner's goals, as the broker keeps them: the same goals the agent reads
// in a chat and a feed. Loaded when the connection opens and after each change,
// set and changed from the Goals page, and closed as reached or set aside. The
// list is the broker's, never this app's guess: until it answers it is empty.
import type { CommandResult, Goal } from '../../protocol/models.js';
import type { GoalsPort } from '../../session/goals.js';
import type { LiveClient } from './client.js';
import type { GoalOp, GoalResult, GoalView } from './protocol/generated.js';
/** What each answer to a change says. */
const SAID: Readonly<Record<Exclude<GoalResult['result'], 'listed' | 'saved'>, string>> = {
    not_found: 'That goal is no longer there.',
    already_closed: 'That goal is closed, so it can’t change. Set a new one instead.',
    rejected: 'Hatchling didn’t accept that: a goal’s title is one line, and the title and detail each have a length they may not pass.',
    limit_reached: 'You have as many goals as Hatchling keeps. Close one to set another.',
    off: 'Hatchling keeps no goals on this install: its configuration doesn’t give them a place (HATCHLING_GOALS_DIR).',
    failed: 'Hatchling couldn’t change your goals. Its log says why.',
};
const REFUSED = 'This connection may not change your goals.';
const UNEXPECTED = 'Hatchling answered with something else, so the change is not confirmed. Check your goals again.';
/** A broker timestamp in Unix seconds, as a date on the person's clock. */
function day(seconds: number): string {
    return new Date(seconds * 1000).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' });
}
/** `goal` as the Goals page shows it. */
export function goalOf(goal: GoalView): Goal {
    const ended = goal.state === 'achieved' ? 'Reached' : goal.state === 'abandoned' ? 'Set aside' : null;
    const timeline = [{ date: day(goal.created_at), text: 'Set' }];
    if (ended !== null)
        timeline.push({ date: day(goal.updated_at), text: ended });
    else if (goal.updated_at !== goal.created_at)
        timeline.push({ date: day(goal.updated_at), text: 'Changed' });
    return {
        id: goal.id,
        kind: 'user',
        title: goal.title,
        description: goal.detail,
        category: 'Something else',
        status: goal.state === 'open' ? 'open' : 'closed',
        pulse: ended === null ? goal.detail || `Set ${day(goal.created_at)}` : `${ended} ${day(goal.updated_at)}`,
        pulseKind: ended === null ? 'monitoring' : 'milestone',
        attention: false,
        due: null,
        timeline,
    };
}
export class LiveGoals implements GoalsPort {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    #goals: readonly Goal[] = [];
    #ready = false;
    constructor(client: LiveClient, changed: () => void, nextId: () => string) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        client.subscribe(() => this.#watch());
    }
    /** Every goal: open first, the most recently changed first within each. */
    goals(): readonly Goal[] {
        return this.#goals;
    }
    async refresh(): Promise<void> {
        const outcome = await this.#ask({ op: 'list' });
        if (outcome?.result !== 'listed')
            return;
        this.#goals = outcome.goals.map(goalOf);
        this.#changed();
    }
    create(title: string, detail: string): Promise<CommandResult> {
        return this.#change({ op: 'create', title: title.trim(), detail: detail.trim() }, 'Goal set.');
    }
    update(id: string, title: string, detail: string): Promise<CommandResult> {
        return this.#change({ op: 'update', id, title: title.trim(), detail: detail.trim() }, 'Saved.');
    }
    close(id: string, achieved: boolean): Promise<CommandResult> {
        return this.#change({ op: 'close', id, achieved }, achieved ? 'Marked reached. It stays in Closed goals.' : 'Set aside. It stays in Closed goals.');
    }
    async #change(op: GoalOp, done: string): Promise<CommandResult> {
        const reply = await this.#client.request(this.#nextId(), { action: 'manage_goals', op });
        if (reply.kind !== 'answered')
            return { ok: false, message: reply.detail };
        const result = reply.result;
        if (result.result === 'refused')
            return { ok: false, message: REFUSED };
        if (result.result !== 'goals' || result.outcome.result === 'listed')
            return { ok: false, message: UNEXPECTED };
        const outcome = result.outcome;
        if (outcome.result === 'saved') {
            await this.refresh();
            return { ok: true, message: done };
        }
        if (outcome.result === 'not_found' || outcome.result === 'already_closed')
            await this.refresh();
        return { ok: false, message: SAID[outcome.result] };
    }
    /** Load the list each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh();
        this.#ready = ready;
    }
    async #ask(op: GoalOp): Promise<GoalResult | undefined> {
        const reply = await this.#client.request(this.#nextId(), { action: 'manage_goals', op });
        return reply.kind === 'answered' && reply.result.result === 'goals' ? reply.result.outcome : undefined;
    }
}
