// The model Hatchling uses, and the ones its server has: what the owner chooses
// from in Settings. Only the model's name is chosen here: its server, where the
// owner's words go, is the operator's setting.
import type { CommandResult } from '../protocol/models.js';
/** The model, as the broker last said. */
export interface ModelView {
    /** The model's server, as the operator set it. */
    readonly endpoint: string | null;
    /** The model the operator installed with. */
    readonly configured: string | null;
    /** The model the owner chose, used over the installed one. */
    readonly chosen: string | null;
    /** The models the server has, when it lists them: a local server does. */
    readonly available: readonly string[] | null;
    /** The window of the model in use, in tokens, when it's known. */
    readonly window: number | null;
    /** All the model in use takes, in bytes, when it's loaded and its server says. */
    readonly size: number | null;
    /** How much of that runs on the CPU, more slowly. */
    readonly onCpu: number | null;
}
/** The model as last read: not yet, couldn't be, or what the broker said. */
export type ModelState = {
    readonly kind: 'unread';
} | {
    readonly kind: 'failed';
    readonly detail: string;
} | {
    readonly kind: 'read';
    readonly model: ModelView;
};
export interface ModelPort {
    current(): ModelState;
    /** Read it again from the broker: the server's list, and the model's window and fit. */
    refresh(): Promise<void>;
    /** Use `name` from the next run on; null goes back to the installed model. */
    choose(name: string | null): Promise<CommandResult>;
}
