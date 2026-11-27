// What the composer says about the model's context window (WO-056): how
// full the chat's last answer got, the model that answered, and what filled
// it. Every number is the broker's: the total is the model server's own
// count, and the parts are the broker's split of it by the size of each part
// of the request it sent, so each part is about right and they sum exactly.
// No DOM here: these are the words and numbers the composer and its panel
// show, and the tests read them directly.
import type { ContextUse } from '../../session/usage.js';
const SHORT = new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 1 });
/** A count of tokens as it's said: 8.3K, 1.2M. */
export function tokensShort(tokens: number): string {
    return SHORT.format(tokens);
}
/** A model's window as it's named: 16K for 16,384, 128K for 131,072. */
export function windowSize(window: number): string {
    return window % 1024 === 0 ? `${window / 1024}K` : SHORT.format(window);
}
/** How full the window was, in whole percent and at most 100; `null` when the window isn't known. */
export function shareOf(context: ContextUse): number | null {
    if (context.window === null || context.window <= 0)
        return null;
    return Math.min(100, Math.round((context.tokens / context.window) * 100));
}
/** The exact numbers, as one sentence: for the ring's name and its tooltip. */
export function exactWords(context: ContextUse): string {
    const share = shareOf(context);
    const tokens = tokensShort(context.tokens);
    return share === null || context.window === null
        ? `${tokens} tokens of context used on the last answer; the model’s window isn’t known`
        : `${tokens} of ${windowSize(context.window)} tokens of context (${share}%) used on the last answer`;
}
/** The panel's headline: how full, and of what. */
export function headline(context: ContextUse): string {
    const share = shareOf(context);
    return share === null || context.window === null ? `${tokensShort(context.tokens)} used` : `${share}% used · ${tokensShort(context.tokens)} of ${windowSize(context.window)}`;
}
/** One line of the panel: what it is, how much, and whether that's about or exact. */
export interface ContextRow {
    readonly label: string;
    readonly tokens: string;
    readonly about: boolean;
}
/** What filled the window, the broker's split (about), and what was free (exact). */
export function contextRows(context: ContextUse): readonly ContextRow[] {
    const rows: ContextRow[] = [];
    if (context.parts !== null) {
        const parts = context.parts;
        for (const [label, tokens] of [['Conversation', parts.conversation], ['Tools', parts.tools], ['Memory', parts.memory], ['System', parts.system]] as const)
            rows.push({ label, tokens: tokensShort(tokens), about: true });
    }
    if (context.window !== null && context.window > 0)
        rows.push({ label: 'Free', tokens: tokensShort(Math.max(0, context.window - context.tokens)), about: false });
    return rows;
}
/** How the panel's numbers are made, in plain words. */
export function countedNote(context: ContextUse): string {
    return context.parts === null
        ? 'The total is the model’s own count. What filled it isn’t known for this answer.'
        : 'The total is the model’s own count. Hatchling splits it by the size of each part of what it sent, so each part is about right.';
}
