// The files the agent gave the owner, as Hatchling's Library keeps them
// (WO-041): each by the broker's plain name, whole, with its size, when it was
// kept and what opening it can do; downloaded or removed from here. The
// Library's use against its bounds, and whether the agent can give more in
// this conversation, are said above the list. Items are grouped by the
// conversation and the reply they came from (WO-043): a folder downloads as
// one .zip, a reply's items, any the owner chooses, or all, as one .zip too.
// An item it can't name is listed by its size, to be removed.
import type { AppContext } from '../../app/context.js';
import type { AgentFile, AgentFilesPort, AgentFilesStanding, AgentFolder, AgentUnnamed, ConversationGroup, LibraryItem } from '../../session/agent-files.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button } from '../../components/ui.js';
import { caution, isGivenName, itemId, leftOutWords, libraryGroups } from '../../session/agent-files.js';
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
/** Downloads the items `ids` as one .zip named `name`, and says how it went. */
function downloadMany(ctx: AppContext, port: AgentFilesPort, ids: readonly string[], name: string): void {
    void port.downloadMany(ids, name).then(result => ctx.toast(result.message, !result.ok));
}
/** The box that chooses the item `id`, named `name`, to download with others. */
function chooseBox(port: AgentFilesPort, id: string, name: string): HTMLElement {
    const box = h('input', { type: 'checkbox', id: `given-choose-${id}`, class: 'given-file-choose', checked: port.chosen().has(id), 'aria-label': `Choose ${name}` });
    // Choosing draws Settings again, the box keeping its focus by its id.
    box.addEventListener('change', () => port.choose(id, box.checked));
    return box;
}
/** Download, or that the item `id` is downloading. */
function downloadButton(ctx: AppContext, port: AgentFilesPort, id: string, label: string): HTMLElement {
    return port.downloading(id)
        ? h('span', { class: 'setting-description', role: 'status' }, 'Downloading…')
        : button(label, () => downloadFile(ctx, port, { id }), { variant: 'ghost' });
}
/** One folder's row: its whole name, its files and size, what its walk left out, Download as a .zip, and Remove. */
function folderRow(ctx: AppContext, port: AgentFilesPort, folder: AgentFolder, refresh: () => void): HTMLElement {
    const files = folder.files === 1 ? '1 file' : `${folder.files} files`;
    const remove = button('Remove', () => removeItem(ctx, port, folder.id, `the folder ${folder.name} and its ${files}`, refresh), { variant: 'ghost' });
    const about = [`A folder of ${files}, ${sizeWords(folder.bytes)} · kept ${folder.keptAt.slice(0, 10)}. Saved as one .zip, its folders as they were.`, leftOutWords(folder.leftOut)].filter(words => words !== '').join(' ');
    return h('div', { class: 'reference-setting-row given-file' }, chooseBox(port, folder.id, folder.name), h('span', { class: 'website-icon' }, icon('folder', 18)), h('span', { class: 'setting-row-copy' }, h('code', { class: 'given-file-inbox-name' }, folder.name), h('span', { class: 'setting-description' }, about)), downloadButton(ctx, port, folder.id, 'Download .zip'), remove);
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
    const remove = button('Remove', () => removeFile(ctx, port, file, refresh), { variant: 'ghost' });
    const about = `${sizeWords(file.bytes)} · kept ${file.keptAt.slice(0, 10)}. ${caution(file.name)}`;
    // The hash, whole, to compare a saved file against elsewhere.
    const hash = h('span', { class: 'setting-description' }, 'SHA-256 ', h('code', {}, file.sha256));
    return h('div', { class: 'reference-setting-row given-file' }, chooseBox(port, file.id, file.name), h('span', { class: 'website-icon' }, icon('file', 18)), h('span', { class: 'setting-row-copy' }, h('code', { class: 'given-file-inbox-name' }, file.name), h('span', { class: 'setting-description' }, about), hash), downloadButton(ctx, port, file.id, 'Download'), remove);
}
/** When the first of `items` was kept, in UTC, to the minute. */
function givenAt(items: readonly LibraryItem[]): string {
    const [first] = items;
    const at = first === undefined ? '' : first.kind === 'file' ? first.file.keptAt : first.folder.keptAt;
    return `${at.slice(0, 10)} ${at.slice(11, 16)} UTC`;
}
/** One item's row. */
function itemRow(ctx: AppContext, port: AgentFilesPort, item: LibraryItem, refresh: () => void): HTMLElement {
    return item.kind === 'file' ? fileRow(ctx, port, item.file, refresh) : folderRow(ctx, port, item.folder, refresh);
}
/**
 * One conversation's items, by the reply each came from, with each reply's
 * items downloadable together; what was kept before items had replies is
 * apart, under none.
 */
function conversationRows(ctx: AppContext, port: AgentFilesPort, group: ConversationGroup, refresh: () => void): HTMLElement[] {
    const rows: HTMLElement[] = [h('h4', { class: 'given-files-group' }, group.open ? 'This conversation' : `Conversation ${group.conversation}`)];
    for (const reply of group.replies) {
        if (reply.reply === null)
            rows.push(h('p', { class: 'setting-description given-files-reply' }, 'Earlier, from before items were kept by reply'));
        else {
            const ids = reply.items.map(itemId);
            const name = `hatchling-${reply.reply}.zip`;
            const together = reply.items.length > 1 ? button(`Download these ${reply.items.length} as one .zip`, () => downloadMany(ctx, port, ids, name), { variant: 'link' }) : null;
            rows.push(h('p', { class: 'setting-description given-files-reply' }, `Given in one reply, ${givenAt(reply.items)}`, together));
        }
        rows.push(...reply.items.map(item => itemRow(ctx, port, item, refresh)));
    }
    return rows;
}
/**
 * The Library's items, by the conversation and the reply each came from;
 * nothing on an install that has none. `refresh` draws Settings again.
 */
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
        const groups = libraryGroups(files, state.folders, state.standing.conversation);
        for (const conversation of groups)
            group.append(...conversationRows(ctx, port, conversation, refresh));
        for (const item of state.unnamed)
            group.append(unnamedRow(ctx, port, item, refresh));
        // Many items are one save, as one .zip, each file checked as a download is.
        const progress = port.downloadProgress();
        const shown = groups.flatMap(conversation => conversation.replies.flatMap(reply => reply.items.map(itemId)));
        const chosen = port.chosen();
        const picked = shown.filter(id => chosen.has(id));
        if (progress !== null)
            notes.push(h('p', { class: 'data-description', role: 'status' }, `Downloading: ${progress.done} of ${progress.total} files…`));
        else if (picked.length > 0) {
            const [one] = picked;
            const take = picked.length === 1 && one !== undefined
                ? button('Download the one chosen', () => downloadFile(ctx, port, { id: one }), { variant: 'ghost' })
                : button(`Download the ${picked.length} chosen as one .zip`, () => downloadMany(ctx, port, picked, 'hatchling-chosen.zip'), { variant: 'ghost' });
            notes.push(h('p', { class: 'data-description' }, take, button('Clear the choice', () => {
                for (const id of picked)
                    port.choose(id, false);
            }, { variant: 'ghost' })));
        }
        else if (shown.length > 1)
            notes.push(h('p', { class: 'data-description' }, button('Download all as one .zip', () => { void port.downloadAll().then(result => ctx.toast(result.message, !result.ok)); }, { variant: 'ghost' })));
    }
    const reread = button('Refresh', () => { void port.refresh().then(refresh); }, { variant: 'link' });
    return [h('section', { class: 'given-files-section' }, h('h3', { class: 'settings-section-title' }, 'Files and folders the agent gave you'), ...notes, group, h('p', { class: 'settings-note' }, 'These are the copies Hatchling made from the agent’s computer and checked for the secrets it keeps. A download is always this copy, each file checked against its SHA-256 and saved, never opened; many at once are one .zip, a folder’s files under its name. ', reread))];
}
