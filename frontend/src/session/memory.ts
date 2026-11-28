// What the Memory page says of each kept memory beyond its words (WO-048):
// where it came from, what kind of thing it is, why and when it applies,
// whether it's known not to be so, what it replaced, when it was last
// confirmed, and whether the owner is asked if it's still so. Confirming is
// never a Command: only the owner's own click on the page does it.
import type { CommandResult } from '../protocol/models.js';
/** One kept memory's details, as the page shows them. */
export interface MemoryDetail {
    /** What kind of thing it is, in the page's words. */
    readonly kind: string | null;
    readonly why: string | null;
    readonly howToApply: string | null;
    /** Whether it records something known not to be so. */
    readonly absence: boolean;
    /** Where it came from, in the page's words. */
    readonly origin: string;
    /** When the owner last confirmed it, in the page's words. */
    readonly confirmed: string | null;
    /** Whether the owner is asked now if it's still so. */
    readonly due: boolean;
    /** The words of the memory it replaced. */
    readonly replaces: string | null;
    /** Whether Hatchling kept it automatically, from what the owner said, without their approval (WO-057). */
    readonly automatic: boolean;
}
/**
 * Whether what the agent asks to remember is kept at once, as last read
 * (WO-057): not yet, kept at once, asked first, or no memory on this install.
 */
export type MemoryModeView = 'unread' | 'automatic' | 'ask' | 'off';
export interface MemoryDetailsPort {
    /** The details of the kept memory `id`, when it's listed. */
    of(id: string): MemoryDetail | undefined;
    /** Say the kept memory `id` is still so. */
    confirm(id: string): Promise<CommandResult>;
    /** Whether what the agent asks to remember is kept at once. */
    mode(): MemoryModeView;
    /** The owner's choice: kept at once, or asked first. Only their own click sets it. */
    setMode(automatic: boolean): Promise<CommandResult>;
    /** The owner opened a page that says the mode: read it again, settled once it's read or couldn't be. */
    opened(): Promise<void>;
}
