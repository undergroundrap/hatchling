import { appendContentState } from '../../components/content-state.js';
import type { AppContext } from '../../app/context.js';
import { h, valueOf } from '../../components/dom.js';
import { pictogram } from '../../components/pictograms.js';
import { icon } from '../../components/icons.js';
import { button, emptyState, menu, pill, tabs, textareaField } from '../../components/ui.js';
export function renderIdeas(ctx: AppContext): HTMLElement {
    const page = h('div', { class: 'page ideas-page reading-wide', 'data-scroll': 'ideas' }, h('header', { class: 'page-header' }, h('h1', {}, 'Ideas'), menu('Idea views', [
        { label: 'Suggested ideas', icon: 'idea', action: () => { ctx.ui.ideaFilter = 'suggested'; ctx.render(); } },
        { label: 'Active & completed', icon: 'run', action: () => { ctx.ui.ideaFilter = 'active'; ctx.render(); } },
        ...(ctx.state.mode === 'live' ? [] : [{ label: ctx.ui.ideaExamples ? 'Hide demonstration ideas' : 'Show demonstration ideas', icon: 'play' as const, action: () => { ctx.ui.ideaExamples = !ctx.ui.ideaExamples; ctx.render(); } }]),
        { label: 'Dismissed ideas', icon: 'trash', action: () => { ctx.ui.ideaFilter = 'dismissed'; ctx.render(); } },
    ])));
    page.append(h('p', { class: 'ideas-intro' }, "I’m always thinking about new and different ways to help you. I’ll surface my favorite ideas here."));
    if (appendContentState(ctx, page, 'ideas'))
        return page;
    const port = ctx.state.mode === 'live' ? ctx.ideas : undefined;
    if (port) {
        const state = port.current();
        if (state.kind === 'unread') {
            page.append(h('p', { class: 'muted' }, 'Reading your ideas…'));
            return page;
        }
        if (state.kind === 'failed') {
            page.append(h('p', { class: 'notice' }, state.detail));
            return page;
        }
        page.append(h('div', { class: 'action-row' }, button(state.suggesting ? 'Thinking of ideas…' : 'Suggest ideas', () => { void port.suggestNow().then((result) => { ctx.toast(result.message, !result.ok); ctx.render(); }); }, { icon: 'spark', variant: 'primary', disabled: state.suggesting })));
        if (state.suggesting)
            page.append(h('p', { class: 'notice', role: 'status' }, 'Hatchling is reading your goals and what you’ve asked it to remember. Its ideas appear here when they’re ready.'));
    }
    const active = ctx.state.ideas.some((idea) => !['suggested', 'dismissed'].includes(idea.state));
    if (active || ctx.ui.ideaFilter !== 'suggested')
        page.append(tabs([{ id: 'suggested', label: 'For you' }, { id: 'active', label: 'Active & completed' }, { id: 'dismissed', label: 'Dismissed' }], ctx.ui.ideaFilter, (id) => { ctx.ui.ideaFilter = id; ctx.render(); }));
    const ideas = ctx.state.ideas.filter((idea) => (!idea.sampleOnly || ctx.ui.ideaExamples) && (ctx.ui.ideaFilter === 'active' ? !['suggested', 'dismissed'].includes(idea.state) : idea.state === ctx.ui.ideaFilter));
    if (!ideas.length)
        page.append(port && ctx.ui.ideaFilter === 'suggested'
            ? emptyState('idea', 'No ideas yet.', 'Ask for some: Hatchling reads your goals and what you’ve asked it to remember, and suggests what it could do for you.')
            : emptyState('idea', ctx.ui.ideaFilter === 'dismissed' ? 'No dismissed ideas.' : 'Nothing here yet.', 'The suggestions stay available in For you.'));
    let previousSection = '';
    for (const idea of ideas) {
        const section = idea.section ?? (idea.sampleOnly ? 'More possibilities' : '');
        if (section && section !== previousSection && ctx.ui.ideaFilter === 'suggested')
            page.append(h('h2', { class: 'idea-section-title' }, section));
        previousSection = section;
        page.append(h('article', { class: 'idea-row', 'data-idea-state': idea.state }, h('span', { class: 'idea-pictogram' }, pictogram(idea.icon)), h('div', { class: 'row-copy' }, button(idea.title, () => openIdea(ctx, idea.id), { variant: 'ghost', className: 'row-title-button' }), h('p', { class: 'muted' }, idea.summary), h('div', { class: `metadata ${idea.state === 'suggested' ? 'sr-only' : ''}` }, idea.domain, idea.state !== 'suggested' ? pill(idea.state.replaceAll('_', ' '), idea.state === 'completed' ? 'success' : '') : null), idea.outcome ? h('p', { class: 'outcome-copy' }, idea.outcome) : null, idea.artifactId ? button('Open result', () => ctx.openArtifact(idea.artifactId ?? ''), { variant: 'link', icon: 'file' }) : null), menu(`Manage ${idea.title}`, [{ label: 'See what’s included', icon: 'expand', action: () => openIdea(ctx, idea.id) }, { label: idea.state === 'dismissed' ? 'Restore idea' : 'Dismiss idea', icon: idea.state === 'dismissed' ? 'refresh' : 'close', action: () => { void ctx.run({ type: idea.state === 'dismissed' ? 'idea.restore' : 'idea.dismiss', id: idea.id }); } }])));
    }
    page.append(h('p', { class: 'sample-footnote' }, ctx.state.mode === 'live' ? 'Ideas Hatchling suggests, in its own words. Nothing runs until you start one in your chat.' : 'Suggestions are grounded in labeled sample facts. No personal data was inferred.'));
    return page;
}
function openIdea(ctx: AppContext, id: string): void {
    const idea = ctx.state.ideas.find((item) => item.id === id);
    if (!idea)
        return;
    const live = ctx.state.mode === 'live';
    const body = h('div', { class: 'stack idea-detail' }, h('span', { class: 'idea-pictogram large' }, pictogram(idea.icon)), pill(idea.domain), h('p', {}, idea.summary), h('h3', {}, 'Why this could be useful'), h('p', { class: 'muted' }, idea.rationale), h('div', { class: 'signal-note' }, icon('memory', 17), idea.signal), ...(idea.items.length || !live ? [h('h3', {}, 'What I’ll make'), ...idea.items.map((item) => h('div', { class: 'included-item' }, icon('file', 18), h('div', {}, h('strong', {}, item.title), h('p', { class: 'muted text-small' }, item.summary))))] : []));
    if (idea.outcome)
        body.append(h('p', { class: 'notice' }, idea.outcome));
    const ref = ctx.dialogs.open(idea.title, body);
    if (live && (idea.state === 'suggested' || idea.state === 'activated')) {
        body.append(h('div', { class: 'action-row' }, button('Let’s do this', () => {
            void ctx.run({ type: 'idea.activate', id }, true).then((result) => {
                if (!result.ok)
                    return;
                ref.close();
                ctx.prefillComposer(`${idea.title}: ${idea.summary}`);
            });
        }, { icon: 'spark', variant: 'primary' })));
        body.append(h('p', { class: 'muted text-small' }, 'This puts it in your chat box, as Hatchling wrote it. Read it, change it, and send it when you’re ready.'));
    }
    else if (idea.state === 'suggested' || idea.state === 'needs_user' || idea.state === 'failed')
        body.append(h('div', { class: 'action-row' }, button('Adjust scope', () => {
            const form = h('form', { class: 'stack' }, textareaField('What to include', 'summary', idea.summary), h('button', { class: 'button primary', type: 'submit' }, 'Save scope'));
            const edit = ctx.dialogs.open('Make it your own', form);
            form.addEventListener('submit', (event) => {
                event.preventDefault();
                void ctx.run({ type: 'idea.edit', id, summary: valueOf(form, 'summary') }).then((result) => {
                    if (result.ok) {
                        edit.close();
                        ref.close();
                        openIdea(ctx, id);
                    }
                });
            });
        }, { icon: 'edit' }), button('Let’s do this', () => { void ctx.run({ type: 'idea.activate', id }); ref.close(); ctx.ui.ideaFilter = 'active'; ctx.render(); }, { icon: 'spark', variant: 'primary' })));
    else if (idea.artifactId)
        body.append(button('Open completed artifact', () => ctx.openArtifact(idea.artifactId ?? ''), { variant: 'primary', icon: 'file' }));
    else
        body.append(h('p', { class: 'notice' }, idea.outcome || 'Restore this idea from Dismissed to activate it.'));
}
