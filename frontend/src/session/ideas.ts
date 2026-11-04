// The owner's Ideas page, connected to Hatchling: what the agent suggests it
// could do for them, in its own words. Nothing runs from a suggestion; starting
// one fills their chat box with it, for them to read and send. Asking for ideas
// now is a one-off run for the page, which only they can start. Never a
// Command: no Command asks for ideas.
import type { CommandResult } from '../protocol/models.js';
/** The page as last read: not yet, couldn't be, or what the broker keeps. */
export type IdeasState = {
    readonly kind: 'unread';
} | {
    readonly kind: 'failed';
    readonly detail: string;
} | {
    readonly kind: 'read';
    /** Whether ideas asked for now are still being thought of. */
    readonly suggesting: boolean;
};
export interface IdeasPort {
    current(): IdeasState;
    /** Ask for ideas now, from what Hatchling can read about the owner. */
    suggestNow(): Promise<CommandResult>;
}
