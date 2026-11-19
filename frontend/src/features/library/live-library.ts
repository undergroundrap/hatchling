// The files and folders the agent gave the owner, as Hatchling's Library keeps
// them (WO-041, WO-043), on the Library page in the demo's own cards (#523):
// grouped by the conversation and the reply each came from, each a card with
// its type, its whole name, Download and Remove; opened, what Hatchling knows
// of it. Choosing is the page's Select mode: the chosen download as one .zip.
// Nothing of what a file holds is shown. The page is drawn again on every
// change the app publishes; a .zip's progress is counted in place, never
// published, so a redraw never takes a press away mid-download.
import type { AppContext } from '../../app/context.js';
import type { AgentFile, AgentFilesPort, AgentFilesStanding, AgentFolder, AgentUnnamed, ConversationGroup, LibraryItem } from '../../session/agent-files.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, emptyState } from '../../components/ui.js';
import { caution, isGivenName, itemId, leftOutWords, libraryGroups, runs } from '../../session/agent-files.js';
import { sizeWords } from '../../session/inbox.js';
import { downloadFile } from '../chat/file-out.js';
import { extensionLabel } from '../chat/message-actions.js';
import { libraryCard } from './library-card.js';
/** The Library's use, and the conversation's, in the owner's words. */
export function standingWords(standing: AgentFilesStanding): string {
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
/** The Library in a few words, for Settings: what it holds of its bounds, or why it can't say. */
export function librarySummary(port: AgentFilesPort): string {
    const state = port.current();
    if (state.kind === 'unread')
        return 'Reading Hatchling’s Library…';
    if (state.kind === 'failed')
        return state.detail;
    const { standing } = state;
    return `${standing.files} of ${standing.fileLimit} items, ${sizeWords(standing.bytes)} of ${sizeWords(standing.byteLimit)}.`;
}
/** Every item the page shows, in its order. */
function shownItems(port: AgentFilesPort): LibraryItem[] {
    const state = port.current();
    if (state.kind !== 'read')
        return [];
    return libraryGroups(state.files.filter(file => isGivenName(file.name)), state.folders, state.standing.conversation)
        .flatMap(group => group.replies.flatMap(reply => reply.items));
}
/** Whether the page has files the agent gave to show. */
export function hasGivenItems(port: AgentFilesPort): boolean {
    return shownItems(port).length > 0;
}
/** Asks before removing the items `ids`, said as `what`, then does, one by one, and says how it went. */
function removeItems(ctx: AppContext, port: AgentFilesPort, ids: readonly string[], what: string, after?: () => void): void {
    ctx.dialogs.confirm(`Remove ${what} from Hatchling’s Library?`, 'You can’t download it after this. A copy you already saved stays where it is.', () => {
        void (async () => {
            for (const id of ids) {
                const result = await port.remove(id);
                if (!result.ok) {
                    ctx.toast(result.message, true);
                    return;
                }
            }
            ctx.toast(ids.length === 1 ? 'Removed from Hatchling’s Library.' : `Removed ${ids.length} items from Hatchling’s Library.`);
            after?.();
        })();
    }, { label: 'Remove', danger: true });
}
/** Downloads the items `ids` as one .zip named `name`, and says how it went. */
function downloadMany(ctx: AppContext, port: AgentFilesPort, ids: readonly string[], name: string): void {
    void port.downloadMany(ids, name).then(result => ctx.toast(result.message, !result.ok));
}
/** A short word, kept on the card, for what opening a file can do; none for what can't run. */
function runsWord(name: string): string | null {
    switch (runs(name)) {
        case 'program':
            return 'Program: runs when opened';
        case 'macros':
            return 'Can run macros';
        case 'page':
            return 'Page: can run script';
        case null:
            return null;
    }
}
/** What Remove says it removes: the item by its name, a folder with its files. */
function removeWhat(item: LibraryItem): string {
    if (item.kind === 'file')
        return item.file.name;
    return `the folder ${item.folder.name} and its ${item.folder.files === 1 ? '1 file' : `${item.folder.files} files`}`;
}
/** Whether the Library, as last listed, still keeps the item `id`. */
function stillKept(port: AgentFilesPort, id: string): boolean {
    return shownItems(port).some(item => itemId(item) === id);
}
/** When `keptAt` was, in UTC, to the minute. */
function givenAt(keptAt: string): string {
    return `${keptAt.slice(0, 10)} ${keptAt.slice(11, 16)} UTC`;
}
/** What Hatchling knows of an item the agent gave, in a dialog: never what it holds. */
function openDetails(ctx: AppContext, port: AgentFilesPort, item: LibraryItem): void {
    const id = itemId(item);
    const lines: HTMLElement[] = item.kind === 'file'
        ? [
            h('p', {}, `${sizeWords(item.file.bytes)}, kept ${givenAt(item.file.keptAt)}.`),
            h('p', {}, caution(item.file.name)),
            h('p', { class: 'muted text-small' }, 'SHA-256 ', h('code', {}, item.file.sha256)),
        ]
        : [
            h('p', {}, `${item.folder.files === 1 ? '1 file' : `${item.folder.files} files`}, ${sizeWords(item.folder.bytes)}, kept ${givenAt(item.folder.keptAt)}. It downloads as one .zip, its folders as they were.`),
            ...(leftOutWords(item.folder.leftOut) === '' ? [] : [h('p', {}, leftOutWords(item.folder.leftOut))]),
            h('p', { class: 'muted text-small' }, 'SHA-256 of its list ', h('code', {}, item.folder.sha256)),
        ];
    const body = h('div', { class: 'stack given-details' }, h('code', { class: 'given-file-inbox-name' }, item.kind === 'file' ? item.file.name : item.folder.name), ...lines);
    const ref = ctx.dialogs.open(item.kind === 'file' ? 'A file the agent gave you' : 'A folder the agent gave you', body);
    // An item removed meanwhile, here or from elsewhere, closes what says it.
    const gone = (): boolean => {
        if (stillKept(port, id))
            return false;
        ref.close();
        return true;
    };
    const download = button(item.kind === 'file' ? 'Download' : 'Download .zip', () => {
        if (gone())
            ctx.toast('It isn’t in Hatchling’s Library any more.', true);
        else if (port.downloading(id))
            ctx.toast('It is already downloading.', true);
        else
            downloadFile(ctx, port, { id });
    }, { icon: 'download', variant: 'primary' });
    const remove = button('Remove', () => removeItems(ctx, port, [id], removeWhat(item), () => ref.close()), { variant: 'danger' });
    body.append(h('div', { class: 'dialog-actions' }, download, remove));
    const timer = setInterval(() => {
        if (!body.isConnected || gone())
            clearInterval(timer);
    }, 1000);
}
/** One item's card, as the demo draws an artifact's. */
function itemCard(ctx: AppContext, port: AgentFilesPort, item: LibraryItem): HTMLElement {
    const id = itemId(item);
    const selecting = ctx.ui.selectionMode;
    const selected = port.chosen().has(id);
    const onOpen = (): void => {
        if (selecting)
            port.choose(id, !selected);
        else
            openDetails(ctx, port, item);
    };
    const actions = [
        { label: item.kind === 'file' ? 'Download' : 'Download .zip', icon: 'download' as const, disabled: port.downloading(id), action: () => downloadFile(ctx, port, { id }) },
        { label: 'Remove', icon: 'trash' as const, danger: true, action: () => removeItems(ctx, port, [id], removeWhat(item)) },
    ];
    const name = item.kind === 'file' ? item.file.name : item.folder.name;
    const busy = port.downloading(id) && port.downloadProgress() === null ? [h('span', { class: 'muted text-small', role: 'status' }, 'Downloading…')] : [];
    const common = { selecting, selected, onOpen, actions, className: 'given', openId: `given-open-${id}`, previewId: `given-preview-${id}`, attributes: { 'data-given': id }, openLabel: selecting ? `Choose ${name}` : `Open ${name}`, menuLabel: `Actions for ${name}` };
    if (item.kind === 'file') {
        const file: AgentFile = item.file;
        const badge = extensionLabel(file.name) ?? 'FILE';
        const word = runsWord(file.name);
        return libraryCard({ ...common, title: file.name, sub: `${badge} · ${sizeWords(file.bytes)} · kept ${file.keptAt.slice(0, 10)}`, preview: [h('div', { class: 'audio-thumb given-thumb' }, icon('file', 48), h('span', {}, badge))], previewClass: 'preview-given', typeIcon: 'file', typeClass: 'given', notes: [...(word === null ? [] : [h('span', { class: 'given-runs text-small' }, icon('warning', 13), word)]), ...busy] });
    }
    const folder: AgentFolder = item.folder;
    const files = folder.files === 1 ? '1 file' : `${folder.files} files`;
    const left = leftOutWords(folder.leftOut);
    return libraryCard({ ...common, title: folder.name, sub: `Folder · ${files} · ${sizeWords(folder.bytes)} · kept ${folder.keptAt.slice(0, 10)}`, preview: [h('div', { class: 'audio-thumb given-thumb' }, icon('folder', 48), h('span', {}, files))], previewClass: 'preview-given', typeIcon: 'folder', typeClass: 'given', notes: [...(left === '' ? [] : [h('span', { class: 'muted text-small' }, left)]), ...busy] });
}
/** An item Hatchling can't name: its size, and Remove when its id is one Hatchling gives. */
function unnamedCard(ctx: AppContext, port: AgentFilesPort, item: AgentUnnamed): HTMLElement {
    const sub = `${sizeWords(item.bytes)}. It isn’t one Hatchling gives, so it isn’t shown or downloaded; it still counts toward the Library’s room.${item.removable ? '' : ' Its id isn’t one Hatchling gives either, so it can’t be removed from here: Hatchling’s log says more.'}`;
    return libraryCard({ title: 'An item Hatchling can’t name', sub, preview: [h('div', { class: 'audio-thumb given-thumb' }, icon('warning', 48))], previewClass: 'preview-given', typeIcon: 'warning', typeClass: 'given', openLabel: '', menuLabel: `Actions for the item Hatchling can’t name, of ${sizeWords(item.bytes)}`, actions: item.removable ? [{ label: 'Remove', icon: 'trash', danger: true, action: () => removeItems(ctx, port, [item.id], 'this item') }] : [], selecting: false, selected: false, onOpen: () => undefined, className: 'given', openable: false });
}
/** One conversation's cards, by the reply each came from, each reply's downloadable together. */
function conversationBlock(ctx: AppContext, port: AgentFilesPort, group: ConversationGroup): HTMLElement[] {
    const count = group.replies.reduce((all, reply) => all + reply.items.length, 0);
    const blocks: HTMLElement[] = [h('div', { class: 'section-heading library-section-label' }, h('h2', {}, group.open ? 'This conversation' : `Conversation ${group.conversation}`), h('span', { class: 'muted tiny' }, count === 1 ? '1 item' : `${count} items`))];
    const layout = ctx.ui.libraryLayout === 'grid' ? 'artifact-grid' : 'artifact-list';
    for (const reply of group.replies) {
        if (reply.reply === null)
            blocks.push(h('p', { class: 'muted text-small given-files-reply' }, 'Earlier, from before items were kept by reply'));
        else {
            const ids = reply.items.map(itemId);
            const name = `hatchling-${reply.reply}.zip`;
            const [first] = reply.items;
            const at = first === undefined ? '' : first.kind === 'file' ? first.file.keptAt : first.folder.keptAt;
            const together = reply.items.length > 1 && !ctx.ui.selectionMode ? button(`Download these ${reply.items.length} as one .zip`, () => downloadMany(ctx, port, ids, name), { variant: 'link', id: `given-reply-${group.conversation}-${reply.reply}` }) : null;
            blocks.push(h('p', { class: 'muted text-small given-files-reply' }, `Given in one reply, ${givenAt(at)}`, together));
        }
        blocks.push(h('div', { class: `${layout} given-cards` }, ...reply.items.map(item => itemCard(ctx, port, item))));
    }
    return blocks;
}
/** How far a .zip has got, counted in place while the line is on the page: never published, never read out each file. */
function progressLine(port: AgentFilesPort): HTMLElement {
    const count = h('span', { class: 'given-progress-count', 'aria-hidden': 'true' });
    const show = (): void => {
        const progress = port.downloadProgress();
        count.textContent = progress === null ? '' : ` ${progress.done} of ${progress.total} files`;
    };
    show();
    const timer = setInterval(() => {
        if (count.isConnected)
            show();
        else
            clearInterval(timer);
    }, 500);
    return h('p', { class: 'data-description given-progress', role: 'status' }, 'Downloading as one .zip…', count);
}
/** While choosing: how many are chosen, and what can be done with them. */
function selectionBar(ctx: AppContext, port: AgentFilesPort): HTMLElement {
    const shown = shownItems(port).map(itemId);
    const chosen = port.chosen();
    const picked = shown.filter(id => chosen.has(id));
    const done = button('Done', () => {
        ctx.ui.selectionMode = false;
        // Letting the chosen go draws the page again, once.
        port.clearChosen();
        document.getElementById('given-select')?.focus({ preventScroll: true });
    }, { variant: 'ghost', id: 'given-select-done' });
    const [one] = picked;
    const take = picked.length === 1 && one !== undefined
        ? button('Download', () => downloadFile(ctx, port, { id: one }), { variant: 'primary', id: 'given-chosen-download' })
        : button('Download as one .zip', () => downloadMany(ctx, port, picked, 'hatchling-chosen.zip'), { variant: 'primary', disabled: picked.length === 0, id: 'given-chosen-download' });
    const remove = button('Remove', () => removeItems(ctx, port, picked, picked.length === 1 ? 'this item' : `these ${picked.length} items`, () => port.clearChosen()), { variant: 'danger', disabled: picked.length === 0, id: 'given-chosen-remove' });
    return h('div', { class: 'selection-bar given-selection-bar' }, `${picked.length} chosen`, take, remove, done);
}
/** The actions in the page's header for the agent's files: Select, and Download all; none while a .zip is made. */
export function liveLibraryActions(ctx: AppContext, port: AgentFilesPort): HTMLElement[] {
    const shown = shownItems(port);
    if (shown.length === 0 || port.downloadProgress() !== null || ctx.ui.selectionMode)
        return [];
    const select = button('Select', () => {
        ctx.ui.selectionMode = true;
        ctx.render();
        document.getElementById('given-select-done')?.focus({ preventScroll: true });
    }, { variant: 'ghost', id: 'given-select' });
    const all = shown.length > 1 ? button('Download all as one .zip', () => { void port.downloadAll().then(result => ctx.toast(result.message, !result.ok)); }, { icon: 'download', id: 'given-download-all' }) : null;
    return [select, ...(all === null ? [] : [all])];
}
/**
 * Ends choosing where its cards aren't shown, and lets the chosen go: after
 * this draw, so the page isn't drawn again while it's being drawn.
 */
export function endChoosing(ctx: AppContext, port: AgentFilesPort): void {
    if (!ctx.ui.selectionMode)
        return;
    ctx.ui.selectionMode = false;
    queueMicrotask(() => port.clearChosen());
}
/**
 * The files and folders the agent gave, for the Library page: the Library's
 * use, a .zip's progress or the choice under way, then each conversation's
 * cards; nothing yet, said with where they come from.
 */
export function liveLibrary(ctx: AppContext, port: AgentFilesPort): HTMLElement[] {
    const state = port.current();
    if (state.kind !== 'read' || !hasGivenItems(port))
        endChoosing(ctx, port);
    if (state.kind === 'unread')
        return [h('p', { class: 'data-description', role: 'status' }, 'Reading Hatchling’s Library…')];
    if (state.kind === 'failed')
        return [h('p', { class: 'data-description given-failed' }, state.detail, ' ', button('Try again', () => { void port.refresh(); }, { variant: 'link', id: 'given-retry' }))];
    const groups = libraryGroups(state.files.filter(file => isGivenName(file.name)), state.folders, state.standing.conversation);
    const reread = button('Refresh', () => { void port.refresh(); }, { variant: 'link', id: 'given-refresh' });
    const blocks: HTMLElement[] = [
        h('p', { class: 'muted text-small given-standing' }, standingWords(state.standing), ' ', reread),
        h('p', { class: 'muted text-small given-provenance' }, 'These are the copies Hatchling made from the agent’s computer and checked for the secrets it keeps. A download is always this copy, each file checked against its SHA-256 and saved, never opened; many at once are one .zip, a folder’s files under its name.'),
    ];
    if (port.downloadProgress() !== null)
        blocks.push(progressLine(port));
    else if (ctx.ui.selectionMode)
        blocks.push(selectionBar(ctx, port));
    if (groups.length === 0 && state.unnamed.length === 0) {
        const empty = emptyState('library', 'Nothing yet.', 'Files and folders the agent gives you in the chat show here.');
        empty.append(button('Go to the chat', () => ctx.navigate('chat'), { variant: 'ghost' }));
        blocks.push(empty);
        return blocks;
    }
    for (const group of groups)
        blocks.push(...conversationBlock(ctx, port, group));
    if (state.unnamed.length > 0)
        blocks.push(h('div', { class: `${ctx.ui.libraryLayout === 'grid' ? 'artifact-grid' : 'artifact-list'} given-cards` }, ...state.unnamed.map(item => unnamedCard(ctx, port, item))));
    return blocks;
}
/**
 * The Library in Settings, Data controls: what it holds of its bounds, kept
 * up to date while it's on screen, and the way to it. The files themselves
 * are on the Library page.
 */
export function libraryOverview(ctx: AppContext): HTMLElement[] {
    const port = ctx.agentFiles;
    if (port === undefined)
        return [];
    const summary = h('span', { class: 'given-summary' });
    const show = (): void => {
        summary.textContent = librarySummary(port);
    };
    show();
    const timer = setInterval(() => {
        if (summary.isConnected)
            show();
        else
            clearInterval(timer);
    }, 1000);
    const open = button('Open the Library', () => {
        ctx.dialogs.closeAll();
        ctx.ui.libraryFilter = 'all';
        ctx.navigate('library');
    }, { variant: 'ghost', className: 'data-navigation-row', id: 'given-open-library' });
    open.append(icon('chevron', 16));
    return [h('section', { class: 'given-files-section' }, h('h3', { class: 'settings-section-title' }, 'Files and folders the agent gave you'), h('div', { class: 'settings-group data-links-card' }, h('p', { class: 'data-description' }, summary), open))];
}
