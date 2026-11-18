// A file the agent gave the owner, as the chat shows it beside the broker's
// notice (WO-041): that it came from the agent's computer, what opening it
// can do, and a download of the Library's copy. Never a preview, a link to
// it, or anything of it shown in the page.
import type { AppContext } from '../../app/context.js';
import type { DeepReadonly, Message } from '../../protocol/models.js';
import type { AgentFilesPort, GivenCard, OfferedFile } from '../../session/agent-files.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button } from '../../components/ui.js';
import { caution } from '../../session/agent-files.js';
import { sizeWords } from '../../session/inbox.js';
/** Downloads `file`, and says how it went. */
export function downloadFile(ctx: AppContext, port: AgentFilesPort, file: {
    readonly id: string;
}): void {
    void port.download(file.id).then(result => ctx.toast(result.message, !result.ok));
}
/** The download, or that one is under way. */
function downloadButton(ctx: AppContext, port: AgentFilesPort, file: OfferedFile): HTMLElement {
    if (port.downloading(file.id))
        return h('span', { class: 'given-file-count', role: 'status' }, 'Downloading…');
    return button('Download', () => downloadFile(ctx, port, file), { variant: 'link' });
}
/**
 * What the chat shows beside the broker's word that the agent gave a file:
 * where it came from, what opening it can do, and Download. Nothing for any
 * other message, or with no Library to ask.
 */
export function offeredFileDetail(ctx: AppContext, message: DeepReadonly<Message>): HTMLElement | null {
    const port = ctx.agentFiles;
    if (port === undefined || message.kind !== 'notice')
        return null;
    const file = port.offered(message.id);
    if (file === undefined)
        return null;
    if (file.removed)
        return h('p', { class: 'given-file-detail' }, 'Removed from Hatchling’s Library since.');
    return h('p', { class: 'given-file-detail' }, `From the agent’s computer. ${caution(file.name)} `, downloadButton(ctx, port, file));
}
/** How many files, in words. */
function filesWords(files: number): string {
    return files === 1 ? '1 file' : `${files} files`;
}
/** What the chat shows of one thing the agent gave, as the Library says it now. */
function givenCard(ctx: AppContext, port: AgentFilesPort, card: GivenCard): HTMLElement {
    switch (card.kind) {
        case 'file':
            return h('p', { class: 'given-file-detail' }, h('code', { class: 'given-file-inbox-name' }, card.file.name), ` · ${sizeWords(card.file.bytes)}. From the agent’s computer. ${caution(card.file.name)} `, downloadButton(ctx, port, { ...card.file, removed: false }));
        case 'folder':
            return h('p', { class: 'given-file-detail' }, h('code', { class: 'given-file-inbox-name' }, card.folder.name), ` · a folder of ${filesWords(card.folder.files)}, ${sizeWords(card.folder.bytes)}. From the agent’s computer. Downloading a folder comes in a later version of Hatchling.`);
        case 'removed':
            return h('p', { class: 'given-file-detail' }, h('code', { class: 'given-file-inbox-name' }, card.removed.name), card.removed.folder ? ` · a folder of ${filesWords(card.removed.files)}. Removed from Hatchling’s Library since.` : ' · Removed from Hatchling’s Library since.');
    }
}
/**
 * What the agent gave in the reply `message`, by the run that wrote it,
 * from the Library: after a restart too. Nothing for any other message, or
 * with no Library to ask.
 */
export function givenInReply(ctx: AppContext, message: DeepReadonly<Message>): HTMLElement | null {
    const port = ctx.agentFiles;
    if (port === undefined || message.role !== 'agent')
        return null;
    const cards = port.givenIn(message.id);
    return cards.length === 0 ? null : h('div', { class: 'given-file-detail' }, ...cards.map(card => givenCard(ctx, port, card)));
}
/**
 * What the open conversation gave in replies the chat doesn't show: kept
 * apart, under no reply, so none reads as another's.
 */
export function givenElsewhere(ctx: AppContext): HTMLElement | null {
    const port = ctx.agentFiles;
    if (port === undefined)
        return null;
    const cards = port.givenElsewhere();
    if (cards.length === 0)
        return null;
    return h('div', { class: 'stream-event notice' }, icon('file', 16), h('div', {}, h('p', {}, 'Given in this conversation, not under a reply shown here.'), ...cards.map(card => givenCard(ctx, port, card))));
}
/**
 * What the chat shows beside the broker's word that the agent gave a
 * folder: where it came from, and its files. Nothing for any other message.
 */
export function offeredFolderDetail(ctx: AppContext, message: DeepReadonly<Message>): HTMLElement | null {
    const port = ctx.agentFiles;
    if (port === undefined || message.kind !== 'notice')
        return null;
    const folder = port.offeredFolder(message.id);
    if (folder === undefined)
        return null;
    if (folder.removed)
        return h('p', { class: 'given-file-detail' }, 'Removed from Hatchling’s Library since.');
    return h('p', { class: 'given-file-detail' }, `From the agent’s computer: ${filesWords(folder.files)}. Downloading a folder comes in a later version of Hatchling.`);
}
