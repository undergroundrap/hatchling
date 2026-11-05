// The model usage the broker counted, read when the connection opens and again
// after each answer the agent gives, since every answer is model calls. The
// counts are the broker's, never this app's guess: until it answers there are
// none. The owner's limits are set here, and answered with the usage as it
// then stands.
import type { LiveClient } from './client.js';
import type { ActionRefusal, ActivityEvent, ChatMessage, UsageReport } from './protocol/generated.js';
import type { CommandResult } from '../../protocol/models.js';
import type { ContextUse, Usage, UsageLimits, UsagePort, UsageState } from '../../session/usage.js';
const REFUSED = 'This connection may not read the usage.';
const UNEXPECTED = 'Hatchling answered with something else.';
/** Why limits weren't set, by the broker's refusal. */
const NOT_SET: Readonly<Record<ActionRefusal, string>> = {
    bad_request: 'Hatchling didn’t take those limits: each is a whole number of tokens, from 1 to a quadrillion.',
    unavailable: 'Hatchling couldn’t keep the limits. It counts usage only when scheduling is on; its log says if a write failed.',
    not_granted: 'This connection may not set the limits.',
    not_connected: 'Not connected.',
};
/** The broker's report, in the app's words. */
export function usageOf(report: UsageReport): Usage {
    return {
        counted: report.counted,
        days: report.days.map(day => ({ date: day.date, calls: day.calls, inputTokens: day.input_tokens, outputTokens: day.output_tokens, unreported: day.unreported })),
        models: report.models.map(model => ({ model: model.model, calls: model.calls, inputTokens: model.input_tokens, outputTokens: model.output_tokens })),
        limits: { daily: report.limits.daily_tokens ?? null, monthly: report.limits.monthly_tokens ?? null },
        spent: { today: report.spent.today, month: report.spent.month, monthOf: report.spent.month_of },
    };
}
export class LiveUsage implements UsagePort {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    #state: UsageState = { kind: 'unread' };
    #context: ContextUse | null = null;
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
    context(): ContextUse | null {
        return this.#context;
    }
    /** A run's record: a chat answer's model calls say how full the context got. */
    observeRun(event: ActivityEvent): void {
        if (event.event !== 'run_summary' || !event.chat || event.model_calls === 0)
            return;
        this.#context = { tokens: event.peak_input_tokens, window: event.context_window ?? null, onCpu: event.model_on_cpu ?? null, memoryLow: event.memory_low ?? false };
        this.#changed();
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
    async setLimits(limits: UsageLimits): Promise<CommandResult> {
        const reply = await this.#client.request(this.#nextId(), {
            action: 'set_usage_limits',
            limits: { ...(limits.daily === null ? {} : { daily_tokens: limits.daily }), ...(limits.monthly === null ? {} : { monthly_tokens: limits.monthly }) },
        });
        if (reply.kind !== 'answered')
            return { ok: false, message: reply.detail };
        if (reply.result.result === 'refused')
            return { ok: false, message: NOT_SET[reply.result.reason] };
        if (reply.result.result !== 'usage')
            return { ok: false, message: UNEXPECTED };
        this.#state = { kind: 'read', usage: usageOf(reply.result.usage) };
        this.#changed();
        const none = limits.daily === null && limits.monthly === null;
        return { ok: true, message: none ? 'No limits now.' : 'Limits saved. Hatchling checks them before every model call.' };
    }
    /** Read it each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh();
        this.#ready = ready;
    }
}
