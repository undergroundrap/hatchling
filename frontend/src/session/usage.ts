// The model usage the broker counted, as the model server reported it with each
// reply, and the limits the owner set on it: what the owner's Settings shows in
// place of a plan's quota. The counts are read, never written: nothing the app
// does changes what was counted. The limits are the owner's to set, and the
// broker enforces them before every model call.
import type { CommandResult } from '../protocol/models.js';
export interface UsageDay {
    /** `YYYY-MM-DD` on the machine's clock. */
    readonly date: string;
    readonly calls: number;
    /** Tokens the model read: prompts, history and tool results. */
    readonly inputTokens: number;
    /** Tokens the model wrote, thinking included. */
    readonly outputTokens: number;
    /** Calls whose reply reported no usage, so their tokens aren't counted. */
    readonly unreported: number;
}
export interface UsageModel {
    readonly model: string;
    readonly calls: number;
    readonly inputTokens: number;
    readonly outputTokens: number;
}
/** The most tokens the owner allows, read and written together: `null` is no limit. */
export interface UsageLimits {
    /** In a day, on the machine's clock. */
    readonly daily: number | null;
    /** In a calendar month. */
    readonly monthly: number | null;
}
/** The tokens counted toward the limits. */
export interface UsageSpent {
    readonly today: number;
    readonly month: number;
    /** The month, `YYYY-MM`. */
    readonly monthOf: string;
}
export interface Usage {
    /** Whether the broker counts usage at all. */
    readonly counted: boolean;
    /** The last seven days, today first. */
    readonly days: readonly UsageDay[];
    /** Each model over those days, the most tokens first. */
    readonly models: readonly UsageModel[];
    readonly limits: UsageLimits;
    readonly spent: UsageSpent;
}
/** How full the model's context was on the chat's last answer, as the broker counted it. */
export interface ContextUse {
    /** Tokens the largest model call of that answer read. */
    readonly tokens: number;
    /** The model's context window: as its server says, or as the operator declared it. */
    readonly window: number | null;
    /** Bytes of the model that ran on the CPU on that answer, as its server says: answers are slower. */
    readonly onCpu: number | null;
    /** Whether the computer was low on memory on that answer. */
    readonly memoryLow: boolean;
}
/** Usage as last read: not yet, couldn't be, or what the broker said. */
export type UsageState = {
    readonly kind: 'unread';
} | {
    readonly kind: 'failed';
    readonly detail: string;
} | {
    readonly kind: 'read';
    readonly usage: Usage;
};
export interface UsagePort {
    current(): UsageState;
    /** Read it again from the broker. */
    refresh(): Promise<void>;
    /** Set the limits, each replacing what was set. */
    setLimits(limits: UsageLimits): Promise<CommandResult>;
    /** How full the context was on the chat's last answer: none until one is recorded. */
    context(): ContextUse | null;
}
