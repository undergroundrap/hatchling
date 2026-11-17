// Files the owner gives the agent, sent into the inbox of the agent's computer
// (WO-041). A file goes straight to the broker in parts, never as a Command:
// a beginning that says its size, its parts in order, each answered before
// the next, then its end. One file at a time for the whole app; a second
// waits its turn. Up to its end, a file that didn't arrive wasn't put; after
// its end, a file whose answer was lost may have been, and the inbox's list,
// or the file's own word on the stream, says which. It is never sent again
// unasked.
import type { LiveClient, Reply } from './client.js';
import type { ActionRefusal, FileInRefusal, UiAction, UiActionResult, UiEventBody } from './protocol/generated.js';
import type { CommandResult } from '../../protocol/models.js';
import type { FileSend, GivenFile, InboxPort, InboxState } from '../../session/inbox.js';
import { isInboxName } from '../../session/inbox.js';
import { MAIN_CHAT, wireChat } from './chat.js';
/** A file the broker put in the inbox, as the stream says. */
export type FileIn = Extract<UiEventBody, {
    kind: 'file_in';
}>;
/** The most bytes a file given to the agent may hold: the inbox's bound. */
export const FILE_IN_BYTES = 8 * 1024 * 1024;
/** The most bytes a part carries: the broker's bound. */
const PART_BYTES = 32 * 1024;
/** How long each step but the last is waited for. */
const STEP_WAIT_MS = 60000;
/** How long the end is waited for: the put may wait its turn at the agent's disk. */
const END_WAIT_MS = 26 * 60000;
const NOTHING_PUT = 'Nothing was put.';
const TOO_LARGE = 'This file is larger than Hatchling takes: 8 MB at most.';
const UNEXPECTED = 'Hatchling answered with something else.';
const NOT_ARRIVED = `It didn’t arrive whole. ${NOTHING_PUT}`;
const NOT_CONFIRMED = 'Not confirmed: Hatchling didn’t say whether it was put. The agent’s inbox, in Settings, lists what it holds.';
const NO_ROOM = 'The agent’s computer isn’t on, so it can’t take a file.';
/** A sentence of the broker's, ended once. */
function sentence(said: string): string {
    return said.replace(/[.\s]+$/u, '');
}
/** Why the inbox didn't take a file, and whether sending it again could work. */
function refusal(reason: FileInRefusal): readonly [
    string,
    boolean
] {
    switch (reason.refusal) {
        case 'too_large':
            return [TOO_LARGE, false];
        case 'busy':
            return ['Another file is being given to Hatchling. Try again when it’s done.', true];
        case 'no_room':
            return [NO_ROOM, true];
        case 'unknown_upload':
        case 'out_of_step':
            return [NOT_ARRIVED, true];
        case 'refused':
            return [`The agent’s inbox didn’t take it: ${sentence(reason.said)}. ${NOTHING_PUT}`, true];
        case 'unknown':
            return [NOT_CONFIRMED, false];
    }
}
/** Why the API refused a file, before the broker took any of it. */
const REFUSED: Readonly<Record<ActionRefusal, readonly [
    string,
    boolean
]>> = {
    not_granted: ['This connection may not give Hatchling files.', false],
    bad_request: ['Hatchling takes a file only in the main chat.', false],
    unavailable: [`Hatchling couldn’t take the file. ${NOTHING_PUT}`, true],
    not_connected: [`Not connected. ${NOTHING_PUT}`, true],
};
/** `bytes` as base64, a piece at a time. */
export function base64(bytes: Uint8Array): string {
    let text = '';
    for (let at = 0; at < bytes.length; at += 8192)
        text += String.fromCharCode(...bytes.subarray(at, at + 8192));
    return btoa(text);
}
/** `reply`, or `late` when it takes longer than `ms`. */
export function within(reply: Promise<Reply>, ms: number): Promise<Reply | 'late'> {
    return new Promise(resolve => {
        const timer = setTimeout(() => resolve('late'), ms);
        void reply.then(answer => {
            clearTimeout(timer);
            resolve(answer);
        });
    });
}
/** A send, as this app keeps it: the file itself until it is let go. */
type Held = FileSend & {
    readonly file: File;
    /** The broker's id for it, once begun: the stream's word of it is by this. */
    readonly uploadId?: string;
};
/** How a step of a send came out: what was answered, or why nothing was. */
type Step = {
    readonly ok: true;
    readonly result: UiActionResult;
} | {
    readonly ok: false;
    /** Not answered in time; never sent, as the app wasn't connected; or sent, and its answer lost with the connection. */
    readonly why: 'late' | 'unsent' | 'lost';
};
/** Before a file's end, why nothing was answered, in the owner's words: nothing was put. */
const UNANSWERED: Readonly<Record<'late' | 'unsent' | 'lost', string>> = {
    late: `Hatchling didn’t answer in time. ${NOTHING_PUT}`,
    unsent: `Not connected. ${NOTHING_PUT}`,
    lost: `The connection was lost. ${NOTHING_PUT}`,
};
/** Why the inbox couldn't be read or a file removed, by the API's refusal. */
const NOT_READ: Readonly<Record<ActionRefusal, string>> = {
    not_granted: 'This connection may not read the agent’s inbox.',
    bad_request: UNEXPECTED,
    unavailable: 'Hatchling couldn’t read the agent’s inbox.',
    not_connected: 'Not connected.',
};
const NOT_REMOVED: Readonly<Record<ActionRefusal, string>> = {
    not_granted: 'This connection may not remove files from the agent’s inbox.',
    bad_request: 'That isn’t a file in the agent’s inbox.',
    unavailable: 'Hatchling couldn’t remove it.',
    not_connected: 'Not connected.',
};
/** A file given, as kept here: by the inbox read it was first known before. */
type Kept = GivenFile & {
    /** Reads of the list begun before this file was known. */
    readonly after: number;
};
export class LiveInbox implements InboxPort {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    readonly #waits: {
        readonly step: number;
        readonly end: number;
    };
    #sends: Held[] = [];
    #serial = 0;
    #busy = false;
    readonly #given = new Map<string, Kept>();
    #state: InboxState = { kind: 'unread' };
    #ready = false;
    /** The read of the list under way, and whether another is wanted after it. */
    #reading: Promise<void> | null = null;
    #again = false;
    /** Reads begun, and the one whose list is the state now. */
    #reads = 0;
    #listed = 0;
    constructor(client: LiveClient, changed: () => void, nextId: () => string, waits = { step: STEP_WAIT_MS, end: END_WAIT_MS }) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        this.#waits = waits;
        client.subscribe(() => this.#watch());
    }
    takes(chatId: string): boolean {
        return chatId === MAIN_CHAT;
    }
    give(chatId: string, file: File): void {
        const base = { id: `file-send-${++this.#serial}`, chatId, name: file.name, bytes: file.size, file };
        if (!this.takes(chatId))
            this.#sends.push({ ...base, state: 'refused', detail: REFUSED.bad_request[0], retry: false });
        else if (file.size > FILE_IN_BYTES)
            this.#sends.push({ ...base, state: 'refused', detail: TOO_LARGE, retry: false });
        else
            this.#sends.push({ ...base, state: 'waiting' });
        this.#changed();
        this.#pump();
    }
    sends(chatId: string): readonly FileSend[] {
        return this.#sends.filter(send => send.chatId === chatId).map(({ file: _file, uploadId: _uploadId, ...send }) => send);
    }
    retry(id: string): void {
        this.#sends = this.#sends.map(send => send.id === id && send.state === 'refused' && send.retry
            ? { id: send.id, chatId: send.chatId, name: send.name, bytes: send.bytes, file: send.file, state: 'waiting' }
            : send);
        this.#changed();
        this.#pump();
    }
    dismiss(id: string): void {
        this.#sends = this.#sends.filter(send => send.id !== id || send.state === 'waiting' || send.state === 'sending');
        this.#changed();
    }
    /**
     * The file a message showed as given. It reads as removed once this app
     * removed it, or once a list read after it was given no longer holds it:
     * so after a reload too.
     */
    given(messageId: string): GivenFile | undefined {
        const kept = this.#given.get(messageId);
        if (kept === undefined)
            return undefined;
        const state = this.#state;
        const gone = state.kind === 'read' && this.#listed > kept.after && !state.files.some(file => file.name === kept.placedAs);
        return { name: kept.name, placedAs: kept.placedAs, bytes: kept.bytes, holdsSecret: kept.holdsSecret, removed: kept.removed || gone };
    }
    current(): InboxState {
        return this.#state;
    }
    /**
     * The stream's word that a file was put: the chat shows it, and a send
     * not confirmed is settled by it. The client tells the app once the event
     * is applied; nothing is drawn from here.
     */
    observe(event: FileIn): void {
        this.#keep(event.message_id, { name: event.name, placedAs: event.placed_as, bytes: event.bytes, holdsSecret: event.holds_secret, removed: false });
        this.#sends = this.#sends.filter(send => send.uploadId !== event.message_id);
        void this.refresh();
    }
    #keep(messageId: string, file: GivenFile): void {
        const known = this.#given.get(messageId);
        this.#given.set(messageId, { ...file, removed: known?.removed ?? file.removed, after: known?.after ?? this.#reads });
    }
    /** Reads the list again; asked while a read is under way, once more after it, however often it was asked. */
    refresh(): Promise<void> {
        if (this.#reading !== null) {
            this.#again = true;
            return this.#reading;
        }
        this.#reading = this.#read().finally(() => {
            this.#reading = null;
            if (this.#again) {
                this.#again = false;
                void this.refresh();
            }
        });
        return this.#reading;
    }
    async #read(): Promise<void> {
        const read = ++this.#reads;
        const reply = await this.#client.request(this.#nextId(), { action: 'list_inbox' });
        if (reply.kind !== 'answered')
            this.#state = { kind: 'failed', detail: reply.detail };
        else if (reply.result.result === 'inbox') {
            this.#state = { kind: 'read', files: reply.result.files.filter(file => isInboxName(file.name)).map(file => ({ name: file.name, bytes: file.bytes })) };
            this.#listed = read;
        }
        else if (reply.result.result === 'file_in_refused' && reply.result.reason.refusal === 'no_room')
            this.#state = { kind: 'failed', detail: 'The agent’s computer isn’t on, so its inbox can’t be read.' };
        else
            this.#state = { kind: 'failed', detail: reply.result.result === 'refused' ? NOT_READ[reply.result.reason] : UNEXPECTED };
        this.#changed();
    }
    /** Takes a file out; the list is read again before this answers. */
    async remove(placedAs: string): Promise<CommandResult> {
        if (!isInboxName(placedAs))
            return { ok: false, message: 'That isn’t a file in the agent’s inbox.' };
        const reply = await this.#client.request(this.#nextId(), { action: 'remove_inbox_file', name: placedAs });
        if (reply.kind !== 'answered')
            return { ok: false, message: reply.detail };
        const result = reply.result;
        if (result.result === 'inbox_file_removed')
            for (const [id, file] of this.#given)
                if (file.placedAs === placedAs)
                    this.#given.set(id, { ...file, removed: true });
        await this.refresh();
        if (result.result === 'inbox_file_removed')
            return { ok: true, message: 'Removed from the agent’s inbox.' };
        if (result.result === 'file_in_refused' && result.reason.refusal === 'refused')
            return { ok: false, message: `The agent’s inbox didn’t remove it: ${sentence(result.reason.said)}.` };
        if (result.result === 'file_in_refused' && result.reason.refusal === 'no_room')
            return { ok: false, message: 'The agent’s computer isn’t on, so nothing can be removed from its inbox.' };
        return { ok: false, message: result.result === 'refused' ? NOT_REMOVED[result.reason] : UNEXPECTED };
    }
    /** Sends the next file waiting, when none is being sent. A send that throws ends as not put, and the next goes on. */
    #pump(): void {
        if (this.#busy)
            return;
        const next = this.#sends.find(send => send.state === 'waiting');
        if (next === undefined)
            return;
        this.#busy = true;
        void this.#send(next).catch((): Held => ({ id: next.id, chatId: next.chatId, name: next.name, bytes: next.bytes, file: next.file, state: 'refused', detail: `${UNEXPECTED} ${NOTHING_PUT}`, retry: true })).then(ended => {
            // A file put is let go here: the chat shows it.
            if (ended === null)
                this.#sends = this.#sends.filter(held => held.id !== next.id);
            else
                this.#replace(ended);
            this.#busy = false;
            this.#changed();
            this.#pump();
        });
    }
    #replace(send: Held): void {
        this.#sends = this.#sends.map(held => held.id === send.id ? send : held);
    }
    /** Sends `send` from its beginning to its end: how it ended, or none when it was put. */
    async #send(send: Held): Promise<Held | null> {
        const base = { id: send.id, chatId: send.chatId, name: send.name, bytes: send.bytes, file: send.file };
        const refused = (detail: string, retry: boolean): Held => ({ ...base, state: 'refused', detail, retry });
        const begun = await this.#step({ action: 'begin_file_in', chat_id: wireChat(send.chatId), name: send.name, bytes: send.bytes }, this.#waits.step);
        if (!begun.ok)
            return refused(UNANSWERED[begun.why], true);
        if (begun.result.result !== 'file_in_begun' || begun.result.part_bytes < 1)
            return this.#answered(begun.result, refused);
        const uploadId = begun.result.upload_id;
        // Never more than a part this app sends, whatever the broker says.
        const size = Math.min(begun.result.part_bytes, PART_BYTES);
        const parts = Math.ceil(send.bytes / size);
        // The app is drawn again at most about twenty times a file, not once a part.
        const every = Math.max(1, Math.ceil(parts / 20));
        for (let seq = 0; seq < parts; seq++) {
            this.#replace({ ...base, state: 'sending', sent: seq, parts, uploadId });
            if (seq % every === 0)
                this.#changed();
            let bytes: Uint8Array;
            try {
                bytes = new Uint8Array(await send.file.slice(seq * size, (seq + 1) * size).arrayBuffer());
            }
            catch {
                return refused(`The file couldn’t be read: it may have changed or moved. ${NOTHING_PUT}`, true);
            }
            const taken = await this.#step({ action: 'file_in_part', upload_id: uploadId, seq, data: base64(bytes) }, this.#waits.step);
            if (!taken.ok)
                return refused(UNANSWERED[taken.why], true);
            const result = taken.result;
            if (result.result !== 'file_in_part_taken' || result.upload_id !== uploadId || result.seq !== seq)
                return this.#answered(result, refused);
        }
        this.#replace({ ...base, state: 'sending', sent: parts, parts, uploadId });
        this.#changed();
        const unknown: Held = { ...base, state: 'unknown', detail: NOT_CONFIRMED, uploadId };
        // Once the end is sent the file may be put: any answer short of the
        // broker's word that it was, or wasn't, is not knowing. An end never
        // sent put nothing.
        const ended = await this.#step({ action: 'end_file_in', upload_id: uploadId }, this.#waits.end);
        if (!ended.ok)
            return ended.why === 'unsent' ? refused(UNANSWERED.unsent, true) : unknown;
        const result = ended.result;
        if (result.result === 'file_in_placed' && result.upload_id === uploadId) {
            const file = result.file;
            this.#keep(file.message_id, { name: file.name, placedAs: file.placed_as, bytes: file.bytes, holdsSecret: file.holds_secret, removed: false });
            void this.refresh();
            return null;
        }
        // Refused before the broker took it, or by the inbox in its words: not put.
        if (result.result === 'refused' || (result.result === 'file_in_refused' && result.reason.refusal !== 'unknown'))
            return this.#answered(result, refused);
        return unknown;
    }
    /** Asks `action` within `ms`: its answer, or why there was none. */
    async #step(action: UiAction, ms: number): Promise<Step> {
        const reply = await within(this.#client.request(this.#nextId(), action), ms);
        if (reply === 'late')
            return { ok: false, why: 'late' };
        if (reply.kind !== 'answered')
            return { ok: false, why: reply.kind === 'unsent' ? 'unsent' : 'lost' };
        return { ok: true, result: reply.result };
    }
    /** How a send ended on an answer that wasn't the next step's. */
    #answered(result: UiActionResult, refused: (detail: string, retry: boolean) => Held): Held {
        if (result.result === 'file_in_refused')
            return refused(...refusal(result.reason));
        if (result.result === 'refused')
            return refused(...REFUSED[result.reason]);
        return refused(`${UNEXPECTED} ${NOTHING_PUT}`, false);
    }
    /** Read the inbox each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh();
        this.#ready = ready;
    }
}
