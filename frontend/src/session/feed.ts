// The owner's Feed page, connected to Hatchling: a post their agent writes from
// their feed prompt every day at the time they pick, or once when they ask, and
// keeps there. The prompt and time are a schedule the broker keeps; the posts
// are the broker's record of what those runs wrote. Never a Command: the
// prompt's Command names no time.
import type { CommandResult } from '../protocol/models.js';
/** The schedule that writes the posts. */
export interface FeedSetup {
    /** What the owner asked for: the schedule's task, their words. */
    readonly prompt: string;
    /** `HH:MM`, on their clock. */
    readonly time: string;
    /** When it runs, in words. */
    readonly when: string;
}
/** The page as last read: not yet, couldn't be, or what the broker keeps. */
export type FeedState = {
    readonly kind: 'unread';
} | {
    readonly kind: 'failed';
    readonly detail: string;
} | {
    readonly kind: 'read';
    /** The daily post's schedule, if there is one. */
    readonly setup: FeedSetup | null;
    /** Whether a post asked for now is still being written. */
    readonly writing: boolean;
};
export interface FeedPort {
    current(): FeedState;
    /** Write a post from `prompt` every day at `time` (`HH:MM`), in place of any earlier. */
    save(prompt: string, time: string): Promise<CommandResult>;
    /** Write one post now, from the saved prompt. */
    writeNow(): Promise<CommandResult>;
    /** Stop the daily post. The posts already written stay. */
    stop(): Promise<CommandResult>;
}
