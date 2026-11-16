// The files the owner gave the agent, as its inbox holds them (WO-041): each
// by the inbox's own name, with its size, and taken out from here.
import type { AppContext } from '../../app/context.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button } from '../../components/ui.js';
import { isInboxName, sizeWords } from '../../session/inbox.js';
/** The inbox's files, newest first; nothing on an install that has no inbox. `refresh` draws Settings again. */
export function liveInbox(ctx: AppContext, refresh: () => void): HTMLElement[] {
    const port = ctx.inbox;
    if (port === undefined)
        return [];
    const state = port.current();
    const group = h('div', { class: 'settings-group reference-list given-files' });
    if (state.kind === 'unread')
        group.append(h('p', { class: 'data-description' }, 'Reading the agent’s inbox…'));
    else if (state.kind === 'failed')
        group.append(h('p', { class: 'data-description' }, state.detail));
    else if (state.files.length === 0)
        group.append(h('p', { class: 'data-description' }, 'Nothing yet. Files you give Hatchling in the main chat show here.'));
    else
        for (const file of [...state.files].reverse().filter(entry => isInboxName(entry.name))) {
            const remove = button('Remove', () => ctx.dialogs.confirm(`Remove ${file.name} from the agent’s inbox?`, 'Hatchling can’t read it after this. Anything it already read or copied from it stays where it is.', () => {
                void port.remove(file.name).then(result => {
                    ctx.toast(result.message, !result.ok);
                    refresh();
                });
            }, { label: 'Remove file', danger: true }), { variant: 'ghost' });
            group.append(h('div', { class: 'reference-setting-row given-file' }, h('span', { class: 'website-icon' }, icon('file', 18)), h('span', { class: 'setting-row-copy' }, h('code', { class: 'given-file-inbox-name' }, file.name), h('span', { class: 'setting-description' }, sizeWords(file.bytes))), remove));
        }
    const reread = button('Refresh', () => { void port.refresh().then(refresh); }, { variant: 'link' });
    return [h('section', { class: 'given-files-section' }, h('h3', { class: 'settings-section-title' }, 'Files you gave Hatchling'), group, h('p', { class: 'settings-note' }, 'Hatchling reads these at /workspace/inbox and can’t change them. This list is the record of what it holds. ', reread))];
}
