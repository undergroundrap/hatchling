// The model Hatchling uses, and the ones its server has: what the owner chooses
// from in Settings. The servers, where the owner's words go, are the operator's
// to set up: when they set up two, a local one and a hosted one say, the owner
// switches between them here.
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
    /** The other server the operator set up, to switch to: none when there's one. */
    readonly other: ModelServerView | null;
    /** Whether the owner switched to the operator's other server. */
    readonly switched: boolean;
}
/** A model's server the operator set up, and the model on it. */
export interface ModelServerView {
    readonly endpoint: string | null;
    readonly name: string | null;
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
    /** Switch to the other server the operator set up, or back to the first. */
    switchServer(other: boolean): Promise<CommandResult>;
}
/** Whether `endpoint` is a server on this machine: the owner's words stay here. */
export function onThisMachine(endpoint: string | null): boolean {
    return endpoint !== null && /^http:\/\/(localhost|127\.0\.0\.1)(:\d+)?(\/|$)/.test(endpoint);
}
/** The host `endpoint` names, or the endpoint itself when it names none. */
export function hostOf(endpoint: string): string {
    try {
        return new URL(endpoint).hostname || endpoint;
    }
    catch {
        return endpoint;
    }
}
/** Where a model at `endpoint` answers, in a few words. */
export function whereShort(endpoint: string | null): string {
    if (endpoint === null)
        return 'on its server';
    return onThisMachine(endpoint) ? 'on this machine' : `hosted at ${hostOf(endpoint)}`;
}
