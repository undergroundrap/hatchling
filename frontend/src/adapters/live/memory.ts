// What the agent is given from the owner's memory, as the broker keeps it:
// what the agent asked to keep and the owner approved. Listed when the
// connection opens and when an approval is decided, and forgotten from the
// Memory page. The list is the broker's, never this app's guess.
import type { CommandResult, MemoryEntry } from '../../protocol/models.js';
import type { MemoryDetail, MemoryDetailsPort, MemoryModeView } from '../../session/memory.js';
import type { LiveClient } from './client.js';
import type { Kept, KeptOp, KeptResult, UiEvent } from './protocol/generated.js';
const FORGOTTEN = 'Forgotten. The agent isn’t given it any more.';
const CONFIRMED = 'Confirmed: it’s still so, as of today.';
/** What setting the mode is answered, by what it is now. */
const MODE_SET: Readonly<Record<'automatic' | 'ask', string>> = {
    automatic: 'Memory is automatic: what you tell the agent about yourself is kept at once, unless the conversation read something from outside.',
    ask: 'The agent asks you first now: nothing is kept until you approve its words.',
};
const SAID: Readonly<Record<Exclude<KeptResult['result'], 'listed' | 'forgotten' | 'confirmed' | 'mode'>, string>> = {
    not_found: 'That was no longer kept.',
    off: 'Hatchling keeps no memory on this install: its configuration doesn’t give it a place (HATCHLING_MEMORY_DIR).',
    failed: 'Hatchling couldn’t change your memory. Its log says why.',
};
const REFUSED = 'This connection may not change your memory.';
const UNEXPECTED = 'Hatchling answered with something else, so nothing is confirmed. Check your memory again.';
/** The words that title an entry: its first line, cut at a word near 60 characters. */
export function titleOf(content: string): string {
    const end = content.indexOf('\n');
    const line = (end === -1 ? content : content.slice(0, end)).trim();
    if (line.length <= 60)
        return line;
    const cut = line.slice(0, 60);
    const space = cut.lastIndexOf(' ');
    return `${(space > 20 ? cut.slice(0, space) : cut).trimEnd()}…`;
}
/** What's kept, as the Memory page shows it. */
export function entryOf(kept: Kept): MemoryEntry {
    const day = new Date(kept.kept_at * 1000).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' });
    return {
        id: kept.id,
        kind: 'memory',
        title: titleOf(kept.content),
        body: kept.content,
        date: kept.kept_by === 'automatic' ? `Kept automatically, ${day}` : kept.approved ? `Approved by you, ${day}` : `Kept ${day}`,
        closeness: 0,
    };
}
/** Each kind of memory, as the page names it. */
const KINDS: Readonly<Record<string, string>> = {
    owner: 'About you',
    guidance: 'How you want it to work',
    work: 'About your work',
    pointer: 'Where something is',
};
/** A day, as the page says it. */
function dayOf(secs: number): string {
    return new Date(secs * 1000).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' });
}
/** Where a memory came from, as the page says it. */
function originOf(kept: Kept): string {
    const from = kept.from ?? { from: 'not_recorded' };
    switch (from.from) {
        case 'chat': return from.chat === 'local' ? 'Asked to remember in your chat' : 'Asked to remember in a chat';
        case 'run': return 'Asked to remember by a scheduled run';
        case 'approval': return 'Approved by you';
        case 'not_recorded': return 'Where it came from isn’t recorded';
    }
}
/** What the Memory page says of a kept memory beyond its words. */
export function detailOf(kept: Kept): MemoryDetail {
    return {
        kind: kept.kind ? KINDS[kept.kind] ?? null : null,
        why: kept.why ?? null,
        howToApply: kept.how_to_apply ?? null,
        absence: kept.absence ?? false,
        origin: originOf(kept),
        confirmed: kept.confirmed_at === undefined || kept.confirmed_at === null || kept.confirmed_at === kept.kept_at ? null : `Last confirmed ${dayOf(kept.confirmed_at)}`,
        due: kept.due ?? false,
        replaces: kept.replaces ?? null,
        automatic: kept.kept_by === 'automatic',
    };
}
/** What a change is answered: anything but a listing. */
type Changed = Exclude<KeptResult, {
    result: 'listed';
}>;
export class LiveMemory implements MemoryDetailsPort {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    #entries: readonly MemoryEntry[] = [];
    #details = new Map<string, MemoryDetail>();
    #mode: MemoryModeView = 'unread';
    #ready = false;
    constructor(client: LiveClient, changed: () => void, nextId: () => string) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        client.subscribe(() => this.#watch());
    }
    /** What's kept, the newest first. */
    entries(): readonly MemoryEntry[] {
        return this.#entries;
    }
    of(id: string): MemoryDetail | undefined {
        return this.#details.get(id);
    }
    async confirm(id: string): Promise<CommandResult> {
        const outcome = await this.#change({ op: 'confirm', id });
        if (typeof outcome === 'string')
            return { ok: false, message: outcome };
        if (outcome.result === 'confirmed' || outcome.result === 'not_found')
            await this.refresh();
        return outcome.result === 'confirmed' ? { ok: true, message: CONFIRMED } : { ok: false, message: this.#says(outcome) };
    }
    mode(): MemoryModeView {
        return this.#mode;
    }
    opened(): Promise<void> {
        return this.#ready ? this.#readMode() : Promise.resolve();
    }
    async setMode(automatic: boolean): Promise<CommandResult> {
        const outcome = await this.#change({ op: 'set_mode', mode: automatic ? 'automatic' : 'ask' });
        if (typeof outcome === 'string')
            return { ok: false, message: outcome };
        if (outcome.result !== 'mode')
            return { ok: false, message: this.#says(outcome) };
        this.#mode = outcome.mode;
        this.#changed();
        return { ok: true, message: MODE_SET[outcome.mode] };
    }
    /** The mode as the broker says it: unchanged when it couldn't be read. */
    async #readMode(): Promise<void> {
        const outcome = await this.#ask({ op: 'mode' });
        const mode = outcome?.result === 'mode' ? outcome.mode : outcome?.result === 'off' ? 'off' : undefined;
        if (mode === undefined || mode === this.#mode)
            return;
        this.#mode = mode;
        this.#changed();
    }
    /**
     * An approval decided may keep something, and the broker's notice says
     * once it has: read again on either.
     */
    observe(event: UiEvent): void {
        const body = event.body;
        if (body.kind === 'confirmation_decided' || (body.kind === 'chat_message' && body.author === 'system'))
            void this.refresh();
    }
    async refresh(): Promise<void> {
        const outcome = await this.#ask({ op: 'list' });
        if (outcome?.result !== 'listed')
            return;
        this.#entries = outcome.kept.map(entryOf);
        this.#details = new Map(outcome.kept.map(kept => [kept.id, detailOf(kept)]));
        this.#changed();
    }
    async forget(id: string): Promise<CommandResult> {
        const outcome = await this.#change({ op: 'forget', id });
        if (typeof outcome === 'string')
            return { ok: false, message: outcome };
        if (outcome.result === 'forgotten' || outcome.result === 'not_found')
            await this.refresh();
        return outcome.result === 'forgotten' ? { ok: true, message: FORGOTTEN } : { ok: false, message: this.#says(outcome) };
    }
    /** A change's answer, or a sentence when there was none, or another. */
    async #change(op: KeptOp): Promise<Changed | string> {
        const reply = await this.#client.request(this.#nextId(), { action: 'manage_memory', op });
        if (reply.kind !== 'answered')
            return reply.detail;
        const result = reply.result;
        if (result.result === 'refused')
            return REFUSED;
        if (result.result !== 'memory' || result.outcome.result === 'listed')
            return UNEXPECTED;
        return result.outcome;
    }
    #says(outcome: Changed): string {
        switch (outcome.result) {
            case 'not_found':
            case 'off':
            case 'failed':
                return SAID[outcome.result];
            case 'forgotten':
            case 'confirmed':
            case 'mode':
                return UNEXPECTED;
        }
    }
    /** Load the list, then whether memory is automatic, each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh().then(() => this.#readMode());
        this.#ready = ready;
    }
    async #ask(op: KeptOp): Promise<KeptResult | undefined> {
        const reply = await this.#client.request(this.#nextId(), { action: 'manage_memory', op });
        return reply.kind === 'answered' && reply.result.result === 'memory' ? reply.result.outcome : undefined;
    }
}
