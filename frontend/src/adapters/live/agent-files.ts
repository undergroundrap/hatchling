// Files the agent gives the owner, kept in Hatchling's Library (WO-041): the
// list, a download, and removal. A download begins a copy the broker read
// back to its hash, asks its parts in order, holds the broker to every bound
// it said before keeping a byte, checks the whole against the hash, and saves
// it as bytes typed application/octet-stream, whatever its name: nothing of
// it is ever shown, opened or put in the page.
import type { LiveClient } from './client.js';
import type { ActionRefusal, FileOutRefusal, UiAction, UiActionResult, UiEventBody } from './protocol/generated.js';
import type { CommandResult } from '../../protocol/models.js';
import type { AgentFile, AgentFilesPort, AgentFilesState, OfferedFile } from '../../session/agent-files.js';
import { isGivenName, runs } from '../../session/agent-files.js';
import { sizeWords } from '../../session/inbox.js';
import { within } from './inbox.js';
import { zipStored } from './zip.js';
/** The stream's word that the agent gave a file. */
export type FileOffered = Extract<UiEventBody, {
    kind: 'file_offered';
}>;
/** The most bytes a file from the agent holds: the Library's bound for one file. */
export const FILE_OUT_BYTES = 8 * 1024 * 1024;
/** The most bytes a part carries. */
export const PART_BYTES = 32 * 1024;
/** How long each step is waited for. */
const STEP_WAIT_MS = 60000;
/** The name every file at once is saved by. */
export const ALL_NAME = 'hatchling-library.zip';
/** What `#downloading` holds while every file is downloading at once. */
const ALL = '*';
/** How long a saved file's address lives: long enough for the browser to take it. */
const SAVE_URL_MS = 60000;
const NOTHING_SAVED = 'Nothing was saved.';
const UNEXPECTED = `Hatchling answered with something else. ${NOTHING_SAVED}`;
const NOT_WHOLE = `The file didn’t arrive whole. ${NOTHING_SAVED} Try again.`;
const NO_DIGEST = `This page can’t check a file’s hash, so it can’t download one. ${NOTHING_SAVED} Open Hatchling at its own address.`;
/** Saves `bytes` as a file named `name`. */
export type Saver = (bytes: Uint8Array, name: string) => void;
/** The SHA-256 of `bytes`, in lowercase hex. */
export type Digest = (bytes: Uint8Array) => Promise<string>;
/**
 * Saves `bytes` as a download named `name`, typed as bytes whatever the name
 * says, so the browser never opens it. The address is let go a minute later:
 * letting go at once can cancel the save.
 */
export function saveBytes(bytes: Uint8Array, name: string): void {
    const url = URL.createObjectURL(new Blob([bytes], { type: 'application/octet-stream' }));
    const link = document.createElement('a');
    link.setAttribute('href', url);
    link.setAttribute('download', name);
    document.body.append(link);
    link.click();
    link.remove();
    setTimeout(() => URL.revokeObjectURL(url), SAVE_URL_MS);
}
/** The browser's SHA-256, or none where it has no WebCrypto (an insecure page). */
export function browserDigest(): Digest | null {
    const subtle = globalThis.crypto?.subtle;
    if (subtle === undefined)
        return null;
    return async (bytes) => [...new Uint8Array(await subtle.digest('SHA-256', bytes))].map(byte => byte.toString(16).padStart(2, '0')).join('');
}
/** Why a file wasn't read out, in the owner's words. */
const OUT_REFUSED: Readonly<Record<FileOutRefusal['refusal'], string>> = {
    gone: 'It isn’t in Hatchling’s Library any more.',
    damaged: `Hatchling’s copy of it is damaged. ${NOTHING_SAVED} Remove it, and ask the agent for it again.`,
    busy: `Other downloads are under way. ${NOTHING_SAVED} Try again in a minute.`,
    unknown_download: NOT_WHOLE,
    out_of_step: NOT_WHOLE,
};
/** Why the API refused, before the broker read anything, for each thing asked. */
const NOT_READ: Readonly<Record<ActionRefusal, string>> = {
    not_granted: 'This connection may not read Hatchling’s Library.',
    bad_request: 'Hatchling answered with something else.',
    unavailable: 'Hatchling couldn’t read its Library.',
    not_connected: 'Not connected.',
};
const NOT_DOWNLOADED: Readonly<Record<ActionRefusal, string>> = {
    not_granted: 'This connection may not download from Hatchling’s Library.',
    bad_request: UNEXPECTED,
    unavailable: `Hatchling couldn’t read its Library. ${NOTHING_SAVED}`,
    not_connected: `Not connected. ${NOTHING_SAVED}`,
};
const NOT_REMOVED: Readonly<Record<ActionRefusal, string>> = {
    not_granted: 'This connection may not remove files from Hatchling’s Library.',
    bad_request: 'Hatchling answered with something else.',
    unavailable: 'Hatchling couldn’t remove it.',
    not_connected: 'Not connected.',
};
/** A file of the wire, as the app keeps it. */
function kept(file: {
    readonly id: string;
    readonly name: string;
    readonly bytes: number;
    readonly sha256: string;
    readonly kept_at: string;
}): AgentFile {
    return { id: file.id, name: file.name, bytes: file.bytes, sha256: file.sha256, keptAt: file.kept_at };
}
/** `data` as bytes; none when it isn't base64. */
function decoded(data: string): Uint8Array | null {
    try {
        return Uint8Array.from(atob(data), char => char.charCodeAt(0));
    }
    catch {
        return null;
    }
}
/** A file a message showed, and the read of the list it was first known before. */
type Offered = AgentFile & {
    readonly removed: boolean;
    readonly after: number;
};
export class LiveAgentFiles implements AgentFilesPort {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    readonly #save: Saver;
    readonly #digest: Digest | null;
    readonly #wait: number;
    #state: AgentFilesState = { kind: 'unread' };
    readonly #offered = new Map<string, Offered>();
    readonly #downloading = new Set<string>();
    /** How far every file's download has got, while it runs. */
    #progress: {
        readonly done: number;
        readonly total: number;
    } | null = null;
    #ready = false;
    /** Reads begun, the last whose answer was taken, and the last whose list was. */
    #reads = 0;
    #taken = 0;
    #listed = 0;
    constructor(client: LiveClient, changed: () => void, nextId: () => string, { save = saveBytes, digest = browserDigest(), wait = STEP_WAIT_MS }: {
        readonly save?: Saver;
        readonly digest?: Digest | null;
        readonly wait?: number;
    } = {}) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        this.#save = save;
        this.#digest = digest;
        this.#wait = wait;
        client.subscribe(() => this.#watch());
    }
    current(): AgentFilesState {
        return this.#state;
    }
    /**
     * The stream's word that the agent gave a file: kept by its message, and
     * the list read again. One whose name the broker never gives is not kept.
     */
    observe(event: FileOffered): void {
        if (!isGivenName(event.file.name))
            return;
        const known = this.#offered.get(event.message_id);
        this.#offered.set(event.message_id, { ...kept(event.file), removed: known?.removed ?? false, after: known?.after ?? this.#reads });
        void this.refresh();
    }
    /**
     * The file a message showed as given. It reads as removed once this app
     * removed it, or once a list read after it was given no longer holds it.
     */
    offered(messageId: string): OfferedFile | undefined {
        const file = this.#offered.get(messageId);
        if (file === undefined)
            return undefined;
        const state = this.#state;
        const gone = state.kind === 'read' && this.#listed > file.after && !state.files.some(listed => listed.id === file.id);
        const { after: _after, ...shown } = file;
        return { ...shown, removed: file.removed || gone };
    }
    downloading(id: string): boolean {
        return this.#downloading.has(id) || this.#downloading.has(ALL);
    }
    downloadingAll(): {
        readonly done: number;
        readonly total: number;
    } | null {
        return this.#progress;
    }
    async refresh(): Promise<void> {
        const read = ++this.#reads;
        const reply = await this.#client.request(this.#nextId(), { action: 'list_library' });
        // An answer older than one already taken says nothing newer.
        if (read < this.#taken)
            return;
        this.#taken = read;
        if (reply.kind !== 'answered')
            this.#state = { kind: 'failed', detail: reply.detail };
        else if (reply.result.result === 'library') {
            const standing = reply.result.standing;
            this.#state = {
                kind: 'read',
                files: reply.result.files.filter(file => isGivenName(file.name)).map(kept),
                standing: { files: standing.files, fileLimit: standing.file_limit, bytes: standing.bytes, byteLimit: standing.byte_limit, given: standing.given, givenLimit: standing.given_limit, off: standing.off },
            };
            this.#listed = read;
        }
        else
            this.#state = { kind: 'failed', detail: reply.result.result === 'refused' ? NOT_READ[reply.result.reason] : NOT_READ.bad_request };
        this.#changed();
    }
    /**
     * Downloads one file at a time: the broker holds one download a
     * connection, and a second would let the first go.
     */
    async download(id: string): Promise<CommandResult> {
        if (this.#downloading.has(id))
            return { ok: false, message: 'It is already downloading.' };
        if (this.#downloading.size > 0)
            return { ok: false, message: 'Another file is downloading. Try again when it’s done.' };
        const digest = this.#digest;
        if (digest === null)
            return { ok: false, message: NO_DIGEST };
        this.#downloading.add(id);
        this.#changed();
        // Whatever throws on the way, nothing was saved, and the owner is told.
        const result = await this.#saveOne(id, digest).catch((): CommandResult => ({ ok: false, message: `The file couldn’t be downloaded. ${NOTHING_SAVED} Try again.` }));
        this.#downloading.delete(id);
        this.#changed();
        return result;
    }
    /** Reads the file `id`, checked, and gives the browser it to save. */
    async #saveOne(id: string, digest: Digest): Promise<CommandResult> {
        const read = await this.#read(id, digest);
        if (!('bytes' in read))
            return read;
        this.#save(read.bytes, read.name);
        return { ok: true, message: `Gave your browser ${read.name}, ${sizeWords(read.bytes.length)}, to save. The agent made it: open it only if you trust it.` };
    }
    /**
     * Downloads every file the Library was last listed with, as one .zip:
     * each read out and checked against its hash as a download is, one at
     * a time, then saved once. Any that fails saves nothing.
     */
    async downloadAll(): Promise<CommandResult> {
        if (this.#downloading.size > 0)
            return { ok: false, message: 'A download is under way. Try again when it’s done.' };
        const digest = this.#digest;
        if (digest === null)
            return { ok: false, message: NO_DIGEST };
        const state = this.#state;
        if (state.kind !== 'read')
            return { ok: false, message: 'Hatchling’s Library hasn’t been read yet. Refresh it, then try again.' };
        if (state.files.length === 0)
            return { ok: false, message: 'Hatchling’s Library holds no files to download.' };
        const files = state.files;
        this.#downloading.add(ALL);
        this.#progress = { done: 0, total: files.length };
        this.#changed();
        // Whatever throws on the way, nothing was saved, and the owner is told.
        const result = await this.#saveAll(files, digest).catch((): CommandResult => ({ ok: false, message: `The files couldn’t be put together, perhaps for want of memory. ${NOTHING_SAVED} Download them one at a time.` }));
        this.#downloading.delete(ALL);
        this.#progress = null;
        this.#changed();
        return result;
    }
    /** Reads each of `files`, checked, one at a time, and gives the browser them as one .zip. */
    async #saveAll(files: readonly AgentFile[], digest: Digest): Promise<CommandResult> {
        const entries = [];
        for (const file of files) {
            const read = await this.#read(file.id, digest);
            if (!('bytes' in read))
                return { ok: false, message: `${file.name}: ${read.message}` };
            entries.push({ name: read.name, bytes: read.bytes, at: file.keptAt });
            this.#progress = { done: entries.length, total: files.length };
            this.#changed();
        }
        const zip = zipStored(entries);
        this.#save(zip, ALL_NAME);
        const running = entries.filter(entry => runs(entry.name) !== null).length;
        const caution = running === 0
            ? 'The agent made them: open them only if you trust them.'
            : `${running} of them can run something when opened, as Settings says of each: open them only if you trust what the agent made.`;
        return { ok: true, message: `Gave your browser ${ALL_NAME}, ${entries.length} files, ${sizeWords(zip.length)}, to save. ${caution}` };
    }
    /** The hash this app was told for the file `id`, by its list or a message. */
    #known(id: string): string | undefined {
        const state = this.#state;
        const listed = state.kind === 'read' ? state.files.find(file => file.id === id) : undefined;
        return listed?.sha256 ?? [...this.#offered.values()].find(file => file.id === id)?.sha256;
    }
    /**
     * Reads the file `id` out of the Library: its name and bytes, held to
     * every bound the broker said and checked against its hash; or how it
     * ended without them.
     */
    async #read(id: string, digest: Digest): Promise<{
        readonly name: string;
        readonly bytes: Uint8Array;
    } | CommandResult> {
        const begun = await this.#step({ action: 'begin_file_out', id });
        if (typeof begun === 'string')
            return { ok: false, message: begun };
        if (begun.result !== 'file_out_begun')
            return this.#refused(begun);
        const { download_id: downloadId, file, parts } = begun;
        const known = this.#known(id);
        // The broker is held to every bound before a byte is kept; that a
        // length is a whole number of bytes, the wire's checks already hold.
        const bounded = file.id === id && isGivenName(file.name) && file.bytes <= FILE_OUT_BYTES
            && parts === Math.ceil(file.bytes / PART_BYTES) && (known === undefined || known === file.sha256);
        if (!bounded)
            return { ok: false, message: UNEXPECTED };
        const bytes = new Uint8Array(file.bytes);
        let at = 0;
        for (let seq = 0; seq < parts; seq++) {
            const read = await this.#step({ action: 'file_out_part', download_id: downloadId, seq });
            if (typeof read === 'string')
                return { ok: false, message: read };
            if (read.result !== 'file_out_part_read')
                return this.#refused(read);
            const part = read.download_id === downloadId && read.seq === seq ? decoded(read.data) : null;
            if (part === null || part.length > PART_BYTES || at + part.length > file.bytes)
                return { ok: false, message: NOT_WHOLE };
            bytes.set(part, at);
            at += part.length;
        }
        if (at !== file.bytes || await digest(bytes) !== file.sha256)
            return { ok: false, message: NOT_WHOLE };
        return { name: file.name, bytes };
    }
    /** Asks `action`: its answer, or what to say when there was none. */
    async #step(action: UiAction): Promise<UiActionResult | string> {
        const reply = await within(this.#client.request(this.#nextId(), action), this.#wait);
        if (reply === 'late')
            return `Hatchling didn’t answer in time. ${NOTHING_SAVED}`;
        if (reply.kind !== 'answered')
            return `${reply.detail} ${NOTHING_SAVED}`;
        return reply.result;
    }
    /** How a download ended on an answer other than its next step's. */
    #refused(result: UiActionResult): CommandResult {
        if (result.result === 'file_out_refused') {
            if (result.reason.refusal === 'gone')
                void this.refresh();
            return { ok: false, message: OUT_REFUSED[result.reason.refusal] };
        }
        return { ok: false, message: result.result === 'refused' ? NOT_DOWNLOADED[result.reason] : UNEXPECTED };
    }
    /** Removes the file `id`; the list is read again before this answers. */
    async remove(id: string): Promise<CommandResult> {
        const reply = await this.#client.request(this.#nextId(), { action: 'remove_library_file', id });
        if (reply.kind !== 'answered')
            return { ok: false, message: reply.detail };
        const result = reply.result;
        const removed = result.result === 'library_file_removed' || (result.result === 'file_out_refused' && result.reason.refusal === 'gone');
        if (removed)
            for (const [messageId, file] of this.#offered)
                if (file.id === id)
                    this.#offered.set(messageId, { ...file, removed: true });
        await this.refresh();
        if (result.result === 'library_file_removed')
            return { ok: true, message: 'Removed from Hatchling’s Library.' };
        if (removed)
            return { ok: true, message: 'It was already removed.' };
        return { ok: false, message: result.result === 'refused' ? NOT_REMOVED[result.reason] : NOT_REMOVED.bad_request };
    }
    /** Read the Library each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh();
        this.#ready = ready;
    }
}
