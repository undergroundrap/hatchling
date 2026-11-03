// Scheduled work that needs the owner: a run cut short when Hatchling stopped,
// held until they decide, and a result kept because it failed or couldn't be
// delivered. No scheduled run ends silently: each one is delivered, told as a
// failure, or listed here. Like a new conversation, it's never a Command: what
// runs again is the owner's to decide.
import type { CommandResult } from '../protocol/models.js';
/** What the owner can do about an item. */
export type AttentionAction = 'retry' | 'resolve' | 'dismiss';
export interface AttentionItem {
    readonly id: string;
    /** The work, by its task when the owner wrote it. */
    readonly title: string;
    /** What happened, in the broker's words. */
    readonly detail: string;
    /** When it was due, on the person's clock. */
    readonly when: string;
    /** A result that couldn't be delivered, as the run wrote it; otherwise none. */
    readonly result: string | null;
    readonly actions: readonly AttentionAction[];
}
export interface AttentionPort {
    /** What needs the owner, oldest first. */
    items(): readonly AttentionItem[];
    /** Do `action` about the item `id`. */
    act(id: string, action: AttentionAction): Promise<CommandResult>;
}
