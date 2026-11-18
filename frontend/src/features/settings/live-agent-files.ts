// The files the agent gave the owner, as Hatchling's Library keeps them
// (WO-041): each by the broker's plain name, whole, with its size, when it was
// kept and what opening it can do; downloaded or removed from here. The
// Library's use against its bounds, and whether the agent can give more in
// this conversation, are said above the list. A folder (WO-043) is listed
// with its count of files, to be removed; it is downloaded from a later
// version. An item it can't name is listed by its size, to be removed.
import type { AppContext } from '../../app/context.js';
import type { AgentFile, AgentFilesPort, AgentFilesStanding, AgentFolder, AgentUnnamed } from '../../session/agent-files.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button } from '../../components/ui.js';
import { caution, isGivenName } from '../../session/agent-files.js';
import { sizeWords } from '../../session/inbox.js';
import { downloadFile } from '../chat/file-out.js';
/** The Library's use, and the conversation's, in the owner's words. */
function standingWords(standing: AgentFilesStanding): string {
    const held = `${standing.files} of ${standing.fileLimit} items (${standing.filesInAll} of ${standing.filesInAllLimit} files), ${sizeWords(standing.bytes)} of ${sizeWords(standing.byteLimit)}.`;
    if (standing.off)
        return `${held} Giving is off for this conversation: something held a secret Hatchling keeps. Start a new conversation to turn it back on.`;
    if (standing.given >= standing.givenLimit)
        return `${held} The agent has given all ${standing.givenLimit} files or folders it can in this conversation. Start a new conversation for more.`;
    if (standing.read >= standing.readLimit)
        return `${held} The agent has read all ${sizeWords(standing.readLimit)} it can out of its computer in this conversation. Start a new conversation for more.`;
    const full = standing.files >= standing.fileLimit || standing.bytes >= standing.byteLimit || standing.filesInAll >= standing.filesInAllLimit;
    const given = `The agent has given ${standing.given} of the ${standing.givenLimit} files or folders it can in this conversation.`;
    return full ? `${held} The Library is full: remove something before the agent can give more. ${given}` : `${held} ${given}`;
}
/** Asks before removing `file`, then does, and draws Settings again. */
function removeFile(ctx: AppContext, port: AgentFilesPort, file: AgentFile, refresh: () => void): void {
    ctx.dialogs.confirm(`Remove ${file.name} from Hatchling’s Library?`, 'You can’t download it after this. A copy you already saved stays where it is.', () => {
        void port.remove(file.id).then(result => {
            ctx.toast(result.message, !result.ok);
            refresh();
        });
    }, { label: 'Remove file', danger: true });
}
/** Asks before removing the item `id`, said as `what`, then does, and draws Settings again. */
function removeItem(ctx: AppContext, port: AgentFilesPort, id: string, what: string, refresh: () => void): void {
    ctx.dialogs.confirm(`Remove ${what} from Hatchling’s Library?`, 'You can’t download it after this. A copy you already saved stays where it is.', () => {
        void port.remove(id).then(result => {
            ctx.toast(result.message, !result.ok);
            refresh();
        });
    }, { label: 'Remove', danger: true });
}
/** One folder's row: its whole name, its files and size, and Remove. */
function folderRow(ctx: AppContext, port: AgentFilesPort, folder: AgentFolder, refresh: () => void): HTMLElement {
    const files = folder.files === 1 ? '1 file' : `${folder.files} files`;
    const remove = button('Remove', () => removeItem(ctx, port, folder.id, `the folder ${folder.name} and its ${files}`, refresh), { variant: 'ghost' });
    const about = `A folder of ${files}, ${sizeWords(folder.bytes)} · kept ${folder.keptAt.slice(0, 10)}. Downloading a folder comes in a later version of Hatchling.`;
    return h('div', { class: 'reference-setting-row given-file' }, h('span', { class: 'website-icon' }, icon('folder', 18)), h('span', { class: 'setting-row-copy' }, h('code', { class: 'given-file-inbox-name' }, folder.name), h('span', { class: 'setting-description' }, about)), remove);
}
/** An item Hatchling can't name: its size, and Remove. */
function unnamedRow(ctx: AppContext, port: AgentFilesPort, item: AgentUnnamed, refresh: () => void): HTMLElement {
    const copy = h('span', { class: 'setting-row-copy' }, h('span', {}, 'An item Hatchling can’t name'), h('span', { class: 'setting-description' }, `${sizeWords(item.bytes)}. It isn’t one Hatchling gives, so it isn’t shown or downloaded; it still counts toward the Library’s room.${item.removable ? '' : ' Its id isn’t one Hatchling gives either, so it can’t be removed from here: Hatchling’s log says more.'}`));
    const row = h('div', { class: 'reference-setting-row given-file' }, h('span', { class: 'website-icon' }, icon('warning', 18)), copy);
    if (item.removable)
        row.append(button('Remove', () => removeItem(ctx, port, item.id, 'this item', refresh), { variant: 'ghost' }));
    return row;
}
/** One file's row: its whole name, what it is, Download and Remove. */
function fileRow(ctx: AppContext, port: AgentFilesPort, file: AgentFile, refresh: () => void): HTMLElement {
    const download = port.downloading(file.id)
        ? h('span', { class: 'setting-description', role: 'status' }, 'Downloading…')
        : button('Download', () => downloadFile(ctx, port, file), { variant: 'ghost' });
    const remove = button('Remove', () => removeFile(ctx, port, file, refresh), { variant: 'ghost' });
    const about = `${sizeWords(file.bytes)} · kept ${file.keptAt.slice(0, 10)}. ${caution(file.name)}`;
    // The hash, whole, to compare a saved file against elsewhere.
    const hash = h('span', { class: 'setting-description' }, 'SHA-256 ', h('code', {}, file.sha256));
    return h('div', { class: 'reference-setting-row given-file' }, h('span', { class: 'website-icon' }, icon('file', 18)), h('span', { class: 'setting-row-copy' }, h('code', { class: 'given-file-inbox-name' }, file.name), h('span', { class: 'setting-description' }, about), hash), download, remove);
}
/** The Library's files, newest first; nothing on an install that has none. `refresh` draws Settings again. */
export function liveAgentFiles(ctx: AppContext, refresh: () => void): HTMLElement[] {
    const port = ctx.agentFiles;
    if (port === undefined)
        return [];
    const state = port.current();
    const group = h('div', { class: 'settings-group reference-list given-files' });
    const notes: HTMLElement[] = [];
    if (state.kind === 'unread')
        group.append(h('p', { class: 'data-description' }, 'Reading Hatchling’s Library…'));
    else if (state.kind === 'failed')
        group.append(h('p', { class: 'data-description' }, state.detail));
    else {
        notes.push(h('p', { class: 'data-description' }, standingWords(state.standing)));
        const files = state.files.filter(file => isGivenName(file.name));
        if (files.length === 0 && state.folders.length === 0 && state.unnamed.length === 0)
            group.append(h('p', { class: 'data-description' }, 'Nothing yet. Files and folders the agent gives you in the main chat show here.'));
        for (const folder of state.folders)
            group.append(folderRow(ctx, port, folder, refresh));
        for (const file of files)
            group.append(fileRow(ctx, port, file, refresh));
        for (const item of state.unnamed)
            group.append(unnamedRow(ctx, port, item, refresh));
        // Many files are one save, as one .zip, each checked as a download is.
        const progress = port.downloadingAll();
        if (progress !== null)
            notes.push(h('p', { class: 'data-description', role: 'status' }, `Downloading all: ${progress.done} of ${progress.total} files…`));
        else if (files.length > 1)
            notes.push(h('p', { class: 'data-description' }, button(state.folders.length > 0 ? 'Download all files as one .zip (not the folders)' : 'Download all as one .zip', () => { void port.downloadAll().then(result => ctx.toast(result.message, !result.ok)); }, { variant: 'ghost' })));
    }
    const reread = button('Refresh', () => { void port.refresh().then(refresh); }, { variant: 'link' });
    return [h('section', { class: 'given-files-section' }, h('h3', { class: 'settings-section-title' }, 'Files and folders the agent gave you'), ...notes, group, h('p', { class: 'settings-note' }, 'These are the copies Hatchling made from the agent’s computer and checked for the secrets it keeps. A download is always this copy, checked against its SHA-256 and saved as a file, never opened. ', reread))];
}
