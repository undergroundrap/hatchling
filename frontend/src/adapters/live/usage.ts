// The model usage the broker counted, read when the connection opens and again
// after each answer the agent gives, since every answer is model calls. The
// counts are the broker's, never this app's guess: until it answers there are
// none. The owner's limits are set here, and answered with the usage as it
// then stands.
import type { LiveClient } from './client.js';
import type { ActionRefusal, ActivityEvent, ChatMessage, UsageReport } from './protocol/generated.js';
import type { CommandResult } from '../../protocol/models.js';
import { MAIN_CHAT, appChat } from './chat.js';
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
/** How long a burst of run summaries is gathered before the composer repaints: a replay's arrive together. */
export const SETTLE_MS = 50;
export class LiveUsage implements UsagePort {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    readonly #later: (run: () => void) => void;
    #state: UsageState = { kind: 'unread' };
    /** Each chat's last answer's context, by the app's chat id (WO-056). */
    readonly #contexts = new Map<string, ContextUse>();
    /** What the model has written so far for the run answering the owner, as the broker counted it (#775). */
    #writing: number | null = null;
    #settling = false;
    #ready = false;
    constructor(client: LiveClient, changed: () => void, nextId: () => string, later: (run: () => void) => void = run => { setTimeout(run, SETTLE_MS); }) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        this.#later = later;
        client.subscribe(() => this.#watch());
    }
    current(): UsageState {
        return this.#state;
    }
    context(chatId: string): ContextUse | null {
        return this.#contexts.get(chatId) ?? null;
    }
    /** The broker tells the steps of the run answering the owner alone: the main chat's. */
    written(chatId: string): number | null {
        return chatId === MAIN_CHAT ? this.#writing : null;
    }
    /** A step of the run answering the owner, with what its model has written so far when the broker said. */
    observeStep(step: {
        readonly tokens?: number | null;
    }): void {
        if (typeof step.tokens === 'number')
            this.#writing = step.tokens;
    }
    /**
     * A run's record: a chat answer's model calls say how full the context got,
     * in the chat it answered (WO-056). A record from before it named its chat
     * is the main chat's. A burst of them, as a replay brings, repaints once,
     * when it settles: the composer never flickers through older answers.
     */
    observeRun(event: ActivityEvent): void {
        if (event.event !== 'run_summary' || !event.chat || event.model_calls === 0)
            return;
        const parts = event.context_parts ?? null;
        const chat = event.chat_id ?? null;
        this.#contexts.set(chat === null ? MAIN_CHAT : appChat(chat), {
            tokens: event.peak_input_tokens,
            window: event.context_window ?? null,
            onCpu: event.model_on_cpu ?? null,
            memoryLow: event.memory_low ?? false,
            model: event.model ?? null,
            parts: parts === null ? null : { system: parts.system, tools: parts.tools, memory: parts.memory, conversation: parts.conversation },
        });
        if (this.#settling)
            return;
        this.#settling = true;
        this.#later(() => {
            this.#settling = false;
            this.#changed();
        });
    }
    /** An answer from the agent, or Hatchling's own word, came after model calls: read again. */
    observe(message: ChatMessage): void {
        if (message.author === 'user')
            return;
        // Its answer, or Hatchling's word that it ended: the run is over.
        this.#writing = null;
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
