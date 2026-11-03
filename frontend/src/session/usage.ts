// The model usage the broker counted, as the model server reported it with each
// reply: what the owner's Settings shows in place of a plan's quota. It's read,
// never written: nothing the app does changes what was counted.
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
export interface Usage {
    /** Whether the broker counts usage at all. */
    readonly counted: boolean;
    /** The last seven days, today first. */
    readonly days: readonly UsageDay[];
    /** Each model over those days, the most tokens first. */
    readonly models: readonly UsageModel[];
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
}
