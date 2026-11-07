import type { AppContext } from '../../app/context.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button } from '../../components/ui.js';
/** Identity is a first-class Markdown document, not a profile form. */
export function editIdentity(ctx: AppContext): void {
    ctx.dialogs.closeAll();
    ctx.openMemoryDocument('identity-profile');
}
export function renderIdentity(ctx: AppContext): HTMLElement {
    const card = h('section', { class: 'identity-card', 'aria-label': 'Agent identity' }, h('div', { class: 'identity-heading' }, h('div', { class: 'identity-description' }, h('h3', {}, ctx.state.settings.name), h('p', { title: ctx.state.settings.vibe }, ctx.state.settings.vibe)), h('span', { class: 'identity-badge', 'aria-hidden': 'true' }, h('img', { src: './media/identity-badge.png', alt: '' }))), ctx.state.settings.vibe ? h('div', { class: 'identity-vibe' }, h('h4', {}, 'VIBE'), h('p', {}, ctx.state.settings.vibe)) : null, ctx.state.mode === 'live' ? null : button('Edit', () => editIdentity(ctx), { icon: 'edit', className: 'identity-edit' }));
    const covers = h('div', { class: 'identity-covers' });
    for (const kind of ['soul', 'memory']) {
        const entry = ctx.state.memory.find((item) => item.id === `identity-${kind}`);
        if (!entry)
            continue;
        const cover = h('button', { class: `identity-cover cover-${kind}`, type: 'button', 'aria-label': `Open ${entry.title}` }, h('span', { class: 'cover-heading' }, h('strong', {}, entry.title), h('span', {}, 'ACCESS WITH CARE')), h('span', { class: 'cover-footer' }, h('time', {}, entry.date), icon('heart', 19)));
        cover.addEventListener('click', () => ctx.openMemoryDocument(entry.id));
        covers.append(cover);
    }
    return h('div', { class: 'identity-panel' }, card, covers);
}
