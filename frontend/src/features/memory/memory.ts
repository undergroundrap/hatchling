import type { AppContext, UiState } from '../../app/context.js';
import { h, valueOf } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, emptyState, field, markdown, menu, tabs, textareaField } from '../../components/ui.js';
const filters: readonly {
    id: UiState['memoryFilter'];
    label: string;
}[] = [{ id: 'profile', label: 'Profile' }, { id: 'people', label: 'People' }, { id: 'memory', label: 'Memory bank' }, { id: 'preferences', label: 'Preferences' }, { id: 'logs', label: 'Source logs' }];
export function renderMemory(ctx: AppContext): HTMLElement {
    if (ctx.state.mode === 'live')
        return liveMemory(ctx);
    const page = h('div', { class: 'page memory-page reading-wide', 'data-scroll': 'memory' }, h('header', { class: 'page-header' }, h('h1', {}, 'Memory'), button('Add a note', () => editMemory(ctx, ''), { icon: 'plus', variant: 'primary' })), h('p', { class: 'page-intro muted' }, 'Continuity without the secrets. Everything here is a labeled sample.'), tabs(filters, ctx.ui.memoryFilter, (id) => { ctx.ui.memoryFilter = id; ctx.render(); }));
    const entries = ctx.state.memory.filter((entry) => entry.kind === ctx.ui.memoryFilter).toSorted((a, b) => ctx.ui.memoryFilter === 'people' ? b.closeness - a.closeness : b.date.localeCompare(a.date));
    if (!entries.length)
        page.append(emptyState('memory', 'No entries in this view.'));
    for (const entry of entries) {
        page.append(h('article', { class: 'memory-entry' }, h('header', {}, h('div', {}, h('h2', {}, entry.title), h('time', { class: 'tiny muted' }, entry.date)), entry.kind === 'logs' ? h('span', { class: 'pill' }, 'Append-only') : menu(`Manage ${entry.title}`, [{ label: 'Edit entry', icon: 'edit', action: () => editMemory(ctx, entry.id) }, { label: 'Delete entry', icon: 'trash', danger: true, action: () => ctx.dialogs.confirm('Delete this memory entry?', 'This removes the compact note from this workspace. It does not rewrite append-only source logs.', () => { void ctx.run({ type: 'memory.delete', id: entry.id }); }, { label: 'Delete entry', danger: true }) }])), markdown(entry.body)));
    }
    page.append(h('div', { class: 'notice muted' }, icon('lock', 20), 'Passwords, codes, tokens, card and account numbers do not belong in memory. Keep only the existence and protected location of a secret.'));
    return page;
}
/**
 * Connected to Hatchling: what the agent is given, each as approved, and
 * forgotten here. Nothing is added from this page: the agent asks in the
 * chat, and the owner approves its words.
 */
function liveMemory(ctx: AppContext): HTMLElement {
    const page = h('div', { class: 'page memory-page reading-wide', 'data-scroll': 'memory' }, h('header', { class: 'page-header' }, h('h1', {}, 'Memory')), h('p', { class: 'page-intro muted' }, 'What the agent is given in every chat and scheduled run. It asks to remember something when you tell it about yourself, and it’s kept only once you approve the words. Forget anything here.'));
    if (!ctx.state.memory.length)
        page.append(emptyState('memory', 'Nothing kept yet.', 'Tell the agent something about yourself worth keeping: it asks to remember it, and you approve.'));
    for (const entry of ctx.state.memory)
        page.append(h('article', { class: 'memory-entry' }, h('header', {}, h('div', {}, h('h2', {}, entry.title), h('time', { class: 'tiny muted' }, entry.date)), button('Forget', () => ctx.dialogs.confirm('Forget this?', 'The agent isn’t given it from the next chat on. This can’t be undone.', () => { void ctx.run({ type: 'memory.delete', id: entry.id }); }, { label: 'Forget', danger: true }), { variant: 'ghost' })), h('p', {}, entry.body)));
    page.append(h('div', { class: 'notice muted' }, icon('lock', 20), 'Passwords, codes, tokens, card and account numbers don’t belong in memory: say no if the agent ever asks to keep one.'));
    return page;
}
function editMemory(ctx: AppContext, id: string): void {
    const entry = ctx.state.memory.find((item) => item.id === id);
    const form = h('form', { class: 'stack' }, field('Title', 'title', entry?.title ?? '', { required: true }), textareaField('Note · never include secrets', 'body', entry?.body ?? '', 7), h('button', { class: 'button primary', type: 'submit' }, 'Save note'));
    const ref = ctx.dialogs.open(entry ? 'Edit memory' : 'Add a memory note', form);
    form.addEventListener('submit', (event) => {
        event.preventDefault();
        void ctx.run({ type: 'memory.save', id, title: valueOf(form, 'title'), body: valueOf(form, 'body') }).then((result) => {
            if (result.ok) {
                if (!entry)
                    ctx.ui.memoryFilter = 'memory';
                ref.close();
                ctx.render();
            }
        });
    });
}
