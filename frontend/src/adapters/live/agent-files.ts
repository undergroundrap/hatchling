// Files the agent gives the owner, kept in Hatchling's Library (WO-041): the
// list, a download, and removal. A download begins a copy the broker read
// back to its hash, asks its parts in order, holds the broker to every bound
// it said before keeping a byte, checks the whole against the hash, and saves
// it as bytes typed application/octet-stream, whatever its name: nothing of
// it is ever shown, opened or put in the page.
import type { LiveClient } from './client.js';
import type { ActionRefusal, FileOutRefusal, UiAction, UiActionResult, UiEventBody } from './protocol/generated.js';
import type { CommandResult } from '../../protocol/models.js';
import type { AgentFile, AgentFilesPort, AgentFilesState, AgentFolder, GivenCard, LibraryItem, OfferedFile, OfferedFolder } from '../../session/agent-files.js';
import { folderPathsRefused, isGivenName, itemId, itemName, leftOutWords, runs } from '../../session/agent-files.js';
import { sizeWords } from '../../session/inbox.js';
import { within } from './inbox.js';
import type { ZipEntry } from './zip.js';
import { named, zipStored } from './zip.js';
/** The stream's word that the agent gave a file. */
export type FileOffered = Extract<UiEventBody, {
    kind: 'file_offered';
}>;
/** The stream's word that the agent gave a folder. */
export type FolderOffered = Extract<UiEventBody, {
    kind: 'folder_offered';
}>;
/** The chat's word on which run wrote each answer it shows. */
export interface ChatRuns {
    /** The run that wrote the answer `messageId`; none for any other message. */
    of(messageId: string): string | undefined;
    /** The runs whose answers the chat shows. */
    shown(): ReadonlySet<string>;
}
/** A chat that shows no answer's run. */
const NO_RUNS: ChatRuns = { of: () => undefined, shown: () => new Set() };
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
const NOT_LISTED = `Hatchling’s list of its files doesn’t match the folder. ${NOTHING_SAVED}`;
/**
 * Whether `paths` hold a git repository git would find: a `.git` anywhere,
 * or a folder holding `HEAD` with files under its `objects/` and `refs/`,
 * as a bare one has. Either way git run there reads its settings and hooks.
 */
export function holdsGit(paths: readonly string[]): boolean {
    const lower = paths.map(path => path.toLowerCase());
    if (lower.some(path => path.split('/').includes('.git')))
        return true;
    return lower
        .filter(path => path === 'head' || path.endsWith('/head'))
        .map(head => head.slice(0, -'head'.length))
        .some(folder => lower.some(path => path.startsWith(`${folder}objects/`)) && lower.some(path => path.startsWith(`${folder}refs/`)));
}
/**
 * What the owner is told of a .zip saved: its name, files and size; what
 * opening its files can do, by each one's extension; a git repository in
 * it, which git acts on; paths too long for Windows' Explorer to unpack far down; and what
 * each folder's walk left out.
 */
function savedWords(name: string, entries: readonly ZipEntry[], size: number, leftOut: readonly string[]): string {
    const running = entries.filter(entry => runs(entry.name) !== null).length;
    const caution = running === 0
        ? 'The agent made them: open them only if you trust them.'
        : `${running} of them can run something when opened: open them only if you trust what the agent made.`;
    const git = holdsGit(entries.map(entry => entry.name))
        ? ' It holds a git repository: git run in it can run that repository’s hooks and settings, so look at them before you use git there.'
        : '';
    const long = entries.filter(entry => entry.name.length >= LONG_PATH).length;
    const far = long === 0 ? '' : ` ${long === 1 ? '1 path in it is' : `${long} paths in it are`} long: unpack it near the top of a drive, or Windows’ Explorer may not unpack ${long === 1 ? 'it' : 'them'}.`;
    const files = entries.length === 1 ? '1 file' : `${entries.length} files`;
    return [`Gave your browser ${name}, ${files}, ${sizeWords(size)}, to save. ${caution}${git}${far}`, ...leftOut].join(' ');
}
/** Saves `parts`, one after another, as one file named `name`. */
export type Saver = (parts: readonly Uint8Array[], name: string) => void;
/** The SHA-256 of `bytes`, in lowercase hex. */
export type Digest = (bytes: Uint8Array) => Promise<string>;
/**
 * Saves `parts` as one download named `name`, typed as bytes whatever the
 * name says, so the browser never opens it; never put together in the page
 * first. The address is let go a minute later: letting go at once can
 * cancel the save.
 */
export function saveBytes(parts: readonly Uint8Array[], name: string): void {
    const url = URL.createObjectURL(new Blob([...parts], { type: 'application/octet-stream' }));
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
    not_a_folder: 'Hatchling answered for a file as if it were a folder.',
    a_folder: 'Hatchling answered for a folder as if it were a file.',
    out_of_range: 'Hatchling answered for a file its folder doesn’t hold.',
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
    readonly conversation: number;
    readonly reply?: string | null;
}): AgentFile {
    return { id: file.id, name: file.name, bytes: file.bytes, sha256: file.sha256, keptAt: file.kept_at, reply: file.reply ?? null, conversation: file.conversation };
}
/** A folder as the stream or the list says it. */
function keptFolder(folder: {
    readonly id: string;
    readonly name: string;
    readonly files: number;
    readonly bytes: number;
    readonly sha256: string;
    readonly kept_at: string;
    readonly conversation: number;
    readonly reply?: string | null;
    readonly left_out: {
        readonly links: number;
        readonly mounts: number;
        readonly others: number;
    };
}): AgentFolder {
    const { links, mounts, others } = folder.left_out;
    return { id: folder.id, name: folder.name, files: folder.files, bytes: folder.bytes, sha256: folder.sha256, leftOut: { links, mounts, others }, keptAt: folder.kept_at, reply: folder.reply ?? null, conversation: folder.conversation };
}
/** How many entries a page of a folder's list holds, but the last. */
const FOLDER_PAGE = 32;
/** A SHA-256 as the broker gives one: 64 lowercase hex digits. */
const SHA256 = /^[0-9a-f]{64}$/u;
/** Paths at least this long can be more than Windows' Explorer unpacks, with the folder it unpacks into. */
const LONG_PATH = 200;
/** What a download saves of one file: its name and bytes. */
interface Read {
    readonly name: string;
    readonly bytes: Uint8Array;
}
/** An id the broker gives an item: 64 lowercase hex digits. */
const ITEM_ID = /^[0-9a-f]{64}$/u;
/** The most bytes a file the agent gives holds. */
const FILE_BYTES = 8 * 1024 * 1024;
/** Whether `file` is one the broker gives: its name as a file's, its size within a file's. */
function isShownFile(file: {
    readonly name: string;
    readonly bytes: number;
}): boolean {
    return isGivenName(file.name) && file.bytes <= FILE_BYTES;
}
/** The most files, and bytes, a folder the agent gives holds. */
const FOLDER_FILES = 512;
const FOLDER_BYTES = 32 * 1024 * 1024;
/** Whether `folder` is one the broker gives: its name as a file's, its counts within a folder's. */
function isShownFolder(folder: {
    readonly name: string;
    readonly files: number;
    readonly bytes: number;
}): boolean {
    return isGivenName(folder.name) && folder.files >= 1 && folder.files <= FOLDER_FILES && folder.bytes <= FOLDER_BYTES;
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
/** A folder a message showed, and the read of the list it was first known before. */
type OfferedDir = AgentFolder & {
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
    readonly #offeredFolders = new Map<string, OfferedDir>();
    readonly #runs: ChatRuns;
    readonly #downloading = new Set<string>();
    /** The items the owner has chosen to download together. */
    readonly #chosen = new Set<string>();
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
    constructor(client: LiveClient, changed: () => void, nextId: () => string, { save = saveBytes, digest = browserDigest(), wait = STEP_WAIT_MS, runs = NO_RUNS }: {
        readonly save?: Saver;
        readonly digest?: Digest | null;
        readonly wait?: number;
        readonly runs?: ChatRuns;
    } = {}) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        this.#runs = runs;
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
     * The stream's word that the agent gave a folder: kept by its message,
     * and the list read again. One whose name the broker never gives is not kept.
     */
    observeFolder(event: FolderOffered): void {
        if (!isShownFolder(event.folder))
            return;
        const known = this.#offeredFolders.get(event.message_id);
        this.#offeredFolders.set(event.message_id, { ...keptFolder(event.folder), removed: known?.removed ?? false, after: known?.after ?? this.#reads });
        void this.refresh();
    }
    /** The folder a message showed as given, removed once a list read after it no longer holds it. */
    offeredFolder(messageId: string): OfferedFolder | undefined {
        const folder = this.#offeredFolders.get(messageId);
        if (folder === undefined)
            return undefined;
        const state = this.#state;
        const gone = state.kind === 'read' && this.#listed > folder.after && !state.folders.some(listed => listed.id === folder.id);
        const { after: _after, ...shown } = folder;
        return { ...shown, removed: folder.removed || gone };
    }
    /** The ids a message of the stream showed: they aren't shown again. */
    #noticed(): ReadonlySet<string> {
        return new Set([...[...this.#offered.values()].map(file => file.id), ...[...this.#offeredFolders.values()].map(folder => folder.id)]);
    }
    /** What the list says the reply `run` gave, unless a message of the stream showed it. */
    #cards(keep: (reply: string) => boolean): GivenCard[] {
        const state = this.#state;
        if (state.kind !== 'read')
            return [];
        const noticed = this.#noticed();
        const fresh = (item: {
            readonly id: string;
            readonly reply: string | null;
            readonly conversation: number;
        }): boolean => item.reply !== null && keep(item.reply) && !noticed.has(item.id);
        return [
            ...state.folders.filter(fresh).map((folder): GivenCard => ({ kind: 'folder', folder })),
            ...state.files.filter(fresh).map((file): GivenCard => ({ kind: 'file', file })),
            ...state.removed.filter(gone => keep(gone.reply) && !noticed.has(gone.id)).map((removed): GivenCard => ({ kind: 'removed', removed })),
        ];
    }
    givenIn(messageId: string): readonly GivenCard[] {
        const run = this.#runs.of(messageId);
        return run === undefined ? [] : this.#cards(reply => reply === run);
    }
    givenElsewhere(): readonly GivenCard[] {
        const shown = this.#runs.shown();
        const state = this.#state;
        if (state.kind !== 'read')
            return [];
        const open = state.standing.conversation;
        const ours = new Set([...state.files, ...state.folders].filter(item => item.conversation === open).map(item => item.id));
        return this.#cards(reply => !shown.has(reply)).filter(card => card.kind === 'removed' || ours.has(card.kind === 'file' ? card.file.id : card.folder.id));
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
    downloadProgress(): {
        readonly done: number;
        readonly total: number;
    } | null {
        return this.#progress;
    }
    chosen(): ReadonlySet<string> {
        const listed = new Set(this.#items().map(itemId));
        return new Set([...this.#chosen].filter(id => listed.has(id)));
    }
    choose(id: string, on: boolean): void {
        if (on)
            this.#chosen.add(id);
        else
            this.#chosen.delete(id);
        this.#changed();
    }
    /** Every item the Library was last listed with: folders, then files, each newest first. */
    #items(): LibraryItem[] {
        const state = this.#state;
        if (state.kind !== 'read')
            return [];
        return [...state.folders.map((folder): LibraryItem => ({ kind: 'folder', folder })), ...state.files.map((file): LibraryItem => ({ kind: 'file', file }))];
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
            const { files, folders, removed, standing } = reply.result;
            // An item it can't name is shown all the same, by its size, so
            // nothing the Library counts is out of the owner's sight.
            const unnamed = [...files.filter(file => !isShownFile(file)), ...folders.filter(folder => !isShownFolder(folder))]
                .map(item => ({ id: item.id, bytes: item.bytes, removable: ITEM_ID.test(item.id) }));
            this.#state = {
                kind: 'read',
                files: files.filter(isShownFile).map(kept),
                folders: folders.filter(isShownFolder).map(keptFolder),
                unnamed,
                removed: removed.filter(gone => isGivenName(gone.name) && ITEM_ID.test(gone.id)).map(gone => ({ id: gone.id, name: gone.name, folder: gone.folder, files: gone.files, bytes: gone.bytes, reply: gone.reply })),
                standing: { files: standing.files, fileLimit: standing.file_limit, bytes: standing.bytes, byteLimit: standing.byte_limit, given: standing.given, givenLimit: standing.given_limit, off: standing.off, filesInAll: standing.files_in_all, filesInAllLimit: standing.files_in_all_limit, read: standing.read, readLimit: standing.read_limit, conversation: standing.conversation },
            };
            this.#listed = read;
        }
        else
            this.#state = { kind: 'failed', detail: reply.result.result === 'refused' ? NOT_READ[reply.result.reason] : NOT_READ.bad_request };
        this.#changed();
    }
    /**
     * Downloads one item: a file as itself, a folder as one .zip. One
     * download at a time: the broker holds one a connection, and a second
     * would let the first go.
     */
    async download(id: string): Promise<CommandResult> {
        if (this.#downloading.has(id))
            return { ok: false, message: 'It is already downloading.' };
        if (this.#downloading.size > 0)
            return { ok: false, message: 'Another file is downloading. Try again when it’s done.' };
        const digest = this.#digest;
        if (digest === null)
            return { ok: false, message: NO_DIGEST };
        const folder = this.#folder(id);
        if (folder !== undefined)
            return this.#archive([{ kind: 'folder', folder }], `${folder.name}.zip`, id, digest);
        this.#downloading.add(id);
        this.#changed();
        // Whatever throws on the way, nothing was saved, and the owner is told.
        const result = await this.#saveOne(id, digest).catch((): CommandResult => ({ ok: false, message: `The file couldn’t be downloaded. ${NOTHING_SAVED} Try again.` }));
        this.#downloading.delete(id);
        this.#changed();
        return result;
    }
    /** The folder `id`, as the list or a message of the stream said it. */
    #folder(id: string): AgentFolder | undefined {
        const state = this.#state;
        return (state.kind === 'read' ? state.folders.find(folder => folder.id === id) : undefined)
            ?? [...this.#offeredFolders.values()].find(folder => folder.id === id);
    }
    /** Reads the file `id`, checked, and gives the browser it to save. */
    async #saveOne(id: string, digest: Digest): Promise<CommandResult> {
        const read = await this.#read(id, digest);
        if (!('bytes' in read))
            return read;
        this.#save([read.bytes], read.name);
        return { ok: true, message: `Gave your browser ${read.name}, ${sizeWords(read.bytes.length)}, to save. The agent made it: open it only if you trust it.` };
    }
    /**
     * Downloads the items `ids`, of those the Library was last listed with,
     * in that order, as one .zip named `name`. Any that fails saves nothing.
     */
    async downloadMany(ids: readonly string[], name: string): Promise<CommandResult> {
        if (this.#downloading.size > 0)
            return { ok: false, message: 'A download is under way. Try again when it’s done.' };
        const digest = this.#digest;
        if (digest === null)
            return { ok: false, message: NO_DIGEST };
        const listed = this.#items();
        const items = [...new Set(ids)].flatMap(id => listed.filter(item => itemId(item) === id));
        if (items.length === 0)
            return { ok: false, message: 'Nothing chosen is in Hatchling’s Library now. Refresh it, then choose again.' };
        return this.#archive(items, name, ALL, digest);
    }
    /**
     * Downloads every item the Library was last listed with, as one .zip:
     * each file read out and checked against its hash as a download is, one
     * at a time, then saved once. Any that fails saves nothing.
     */
    async downloadAll(): Promise<CommandResult> {
        if (this.#downloading.size > 0)
            return { ok: false, message: 'A download is under way. Try again when it’s done.' };
        const digest = this.#digest;
        if (digest === null)
            return { ok: false, message: NO_DIGEST };
        if (this.#state.kind !== 'read')
            return { ok: false, message: 'Hatchling’s Library hasn’t been read yet. Refresh it, then try again.' };
        const items = this.#items();
        if (items.length === 0)
            return { ok: false, message: 'Hatchling’s Library holds nothing to download.' };
        return this.#archive(items, ALL_NAME, ALL, digest);
    }
    /** Downloads `items` as one .zip named `name`, held under `marker` while it runs, its files counted as they come. */
    async #archive(items: readonly LibraryItem[], name: string, marker: string, digest: Digest): Promise<CommandResult> {
        const total = items.reduce((all, item) => all + (item.kind === 'file' ? 1 : item.folder.files), 0);
        let done = 0;
        const tick = (): void => {
            this.#progress = { done: ++done, total };
            this.#changed();
        };
        this.#downloading.add(marker);
        this.#progress = { done, total };
        this.#changed();
        // Whatever throws on the way, nothing was saved, and the owner is told.
        const result = await this.#saveArchive(items, name, digest, tick).catch((): CommandResult => ({ ok: false, message: `The files couldn’t be put together: too many for one .zip, or too much for this page’s memory. ${NOTHING_SAVED} Choose fewer, or download them one at a time.` }));
        this.#downloading.delete(marker);
        this.#progress = null;
        this.#changed();
        return result;
    }
    /**
     * Reads each of `items`, checked, one file at a time, and gives the
     * browser them as one .zip: each file by its name, each folder's files
     * under the folder's name, the names at the top made distinct; `tick`
     * as each file is read.
     */
    async #saveArchive(items: readonly LibraryItem[], name: string, digest: Digest, tick: () => void): Promise<CommandResult> {
        const entries: ZipEntry[] = [];
        const leftOut: string[] = [];
        for (const { item, name: top } of named(items, itemName)) {
            if (item.kind === 'file') {
                const read = await this.#read(item.file.id, digest);
                if (!('bytes' in read))
                    return { ok: false, message: `${item.file.name}: ${read.message}` };
                entries.push({ name: top, bytes: read.bytes, at: item.file.keptAt });
                tick();
                continue;
            }
            const read = await this.#readFolder(item.folder, digest, tick);
            if (!Array.isArray(read))
                return { ok: false, message: `${item.folder.name}: ${read.message}` };
            entries.push(...read.map(file => ({ name: `${top}/${file.name}`, bytes: file.bytes, at: item.folder.keptAt })));
            const words = leftOutWords(item.folder.leftOut);
            if (words !== '')
                leftOut.push(`${item.folder.name}: ${words}`);
        }
        const zip = zipStored(entries);
        this.#save(zip, name);
        return { ok: true, message: savedWords(name, entries, zip.reduce((all, part) => all + part.length, 0), leftOut) };
    }
    /**
     * Reads the folder `folder` out of the Library: its list, page by page,
     * held to the folder the list said, every path checked here and the
     * whole list to its hash; then each file, held to its entry and checked
     * against its hash, `tick` as each is. Each file's name is its path in
     * the folder.
     */
    async #readFolder(folder: AgentFolder, digest: Digest, tick: () => void): Promise<Read[] | CommandResult> {
        const list: {
            readonly path: string;
            readonly bytes: number;
            readonly sha256: string;
        }[] = [];
        while (list.length < folder.files) {
            const from = list.length;
            const page = await this.#step({ action: 'list_folder', id: folder.id, from });
            if (typeof page === 'string')
                return { ok: false, message: page };
            if (page.result !== 'folder')
                return this.#refused(page);
            const size = Math.min(FOLDER_PAGE, folder.files - from);
            if (page.id !== folder.id || page.from !== from || page.total !== folder.files || page.entries.length !== size)
                return { ok: false, message: UNEXPECTED };
            list.push(...page.entries.map(entry => ({ path: entry.path, bytes: entry.bytes, sha256: entry.sha256 })));
        }
        const why = folderPathsRefused(list.map(entry => entry.path));
        if (why !== null)
            return { ok: false, message: `A path in it isn’t one Hatchling keeps (${why}). ${NOTHING_SAVED}` };
        // Each entry's length is a whole number of bytes: the wire's checks
        // hold that. The list as the Library keeps it, and hashes it: its entries' JSON,
        // each its path, length and hash, in that order. Its paths are plain
        // ASCII without " or \, so this is the Library's own bytes.
        const sized = list.every(entry => SHA256.test(entry.sha256) && entry.bytes <= FILE_OUT_BYTES)
            && list.reduce((all, entry) => all + entry.bytes, 0) === folder.bytes;
        if (!sized || await digest(new TextEncoder().encode(JSON.stringify(list))) !== folder.sha256)
            return { ok: false, message: NOT_LISTED };
        const read: Read[] = [];
        for (const [file, entry] of list.entries()) {
            const begun = await this.#step({ action: 'begin_file_out', id: folder.id, file });
            if (typeof begun === 'string')
                return { ok: false, message: begun };
            if (begun.result !== 'folder_file_out_begun')
                return this.#refused(begun);
            const bounded = begun.id === folder.id && begun.file === file && begun.path === entry.path && begun.bytes === entry.bytes
                && begun.sha256 === entry.sha256 && begun.parts === Math.ceil(entry.bytes / PART_BYTES);
            if (!bounded)
                return { ok: false, message: UNEXPECTED };
            const bytes = await this.#parts(begun.download_id, entry.bytes, entry.sha256, begun.parts, digest);
            if (!(bytes instanceof Uint8Array))
                return bytes;
            read.push({ name: entry.path, bytes });
            tick();
        }
        return read;
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
    async #read(id: string, digest: Digest): Promise<Read | CommandResult> {
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
        const bytes = await this.#parts(downloadId, file.bytes, file.sha256, parts, digest);
        return bytes instanceof Uint8Array ? { name: file.name, bytes } : bytes;
    }
    /**
     * Reads the `parts` of the download `downloadId`, in order, held to
     * `length` bytes, and checks the whole against `sha256`: the bytes, or
     * how it ended without them.
     */
    async #parts(downloadId: string, length: number, sha256: string, parts: number, digest: Digest): Promise<Uint8Array | CommandResult> {
        const bytes = new Uint8Array(length);
        let at = 0;
        for (let seq = 0; seq < parts; seq++) {
            const read = await this.#step({ action: 'file_out_part', download_id: downloadId, seq });
            if (typeof read === 'string')
                return { ok: false, message: read };
            if (read.result !== 'file_out_part_read')
                return this.#refused(read);
            const part = read.download_id === downloadId && read.seq === seq ? decoded(read.data) : null;
            if (part === null || part.length > PART_BYTES || at + part.length > length)
                return { ok: false, message: NOT_WHOLE };
            bytes.set(part, at);
            at += part.length;
        }
        if (at !== length || await digest(bytes) !== sha256)
            return { ok: false, message: NOT_WHOLE };
        return bytes;
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
            this.#chosen.delete(id);
        if (removed)
            for (const [messageId, folder] of this.#offeredFolders)
                if (folder.id === id)
                    this.#offeredFolders.set(messageId, { ...folder, removed: true });
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
