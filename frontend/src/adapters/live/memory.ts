// What the agent is given from the owner's memory, as the broker keeps it:
// what the agent asked to keep and the owner approved. Listed when the
// connection opens and when an approval is decided, and forgotten from the
// Memory page. The list is the broker's, never this app's guess.
import type { CommandResult, MemoryEntry } from '../../protocol/models.js';
import type { LiveClient } from './client.js';
import type { Kept, KeptOp, KeptResult, UiEvent } from './protocol/generated.js';
const FORGOTTEN = 'Forgotten. The agent isn’t given it any more.';
const SAID: Readonly<Record<Exclude<KeptResult['result'], 'listed' | 'forgotten'>, string>> = {
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
        date: kept.approved ? `Approved by you, ${day}` : `Kept ${day}`,
        closeness: 0,
    };
}
export class LiveMemory {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    #entries: readonly MemoryEntry[] = [];
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
        this.#changed();
    }
    async forget(id: string): Promise<CommandResult> {
        const reply = await this.#client.request(this.#nextId(), { action: 'manage_memory', op: { op: 'forget', id } });
        if (reply.kind !== 'answered')
            return { ok: false, message: reply.detail };
        const result = reply.result;
        if (result.result === 'refused')
            return { ok: false, message: REFUSED };
        if (result.result !== 'memory' || result.outcome.result === 'listed')
            return { ok: false, message: UNEXPECTED };
        const outcome = result.outcome;
        if (outcome.result === 'forgotten' || outcome.result === 'not_found')
            await this.refresh();
        return outcome.result === 'forgotten' ? { ok: true, message: FORGOTTEN } : { ok: false, message: SAID[outcome.result] };
    }
    /** Load the list each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh();
        this.#ready = ready;
    }
    async #ask(op: KeptOp): Promise<KeptResult | undefined> {
        const reply = await this.#client.request(this.#nextId(), { action: 'manage_memory', op });
        return reply.kind === 'answered' && reply.result.result === 'memory' ? reply.result.outcome : undefined;
    }
}
