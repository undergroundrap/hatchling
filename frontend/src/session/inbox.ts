// Files the owner gives the agent: sent into the inbox of the agent's computer,
// its Room, where the agent reads them and can change none of them (WO-041).
// A file goes straight to the broker in parts, never as a Command, so no
// automation can send one. Optional frontend capability: only a live install
// has it. NOT an addition to the frozen wire protocol.
import type { CommandResult } from '../protocol/models.js';
/** A file being sent, or one that ended, in the chat it was given in. */
export type FileSend = {
    /** Its place among the chat's sends. */
    readonly id: string;
    /** The chat it was given in. */
    readonly chatId: string;
    /** The owner's name for it: untrusted text, shown inert. */
    readonly name: string;
    /** Its size. */
    readonly bytes: number;
} & ({
    readonly state: 'waiting';
} | {
    readonly state: 'sending';
    /** Parts sent, of all its parts. */
    readonly sent: number;
    readonly parts: number;
} | {
    /** Not put: nothing of it is in the inbox. */
    readonly state: 'refused';
    readonly detail: string;
    /** Whether sending it again could work. */
    readonly retry: boolean;
} | {
    /** It may or may not have been put: the inbox's list says. */
    readonly state: 'unknown';
    readonly detail: string;
});
/** A file the chat showed as put, by the message that showed it. */
export interface GivenFile {
    /** The owner's name for it: untrusted text, shown inert. */
    readonly name: string;
    /** Its name in the inbox. */
    readonly placedAs: string;
    readonly bytes: number;
    /** It holds a secret Hatchling keeps, or couldn't be checked. */
    readonly holdsSecret: boolean;
    /** Taken out of the inbox since. */
    readonly removed: boolean;
}
/** A file in the inbox, as the broker lists it. */
export interface InboxEntry {
    readonly name: string;
    readonly bytes: number;
}
/** The inbox as last read. */
export type InboxState = {
    readonly kind: 'unread';
} | {
    readonly kind: 'failed';
    readonly detail: string;
} | {
    readonly kind: 'read';
    readonly files: readonly InboxEntry[];
};
export interface InboxPort {
    /** Whether a file may be given in `chatId`: the main chat alone. */
    takes(chatId: string): boolean;
    /** Sends `file` in `chatId`, after any other the app is sending. */
    give(chatId: string, file: File): void;
    /** The files being sent in `chatId`, or that ended without being put, oldest first. */
    sends(chatId: string): readonly FileSend[];
    /** Sends a file that wasn't put again, from its start. */
    retry(id: string): void;
    /** Lets go of a send that ended: it is no longer shown. */
    dismiss(id: string): void;
    /** The file the chat's message `messageId` showed as put. */
    given(messageId: string): GivenFile | undefined;
    current(): InboxState;
    /** Reads the inbox's list again. */
    refresh(): Promise<void>;
    /** Takes the file `placedAs` out of the inbox. */
    remove(placedAs: string): Promise<CommandResult>;
}
/** The id of the warning after a file given that holds a secret: the file's message id, and this. */
export const SECRET_SUFFIX = ':secret';
/** A name the inbox gives: a number, a hyphen, and a stem of plain ASCII. */
export function isInboxName(name: string): boolean {
    return /^\d{4,9}-[A-Za-z0-9_-][A-Za-z0-9._-]{0,63}$/u.test(name);
}
/**
 * Whether `name` is one the broker gives a file the agent handed over: 1 to
 * 64 ASCII letters, digits, `.`, `_` or `-`, not starting with a dot. Any
 * other is never shown as the file's name.
 */
export function isLibraryName(name: string): boolean {
    return /^[A-Za-z0-9_-][A-Za-z0-9._-]{0,63}$/u.test(name);
}
/** A size in the owner's words. */
export function sizeWords(bytes: number): string {
    if (bytes < 1024)
        return bytes === 1 ? '1 byte' : `${bytes} bytes`;
    if (bytes < 1048576)
        return `${Number((bytes / 1024).toFixed(1))} KB`;
    return `${Number((bytes / 1048576).toFixed(1))} MB`;
}
