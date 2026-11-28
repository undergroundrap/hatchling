// What background work may not apply on its own, waiting on the Memory page
// for the owner: Hatchling's review queue (WO-054). A loop proposes, and the
// owner approves or rejects. Never a Command: no Command decides one, so
// nothing but the owner's own click on the page can.
import type { CommandResult } from '../protocol/models.js';
/** One proposal, as the page shows it. */
export interface ReviewEntry {
    readonly id: string;
    /** What kind of change, in the page's words. */
    readonly kind: string;
    /** What would change, in the loop's words. */
    readonly change: string;
    /** Why, in the loop's words. */
    readonly why: string;
    /** When it was proposed. */
    readonly date: string;
    readonly state: 'waiting' | 'approved' | 'rejected' | 'stale';
    /** Where a decided one stands, in the page's words. */
    readonly outcome: string;
    /** Whether approving it applies it at once, as a memory to keep does, rather than when its work next runs. */
    readonly atOnce: boolean;
}
/** The list as last read: not yet, couldn't be, or what the broker keeps, the newest first. */
export type ReviewList = {
    readonly kind: 'unread';
} | {
    readonly kind: 'failed';
    readonly detail: string;
} | {
    readonly kind: 'read';
    readonly entries: readonly ReviewEntry[];
};
export interface ReviewPort {
    current(): ReviewList;
    /** Approve the waiting proposal `id`, or reject it. */
    decide(id: string, approve: boolean): Promise<CommandResult>;
    /** The owner opened the Memory page: read it again, since loops propose without the app being told. */
    opened(): void;
}
