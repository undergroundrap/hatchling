// Files the agent gives the owner (WO-041): kept in Hatchling's Library,
// listed, downloaded and removed. A download is the Library's copy, read in
// parts and checked against its hash, saved as bytes only: never shown,
// opened or put in the page. Optional frontend capability: only a live
// install has it. NOT an addition to the frozen wire protocol.
import type { CommandResult } from '../protocol/models.js';
import { isLibraryName } from './inbox.js';
/** A file the Library keeps, as the broker lists it. */
export interface AgentFile {
    readonly id: string;
    /** The broker's name for it: plain ASCII, made from the file's own. */
    readonly name: string;
    readonly bytes: number;
    readonly sha256: string;
    /** When it was kept, as RFC 3339. */
    readonly keptAt: string;
    /** The reply it came from, by its run's cell id; none for one kept before replies were. */
    readonly reply: string | null;
    /** The conversation it came from, by the Library's number. */
    readonly conversation: number;
}
/** What a folder's walk left out when it was given, by kind. */
export interface LeftOut {
    readonly links: number;
    /** Other drives: the agent's inbox among them. */
    readonly mounts: number;
    /** Devices, pipes and sockets. */
    readonly others: number;
}
/** A folder the Library keeps (WO-043), as the broker lists it. */
export interface AgentFolder {
    readonly id: string;
    /** The broker's name for it: plain ASCII, made from the folder's own. */
    readonly name: string;
    /** How many files it holds. */
    readonly files: number;
    readonly bytes: number;
    /** The SHA-256 of its list, as the Library keeps it: what its files are checked by. */
    readonly sha256: string;
    /** What its walk left out. */
    readonly leftOut: LeftOut;
    /** When it was kept, as RFC 3339. */
    readonly keptAt: string;
    /** The reply it came from, by its run's cell id; none for one kept before replies were. */
    readonly reply: string | null;
    /** The conversation it came from, by the Library's number. */
    readonly conversation: number;
}
/**
 * An item the Library keeps whose name, or size, isn't one the broker
 * gives: it is counted, so it is shown, by its size alone, to be removed.
 */
export interface AgentUnnamed {
    readonly id: string;
    readonly bytes: number;
    /** Whether its id is one the broker gives, so it can be removed from here. */
    readonly removable: boolean;
}
/** What was removed that the open conversation gave: its chat says so. */
export interface AgentRemoved {
    readonly id: string;
    readonly name: string;
    readonly folder: boolean;
    readonly files: number;
    readonly bytes: number;
    readonly reply: string;
}
/** What a chat shows the agent gave: a file, a folder, or one removed since. */
export type GivenCard = {
    readonly kind: 'file';
    readonly file: AgentFile;
} | {
    readonly kind: 'folder';
    readonly folder: AgentFolder;
} | {
    readonly kind: 'removed';
    readonly removed: AgentRemoved;
};
/** The Library's use against its bounds, and the open conversation's. */
export interface AgentFilesStanding {
    /** Items: files and folders, a folder one. */
    readonly files: number;
    readonly fileLimit: number;
    readonly bytes: number;
    readonly byteLimit: number;
    /** Files the agent gave in the open conversation. */
    readonly given: number;
    readonly givenLimit: number;
    /** Giving files is off for the open conversation: one held a secret. */
    readonly off: boolean;
    /** Files in all, each of a folder's counted. */
    readonly filesInAll: number;
    readonly filesInAllLimit: number;
    /** What the open conversation has read out of the agent's computer, in bytes. */
    readonly read: number;
    readonly readLimit: number;
    /** The open conversation, by the Library's number. */
    readonly conversation: number;
}
/** The Library as last read. */
export type AgentFilesState = {
    readonly kind: 'unread';
} | {
    readonly kind: 'failed';
    readonly detail: string;
} | {
    readonly kind: 'read';
    /** Newest first. */
    readonly files: readonly AgentFile[];
    /** Newest first. */
    readonly folders: readonly AgentFolder[];
    /** Items it can't name, newest first. */
    readonly unnamed: readonly AgentUnnamed[];
    /** What was removed that the open conversation gave, oldest first. */
    readonly removed: readonly AgentRemoved[];
    readonly standing: AgentFilesStanding;
};
/** A file a chat message showed as given, and whether it was removed since. */
export interface OfferedFile extends AgentFile {
    readonly removed: boolean;
}
/** A folder a chat message showed as given, and whether it was removed since. */
export interface OfferedFolder extends AgentFolder {
    readonly removed: boolean;
}
export interface AgentFilesPort {
    current(): AgentFilesState;
    /** Reads the Library's list again. */
    refresh(): Promise<void>;
    /** The file the chat's message `messageId` said the agent gave. */
    offered(messageId: string): OfferedFile | undefined;
    /** The folder the chat's message `messageId` said the agent gave. */
    offeredFolder(messageId: string): OfferedFolder | undefined;
    /** What the agent gave in the reply `messageId`, by the run that wrote it: none for any other message. */
    givenIn(messageId: string): readonly GivenCard[];
    /** What the open conversation gave in replies the chat doesn't show. */
    givenElsewhere(): readonly GivenCard[];
    /** Whether the item `id` is being downloaded now, alone or with others. */
    downloading(id: string): boolean;
    /** How far a download of more than one file, as one .zip, has got, in files; none when none is under way. */
    downloadProgress(): {
        readonly done: number;
        readonly total: number;
    } | null;
    /** Reads the item `id` out of the Library, checks it, and saves it: a file as itself, a folder as one .zip. */
    download(id: string): Promise<CommandResult>;
    /** Reads the items `ids` out of the Library, checks each file, and saves them as one .zip named `name`. */
    downloadMany(ids: readonly string[], name: string): Promise<CommandResult>;
    /** Reads every item out of the Library, checks each file, and saves them as one .zip. */
    downloadAll(): Promise<CommandResult>;
    /** The items the owner has chosen, of those the Library was last listed with. */
    chosen(): ReadonlySet<string>;
    /** Chooses the item `id`, or lets it go. */
    choose(id: string, on: boolean): void;
    /** Removes the item `id`, a file or a folder, from the Library. */
    remove(id: string): Promise<CommandResult>;
}
/** One item the Library keeps. */
export type LibraryItem = {
    readonly kind: 'file';
    readonly file: AgentFile;
} | {
    readonly kind: 'folder';
    readonly folder: AgentFolder;
};
/** The id of `item`. */
export function itemId(item: LibraryItem): string {
    return item.kind === 'file' ? item.file.id : item.folder.id;
}
/** The name of `item`. */
export function itemName(item: LibraryItem): string {
    return item.kind === 'file' ? item.file.name : item.folder.name;
}
/** When `item` was kept, as RFC 3339. */
function keptAt(item: LibraryItem): string {
    return item.kind === 'file' ? item.file.keptAt : item.folder.keptAt;
}
/** What one reply gave, oldest first; a reply of none holds what was kept before items had replies. */
export interface ReplyGroup {
    readonly reply: string | null;
    readonly items: readonly LibraryItem[];
}
/** What one conversation gave, by reply: the oldest reply first, any of none last. */
export interface ConversationGroup {
    readonly conversation: number;
    /** Whether it is the open conversation. */
    readonly open: boolean;
    readonly replies: readonly ReplyGroup[];
}
/**
 * The Library's items grouped by the conversation and the reply each came
 * from: the open conversation first, then the others, newest first.
 */
export function libraryGroups(files: readonly AgentFile[], folders: readonly AgentFolder[], open: number): ConversationGroup[] {
    const items: LibraryItem[] = [...folders.map((folder): LibraryItem => ({ kind: 'folder', folder })), ...files.map((file): LibraryItem => ({ kind: 'file', file }))]
        .sort((a, b) => (keptAt(a) < keptAt(b) ? -1 : keptAt(a) > keptAt(b) ? 1 : 0));
    const conversations = new Map<number, Map<string | null, LibraryItem[]>>();
    for (const item of items) {
        const { conversation, reply } = item.kind === 'file' ? item.file : item.folder;
        const replies = conversations.get(conversation) ?? new Map<string | null, LibraryItem[]>();
        conversations.set(conversation, replies);
        replies.set(reply, [...(replies.get(reply) ?? []), item]);
    }
    return [...conversations.entries()]
        .sort(([a], [b]) => (a === open ? -1 : b === open ? 1 : b - a))
        .map(([conversation, replies]) => ({
        conversation,
        open: conversation === open,
        replies: [...replies.entries()]
            .sort(([a], [b]) => (a === null ? 1 : 0) - (b === null ? 1 : 0))
            .map(([reply, given]) => ({ reply, items: given })),
    }));
}
/** The most steps a path in a folder has, the longest it is, and the longest one step is, in bytes. */
export const FOLDER_DEPTH = 16;
export const PATH_BYTES = 1024;
export const STEP_BYTES = 255;
/** Names Windows keeps for a device, whatever the extension after them. */
const DEVICE = /^(con|prn|aux|nul|com[0-9]|lpt[0-9]|conin\$|conout\$|clock\$)$/u;
/** Whether `name` is of a kind Windows acts on when its folder is only shown. */
function reachesOut(name: string): boolean {
    const lower = name.toLowerCase();
    return lower === 'desktop.ini' || /\.(url|website|scf|lnk|pif|library-ms|search-ms|searchconnector-ms|theme|themepack|deskthemepack)$/u.test(lower);
}
/** Why `step`, one step of a path in a folder, isn't one Hatchling keeps; none when it is. */
function stepRefused(step: string): string | null {
    if (step === '' || step === '.' || step === '..')
        return 'a step of a path is empty, . or ..';
    if (step.length > STEP_BYTES)
        return 'a name is longer than 255 bytes';
    if (!/^[ -~]+$/u.test(step) || /[<>:"\\|?*]/u.test(step))
        return 'a name isn’t plain ASCII, or holds a character Windows forbids';
    if (step.startsWith(' ') || step.endsWith(' ') || step.endsWith('.'))
        return 'a name starts or ends with a space, or ends with a dot';
    if (DEVICE.test((step.split('.')[0] ?? '').trimEnd().toLowerCase()))
        return 'a name is one Windows keeps for a device';
    if (reachesOut(step))
        return 'a name is a kind of file Windows acts on when its folder is shown';
    return null;
}
/**
 * Why `paths`, a folder's, aren't ones Hatchling keeps, held to the
 * broker's own rules here and never taken on its word: each a relative path
 * of plain steps, at most 1,024 bytes and 16 steps; no two the same but for
 * case; none a file where another has a folder. None when they are.
 */
export function folderPathsRefused(paths: readonly string[]): string | null {
    const seen = new Set<string>();
    for (const path of paths) {
        if (path.length > PATH_BYTES)
            return 'a path is longer than 1,024 bytes';
        const steps = path.split('/');
        if (steps.length > FOLDER_DEPTH)
            return 'a path is more than 16 steps deep';
        for (const step of steps) {
            const why = stepRefused(step);
            if (why !== null)
                return why;
        }
        const lower = path.toLowerCase();
        if (seen.has(lower))
            return 'two paths are the same but for case';
        seen.add(lower);
    }
    for (const path of seen)
        for (let slash = path.indexOf('/'); slash >= 0; slash = path.indexOf('/', slash + 1))
            if (seen.has(path.slice(0, slash)))
                return 'a path is a file where another has a folder';
    return null;
}
/** What a folder's walk left out, in the owner's words; empty when nothing was. */
export function leftOutWords(leftOut: LeftOut): string {
    const count = (n: number, one: string, many: string): string[] => (n === 0 ? [] : [n === 1 ? `1 ${one}` : `${n} ${many}`]);
    const parts = [...count(leftOut.links, 'link', 'links'), ...count(leftOut.mounts, 'other drive', 'other drives'), ...count(leftOut.others, 'special file', 'special files')];
    return parts.length === 0 ? '' : `Left out when it was given: ${parts.join(', ')}.`;
}
/** What opening a file of a kind can do, when it can do more than show itself. */
export type Runs = 'program' | 'macros' | 'page';
/**
 * Extensions of files that run something when opened, lowercase: programs,
 * installers, scripts, shortcuts and the like; documents that hold macros;
 * and pages that can run script in a browser.
 */
const RUNS: Readonly<Record<string, Runs>> = {
    ...Object.fromEntries([
        'exe', 'com', 'bat', 'cmd', 'ps1', 'psm1', 'psd1', 'ps1xml', 'vbs', 'vbe', 'js', 'jse', 'mjs', 'wsf', 'wsh',
        'wsc', 'sct', 'hta', 'msi', 'msp', 'msu', 'msix', 'appx', 'appinstaller', 'application', 'appref-ms', 'scr',
        'pif', 'cpl', 'msc', 'chm', 'hlp', 'inf', 'reg', 'dll', 'ocx', 'sys', 'lnk', 'settingcontent-ms',
        'diagcab', 'gadget', 'jar', 'jnlp', 'sh', 'bash', 'zsh', 'ksh', 'csh', 'fish', 'py', 'pyw', 'pl', 'rb',
        'php', 'app', 'dmg', 'pkg', 'deb', 'rpm', 'appimage', 'run', 'bin', 'command', 'desktop', 'apk', 'ipa',
        'scpt', 'workflow', 'iso', 'img', 'vhd', 'vhdx', 'xll',
    ].map(extension => [extension, 'program' as const])),
    ...Object.fromEntries([
        'doc', 'dot', 'docm', 'dotm', 'xls', 'xlt', 'xlsm', 'xltm', 'xlsb', 'xlam', 'ppt', 'pot', 'pptm', 'potm',
        'ppam', 'ppsm', 'sldm', 'one',
    ].map(extension => [extension, 'macros' as const])),
    ...Object.fromEntries(['html', 'htm', 'shtml', 'xhtml', 'xht', 'svg', 'mht', 'mhtml', 'xml', 'xsl'].map(extension => [extension, 'page' as const])),
};
/**
 * Whether `name` is one the broker gives a file the agent handed over, held
 * to the broker's own refusals: a plain name, not ending in a dot, not a
 * name Windows keeps for a device, and not a file Windows acts on when its
 * folder is only shown. Any other is never offered, listed or saved.
 */
export function isGivenName(name: string): boolean {
    if (!isLibraryName(name) || name.endsWith('.'))
        return false;
    const lower = name.toLowerCase();
    const device = /^(con|prn|aux|nul|com[0-9]|lpt[0-9])(\.|$)/u.test(lower);
    const reaches = lower === 'desktop.ini' || /\.(url|website|scf|lnk|pif|library-ms|search-ms|searchconnector-ms|theme|themepack|deskthemepack)$/u.test(lower);
    return !device && !reaches;
}
/** What opening the file `name` can do, by its last extension, whatever its case; none for a file that only shows itself. */
export function runs(name: string): Runs | null {
    const dot = name.lastIndexOf('.');
    if (dot < 0)
        return null;
    return RUNS[name.slice(dot + 1).toLowerCase()] ?? null;
}
/** What the owner is told of a file the agent gave, by what opening it can do. */
export function caution(name: string): string {
    switch (runs(name)) {
        case 'program':
            return 'A program: it runs when opened. Open it only if you trust what the agent made.';
        case 'macros':
            return 'A document that can run macros. Open it only if you trust what the agent made.';
        case 'page':
            return 'A page that can run script when opened in a browser. Open it only if you trust what the agent made.';
        case null:
            return 'Made by the agent: open it only if you trust what it made.';
    }
}
