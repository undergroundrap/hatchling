import type { AppContext, UiState } from '../../app/context.js';
import { h, valueOf } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, emptyState, field, markdown, menu, pill, tabs, textareaField } from '../../components/ui.js';
import type { CommandResult } from '../../protocol/models.js';
import type { ReviewEntry } from '../../session/review.js';
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
    appendReview(ctx, page);
    if (!ctx.state.memory.length)
        page.append(emptyState('memory', 'Nothing kept yet.', 'Tell the agent something about yourself worth keeping: it asks to remember it, and you approve.'));
    for (const entry of ctx.state.memory)
        page.append(h('article', { class: 'memory-entry' }, h('header', {}, h('div', {}, h('p', {}, entry.body), h('time', { class: 'tiny muted' }, entry.date)), button('Forget', () => ctx.dialogs.confirm('Forget this?', 'The agent isn’t given it from the next chat on. This can’t be undone.', () => { void ctx.run({ type: 'memory.delete', id: entry.id }); }, { label: 'Forget', danger: true }), { variant: 'ghost' })), ...memoryDetails(ctx, entry.id)));
    page.append(h('div', { class: 'notice muted' }, icon('lock', 20), 'Passwords, codes, tokens, card and account numbers don’t belong in memory: say no if the agent ever asks to keep one.'));
    return page;
}
/**
 * What background work proposed and may not apply on its own: waiting for
 * the owner, then kept as they decided. Only their click here decides one.
 */
function appendReview(ctx: AppContext, page: HTMLElement): void {
    const port = ctx.review;
    const list = port?.current();
    if (!port || !list || list.kind === 'unread')
        return;
    if (list.kind === 'failed') {
        page.append(h('p', { class: 'notice' }, list.detail));
        return;
    }
    const said = (result: CommandResult): void => { ctx.toast(result.message, !result.ok); ctx.render(); };
    const waiting = list.entries.filter((entry) => entry.state === 'waiting');
    const decided = list.entries.filter((entry) => entry.state !== 'waiting');
    if (waiting.length)
        page.append(h('h2', { class: 'idea-section-title' }, 'Waiting for you'), h('p', { class: 'muted' }, 'Hatchling’s background work proposed these and may not apply them on its own. Nothing changes until you approve one.'));
    for (const entry of waiting)
        page.append(reviewEntry(entry, h('div', { class: 'action-row' }, button('Approve', () => ctx.dialogs.confirm('Approve this change?', 'Hatchling applies it the next time that work runs, once it has checked that nothing it would change has changed since.', () => { void port.decide(entry.id, true).then(said); }, { label: 'Approve' }), { variant: 'primary' }), button('Reject', () => { void port.decide(entry.id, false).then(said); }, { variant: 'ghost' }))));
    if (decided.length)
        page.append(h('h2', { class: 'idea-section-title' }, 'Decided'));
    for (const entry of decided)
        page.append(reviewEntry(entry, h('p', { class: 'outcome-copy' }, entry.outcome)));
    if (list.entries.length)
        page.append(h('h2', { class: 'idea-section-title' }, 'Kept'));
}
function reviewEntry(entry: ReviewEntry, footer: HTMLElement): HTMLElement {
    return h('article', { class: 'memory-entry', 'data-review-state': entry.state }, h('header', {}, h('div', {}, h('p', {}, entry.change), h('time', { class: 'tiny muted' }, entry.date)), pill(entry.kind)), entry.why ? h('p', { class: 'muted' }, entry.why) : null, footer);
}
/**
 * What the page says of a kept memory beyond its words: its kind, whether
 * it's known not to be so, why and when it applies, what it replaced,
 * where it came from and when it was confirmed; and, when its day has
 * come, whether it's still so, which only the owner's click confirms.
 */
function memoryDetails(ctx: AppContext, id: string): HTMLElement[] {
    const detail = ctx.memoryDetails?.of(id);
    if (!detail)
        return [];
    const port = ctx.memoryDetails;
    const said = (result: CommandResult): void => { ctx.toast(result.message, !result.ok); ctx.render(); };
    const pills = [detail.kind, detail.absence ? 'Known not to be so' : null].filter((label): label is string => label !== null);
    const lines: HTMLElement[] = [];
    if (pills.length)
        lines.push(h('div', { class: 'metadata' }, ...pills.map((label) => pill(label))));
    if (detail.why)
        lines.push(h('p', { class: 'muted' }, `Why: ${detail.why}`));
    if (detail.howToApply)
        lines.push(h('p', { class: 'muted' }, `When it applies: ${detail.howToApply}`));
    if (detail.replaces)
        lines.push(h('p', { class: 'muted' }, `It replaced “${detail.replaces}”`));
    lines.push(h('p', { class: 'tiny muted' }, detail.confirmed ? `${detail.origin}. ${detail.confirmed}.` : `${detail.origin}.`));
    if (detail.due && port)
        lines.push(h('div', { class: 'notice' }, h('p', {}, 'Is this still so? You asked to be asked by now.'), h('div', { class: 'action-row' }, button('Still so', () => { void port.confirm(id).then(said); }, { variant: 'primary' }))));
    return lines;
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
