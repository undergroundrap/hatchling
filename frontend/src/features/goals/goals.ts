import type { AppContext } from '../../app/context.js';
import type { DeepReadonly, Goal, GoalCategory, IconName } from '../../protocol/models.js';
import { h, valueOf } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, emptyState, field, menu, pill, textareaField } from '../../components/ui.js';
const categories: readonly {
    name: GoalCategory;
    label: string;
    icon: IconName;
}[] = [
    { name: 'Health & fitness', label: 'Health', icon: 'heart' },
    { name: 'Relationships', label: 'Relationships', icon: 'people' },
    { name: 'Money', label: 'Finance', icon: 'currency' },
    { name: 'Career', label: 'Career', icon: 'briefcase' },
    { name: 'Interests', label: 'Interests', icon: 'palette' },
    { name: 'Productivity', label: 'Productivity', icon: 'laptop' },
    { name: 'Something else', label: 'Something else', icon: 'goal' },
];
export function renderGoals(ctx: AppContext): HTMLElement {
    // Connected to Hatchling, the goals are the broker's: set and changed
    // here, and read by the agent; no sample goals.
    const live = ctx.state.mode === 'live';
    const options = menu('Goal options', live ? [
        { label: 'New goal', icon: 'plus', action: () => createGoal(ctx) },
        { label: ctx.ui.goalFilter === 'active' ? 'Closed goals' : 'Active goals', icon: 'check', action: () => { ctx.ui.goalFilter = ctx.ui.goalFilter === 'active' ? 'closed' : 'active'; ctx.render(); } },
    ] : [
        { label: 'New goal', icon: 'plus', action: () => createGoal(ctx) },
        { label: ctx.ui.goalFilter === 'active' ? 'Closed goals' : 'Active goals', icon: 'check', action: () => { ctx.ui.goalFilter = ctx.ui.goalFilter === 'active' ? 'closed' : 'active'; ctx.render(); } },
        { label: ctx.ui.goalExamples ? 'Hide demonstration goals' : 'Show demonstration goals', icon: 'play', action: () => { ctx.ui.goalExamples = !ctx.ui.goalExamples; ctx.render(); } },
        { label: 'About this workspace', icon: 'help', action: () => { ctx.dialogs.open('Sample workspace', h('p', {}, 'The initial tracking rows are synthetic examples, not your current tasks or project results. New goals and lifecycle changes are local to the scripted adapter.')); } },
    ]);
    const page = h('div', { class: 'page goals-page', 'data-scroll': 'goals' }, h('header', { class: 'page-header' }, h('h1', {}, ctx.ui.goalFilter === 'closed' ? 'Closed goals' : 'Goals'), options));
    const goals = ctx.state.goals.filter((goal) => (!goal.sampleOnly || ctx.ui.goalExamples) && (goal.status === 'closed') === (ctx.ui.goalFilter === 'closed'));
    for (const kind of ['user', 'tracking'] as const) {
        const matches = goals.filter((goal) => goal.kind === kind);
        if (!matches.length)
            continue;
        const group = h('section', { class: 'goal-group', 'aria-label': kind === 'tracking' ? 'Tracking' : 'Your goals' }, h('h2', { class: 'tracking-heading' }, h('span', { class: 'tracking-status', 'aria-hidden': 'true' }), kind === 'tracking' ? 'Tracking' : 'Your goals'));
        group.append(...matches.map((goal) => goalRow(ctx, goal)));
        page.append(group);
    }
    if (!goals.length && ctx.ui.goalFilter === 'closed')
        page.append(emptyState('goal', 'No closed goals yet.'));
    if (live && ctx.ui.goalFilter === 'active' && !goals.length) {
        // Not "yet" when some are closed: they're kept, in Closed goals.
        const closed = ctx.state.goals.some((goal) => goal.status === 'closed');
        page.append(emptyState('goal', closed ? 'No open goals.' : 'No goals yet.', closed ? 'The ones you closed are in Closed goals, from the menu. Set a new one below.' : 'Set one below: what you’re working toward, in a line. Your digests can include it.'));
    }
    if (ctx.ui.goalFilter === 'active') {
        page.append(h('section', { class: 'goal-categories', 'aria-label': 'Create a goal' }, h('h2', {}, 'Create a goal'), ...categories.map((category) => {
            const node = button(category.label, () => createGoal(ctx, category.name), { icon: category.icon, variant: 'ghost', className: 'goal-category' });
            node.append(icon('chevron', 16));
            return node;
        })));
    }
    return page;
}
function goalRow(ctx: AppContext, goal: DeepReadonly<Goal>): HTMLElement {
    // Connected to Hatchling a closed goal stays closed.
    const final = ctx.state.mode === 'live' && goal.status === 'closed';
    const checkbox = h('input', { type: 'checkbox', class: 'goal-checkbox', checked: goal.status === 'closed', disabled: final, 'aria-label': final ? `${goal.title}: closed` : `${goal.status === 'closed' ? 'Reopen' : 'Close'} ${goal.title}` });
    checkbox.addEventListener('change', () => {
        // Closing is consequential; keep its old state until the confirmation commits.
        checkbox.checked = goal.status === 'closed';
        lifecycle(ctx, goal);
    });
    const main = button(goal.title, () => openGoal(ctx, goal.id), { variant: 'ghost', className: 'row-title-button' });
    const pulse = h('p', { class: 'goal-pulse', 'data-pulse-kind': goal.pulseKind }, goal.pulse);
    return h('article', { class: 'goal-row', 'data-goal': goal.id }, checkbox, h('div', { class: 'row-copy' }, main, pulse), goal.attention ? h('span', { class: 'goal-attention', title: 'Needs your attention', 'aria-label': 'Needs your attention' }, icon('warning', 16)) : null);
}
function lifecycle(ctx: AppContext, goal: DeepReadonly<Goal>): void {
    const port = ctx.goals;
    if (ctx.state.mode === 'live' && port !== undefined) {
        if (goal.status === 'closed')
            return;
        // Closing is final in Hatchling: ask how it ended, and say it stays.
        const body = h('div', { class: 'stack' }, h('p', {}, 'Did you reach it, or are you setting it aside? Either way it stays in Closed goals, and can’t be reopened.'));
        const ref = ctx.dialogs.open(`Close “${goal.title}”`, body, 'confirm-dialog');
        const close = (achieved: boolean): void => {
            ref.close();
            void port.close(goal.id, achieved).then((result) => { ctx.toast(result.message, !result.ok); });
        };
        body.append(h('div', { class: 'action-row' }, button('Set it aside', () => close(false)), button('I reached it', () => close(true), { variant: 'primary' })));
        return;
    }
    if (goal.status === 'closed') {
        void ctx.run({ type: 'goal.reopen', id: goal.id });
        return;
    }
    ctx.dialogs.confirm('Close this responsibility?', 'This stops its monitoring and disables any goal-owned scheduled work. You can reopen it later.', () => { void ctx.run({ type: 'goal.close', id: goal.id }); }, { label: 'Close responsibility' });
}
export function openGoal(ctx: AppContext, id: string): void {
    const goal = ctx.state.goals.find((item) => item.id === id);
    if (!goal)
        return;
    const body = h('div', { class: 'goal-detail stack' }, pill(goal.kind === 'user' ? 'Your goal' : 'Assistant tracking'), h('p', {}, goal.description), h('div', { class: 'notice' }, icon(goal.attention ? 'warning' : 'check', 20), goal.pulse));
    const ref = ctx.dialogs.open(goal.title, body);
    body.append(h('h3', {}, 'Activity timeline'), h('ol', { class: 'timeline' }, ...goal.timeline.map((entry) => h('li', {}, h('time', {}, entry.date), h('p', {}, entry.text)))));
    const jobs = ctx.state.jobs.filter((job) => job.goalId === id);
    if (jobs.length)
        body.append(h('h3', {}, 'Scheduled work'), ...jobs.map((job) => button(`${job.title} · ${job.schedule}`, () => { ref.close(); ctx.navigate('schedule'); }, { icon: 'clock', variant: 'ghost' })));
    if (ctx.state.mode === 'live') {
        if (goal.status === 'open')
            body.append(h('div', { class: 'action-row' }, button('Edit details', () => { ref.close(); editGoal(ctx, goal); }, { icon: 'edit' }), button('Close', () => { ref.close(); lifecycle(ctx, goal); }, { icon: 'check' })));
        return;
    }
    body.append(h('div', { class: 'action-row' }, button('Edit details', () => { ref.close(); editGoal(ctx, goal); }, { icon: 'edit' }), button(goal.status === 'closed' ? 'Reopen' : 'Close responsibility', () => { ref.close(); lifecycle(ctx, goal); }, { icon: 'check' })));
}
function editGoal(ctx: AppContext, goal: DeepReadonly<Goal>): void {
    // Connected to Hatchling a goal has no due date to keep.
    const due = ctx.state.mode === 'live' ? null : field('Due date · optional', 'due', goal.due ?? '', { type: 'date' });
    const form = h('form', { class: 'stack' }, field('Title', 'title', goal.title, { required: true }), textareaField('Description', 'description', goal.description), due, h('button', { class: 'button primary', type: 'submit' }, 'Save changes'));
    const ref = ctx.dialogs.open('Edit goal', form);
    form.addEventListener('submit', (event) => {
        event.preventDefault();
        void ctx.run({ type: 'goal.update', id: goal.id, title: valueOf(form, 'title'), description: valueOf(form, 'description'), due: due === null ? '' : valueOf(form, 'due') }).then((result) => {
            if (result.ok)
                ref.close();
        });
    });
}
function createGoal(ctx: AppContext, initialCategory?: GoalCategory): void {
    const body = h('div', { class: 'goal-wizard' });
    const ref = ctx.dialogs.open('A little direction', body);
    let category: GoalCategory = initialCategory ?? 'Interests';
    let title = '';
    let description = '';
    let step = initialCategory ? 1 : 0;
    // Connected to Hatchling a goal has no due date to keep: the second
    // answer sets it.
    const last = ctx.state.mode === 'live' ? 2 : 3;
    const render = (): void => {
        body.replaceChildren(h('p', { class: 'eyebrow' }, step === 0 ? 'START WITH WHAT MATTERS' : category));
        if (step === 0) {
            body.append(h('h3', {}, 'Which part of life is this for?'), h('div', { class: 'category-picker' }, ...categories.map((item) => button(item.label, () => { category = item.name; step = 1; render(); }, { icon: item.icon }))));
            return;
        }
        const question = step === 1 ? 'What would you like to work toward?' : step === 2 ? 'What would a useful result look like?' : 'Is there a date you’re working toward?';
        const form = h('form', { class: 'stack' }, h('h3', {}, question), step === 2 ? textareaField('Your answer', 'answer', description, 4) : field(step === 3 ? 'Optional due date' : 'Your answer', 'answer', step === 1 ? title : '', { type: step === 3 ? 'date' : 'text', required: step === 1 }), h('div', { class: 'action-row' }, button('Back', () => { step--; render(); }), h('button', { class: 'button primary', type: 'submit' }, step === last ? 'Create goal' : 'Continue')));
        if (step === 3)
            form.append(h('p', { class: 'muted text-small' }, 'No reminder or recurring job will be created unless you approve it separately.'));
        form.addEventListener('submit', (event) => {
            event.preventDefault();
            const answer = valueOf(form, 'answer');
            if (step === 1) {
                title = answer;
                step++;
                render();
            }
            else if (step === 2 && last === 3) {
                description = answer;
                step++;
                render();
            }
            else if (step === 2)
                void ctx.run({ type: 'goal.create', title, description: answer, category, due: '' }).then((result) => {
                    if (result.ok)
                        ref.close();
                });
            else
                void ctx.run({ type: 'goal.create', title, description, category, due: answer }).then((result) => {
                    if (result.ok)
                        ref.close();
                });
        });
        body.append(form);
        body.querySelector<HTMLInputElement | HTMLTextAreaElement>('input,textarea')?.focus();
    };
    render();
}
