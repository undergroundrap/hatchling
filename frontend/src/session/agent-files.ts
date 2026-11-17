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
}
/** The Library's use against its bounds, and the open conversation's. */
export interface AgentFilesStanding {
    readonly files: number;
    readonly fileLimit: number;
    readonly bytes: number;
    readonly byteLimit: number;
    /** Files the agent gave in the open conversation. */
    readonly given: number;
    readonly givenLimit: number;
    /** Giving files is off for the open conversation: one held a secret. */
    readonly off: boolean;
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
    readonly standing: AgentFilesStanding;
};
/** A file a chat message showed as given, and whether it was removed since. */
export interface OfferedFile extends AgentFile {
    readonly removed: boolean;
}
export interface AgentFilesPort {
    current(): AgentFilesState;
    /** Reads the Library's list again. */
    refresh(): Promise<void>;
    /** The file the chat's message `messageId` said the agent gave. */
    offered(messageId: string): OfferedFile | undefined;
    /** Whether the file `id` is being downloaded now. */
    downloading(id: string): boolean;
    /** Reads the file `id` out of the Library, checks it, and saves it. */
    download(id: string): Promise<CommandResult>;
    /** Removes the file `id` from the Library. */
    remove(id: string): Promise<CommandResult>;
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
