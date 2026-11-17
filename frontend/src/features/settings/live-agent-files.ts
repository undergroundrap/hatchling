// The files the agent gave the owner, as Hatchling's Library keeps them
// (WO-041): each by the broker's plain name, whole, with its size, when it was
// kept and what opening it can do; downloaded or removed from here. The
// Library's use against its bounds, and whether the agent can give more in
// this conversation, are said above the list.
import type { AppContext } from '../../app/context.js';
import type { AgentFile, AgentFilesPort, AgentFilesStanding } from '../../session/agent-files.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button } from '../../components/ui.js';
import { caution, isGivenName } from '../../session/agent-files.js';
import { sizeWords } from '../../session/inbox.js';
import { downloadFile } from '../chat/file-out.js';
/** The Library's use, and the conversation's, in the owner's words. */
function standingWords(standing: AgentFilesStanding): string {
    const held = `${standing.files} of ${standing.fileLimit} files, ${sizeWords(standing.bytes)} of ${sizeWords(standing.byteLimit)}.`;
    if (standing.off)
        return `${held} Giving files is off for this conversation: a file held a secret Hatchling keeps. Start a new conversation to turn it back on.`;
    if (standing.given >= standing.givenLimit)
        return `${held} The agent has given all ${standing.givenLimit} files it can in this conversation. Start a new conversation for more.`;
    const full = standing.files >= standing.fileLimit || standing.bytes >= standing.byteLimit;
    const given = `The agent has given ${standing.given} of ${standing.givenLimit} files in this conversation.`;
    return full ? `${held} The Library is full: remove a file before the agent can give another. ${given}` : `${held} ${given}`;
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
        if (files.length === 0)
            group.append(h('p', { class: 'data-description' }, 'Nothing yet. Files the agent gives you in the main chat show here.'));
        for (const file of files)
            group.append(fileRow(ctx, port, file, refresh));
        // Many files are one save, as one .zip, each checked as a download is.
        const progress = port.downloadingAll();
        if (progress !== null)
            notes.push(h('p', { class: 'data-description', role: 'status' }, `Downloading all: ${progress.done} of ${progress.total} files…`));
        else if (files.length > 1)
            notes.push(h('p', { class: 'data-description' }, button('Download all as one .zip', () => { void port.downloadAll().then(result => ctx.toast(result.message, !result.ok)); }, { variant: 'ghost' })));
    }
    const reread = button('Refresh', () => { void port.refresh().then(refresh); }, { variant: 'link' });
    return [h('section', { class: 'given-files-section' }, h('h3', { class: 'settings-section-title' }, 'Files the agent gave you'), ...notes, group, h('p', { class: 'settings-note' }, 'These are the copies Hatchling made from the agent’s computer and checked for the secrets it keeps. A download is always this copy, checked against its SHA-256 and saved as a file, never opened. ', reread))];
}
