// The model usage the broker counted, read when the connection opens and again
// after each answer the agent gives, since every answer is model calls. The
// counts are the broker's, never this app's guess: until it answers there are
// none.
import type { LiveClient } from './client.js';
import type { ChatMessage, UsageReport } from './protocol/generated.js';
import type { Usage, UsagePort, UsageState } from '../../session/usage.js';
const REFUSED = 'This connection may not read the usage.';
const UNEXPECTED = 'Hatchling answered with something else.';
/** The broker's report, in the app's words. */
export function usageOf(report: UsageReport): Usage {
    return {
        counted: report.counted,
        days: report.days.map(day => ({ date: day.date, calls: day.calls, inputTokens: day.input_tokens, outputTokens: day.output_tokens, unreported: day.unreported })),
        models: report.models.map(model => ({ model: model.model, calls: model.calls, inputTokens: model.input_tokens, outputTokens: model.output_tokens })),
    };
}
export class LiveUsage implements UsagePort {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    #state: UsageState = { kind: 'unread' };
    #ready = false;
    constructor(client: LiveClient, changed: () => void, nextId: () => string) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        client.subscribe(() => this.#watch());
    }
    current(): UsageState {
        return this.#state;
    }
    /** An answer from the agent, or Hatchling's own word, came after model calls: read again. */
    observe(message: ChatMessage): void {
        if (message.author !== 'user')
            void this.refresh();
    }
    async refresh(): Promise<void> {
        const reply = await this.#client.request(this.#nextId(), { action: 'read_usage' });
        if (reply.kind !== 'answered')
            this.#state = { kind: 'failed', detail: reply.detail };
        else if (reply.result.result === 'usage')
            this.#state = { kind: 'read', usage: usageOf(reply.result.usage) };
        else
            this.#state = { kind: 'failed', detail: reply.result.result === 'refused' ? REFUSED : UNEXPECTED };
        this.#changed();
    }
    /** Read it each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh();
        this.#ready = ready;
    }
}
