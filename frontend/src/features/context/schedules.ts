import type { AppContext } from '../../app/context.js';
import type { DeepReadonly, ScheduledJob } from '../../protocol/models.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, iconButton, pill } from '../../components/ui.js';
import { createJob } from '../schedule/schedule.js';
/** Which heading a schedule sits under: one that runs once, by its day of the week, or more often; or on an event. */
export function scheduleGroup(job: DeepReadonly<ScheduledJob>): 'Once' | 'Daily' | 'Weekly' | 'Events' {
    if (job.kind === 'hook')
        return 'Events';
    if (!job.recurring)
        return 'Once';
    // "Weekdays" runs more than once a week: it isn't weekly.
    return /\bweek(ly)?\b|sunday|monday|tuesday|wednesday|thursday|friday|saturday/i.test(job.schedule) ? 'Weekly' : 'Daily';
}
function scheduleLabel(job: DeepReadonly<ScheduledJob>): string {
    if (job.health === 'disabled')
        return 'Paused';
    if (job.health === 'errored')
        return 'Needs attention';
    return /Every \d|Every hour/i.test(job.schedule) ? job.schedule : job.nextRun;
}
function scheduleIcon(job: DeepReadonly<ScheduledJob>): HTMLElement {
    const heartbeat = job.title.toLowerCase() === 'heartbeat';
    const calendar = !/Every \d|Every hour/i.test(job.schedule);
    if (heartbeat)
        return h('span', { class: 'schedule-symbol heartbeat' }, icon('heart', 17));
    if (job.kind === 'hook')
        return h('span', { class: 'schedule-symbol event' }, icon('link', 23));
    return h('span', { class: `schedule-symbol ${calendar ? 'calendar' : 'clock-face'}` }, icon(calendar ? 'calendar' : 'clock', 32), calendar ? h('span', { class: 'schedule-badge' }, icon('clock', 12)) : null);
}
export function renderContextSchedules(ctx: AppContext): HTMLElement {
    const root = h('div', { class: 'context-schedules' });
    let first = true;
    for (const group of ['Once', 'Daily', 'Weekly', 'Events']) {
        const jobs = ctx.state.jobs.filter((job) => job.goalId === null && scheduleGroup(job) === group);
        if (!jobs.length)
            continue;
        // The first heading shown carries the way to add more.
        const section = h('section', { class: 'schedule-section' }, h('div', { class: 'context-section-heading' }, h('h3', {}, group), first ? iconButton('plus', 'Add scheduled work', () => createJob(ctx), 'context-heading-action') : null));
        first = false;
        for (const job of jobs) {
            const row = h('button', { type: 'button', class: `context-job ${job.health}`, 'aria-label': `Open schedule: ${job.title}` }, scheduleIcon(job), h('span', { class: 'context-job-copy' }, h('strong', {}, job.title), h('span', {}, scheduleLabel(job))));
            row.addEventListener('click', () => openJobDetail(ctx, job.id));
            section.append(row);
        }
        root.append(section);
    }
    if (!ctx.state.jobs.length)
        root.append(h('p', { class: 'muted' }, 'No scheduled work yet.'), button('Add scheduled work', () => createJob(ctx), { icon: 'plus' }));
    root.append(button('Manage all scheduled work', () => ctx.navigate('schedule'), { variant: 'ghost', className: 'context-manage-link', icon: 'clock' }));
    return root;
}
export function openJobDetail(ctx: AppContext, id: string): void {
    const job = ctx.state.jobs.find((item) => item.id === id);
    if (!job)
        return;
    const content = h('div', { class: 'stack' }, h('div', { class: 'split-row' }, h('strong', {}, job.schedule), pill(job.health === 'disabled' ? 'Paused' : job.health === 'errored' ? 'Needs attention' : 'Scheduled')), h('p', {}, job.limit), h('dl', { class: 'schedule-detail-dates' }, h('dt', {}, 'Last run'), h('dd', {}, job.lastRun), h('dt', {}, 'Next run'), h('dd', {}, job.nextRun)), h('p', { class: 'notice muted' }, 'Scripted schedule. No background job runs when this frontend is closed.'));
    const ref = ctx.dialogs.open(job.title, content, 'schedule-detail');
    content.append(h('div', { class: 'action-row' }, button('Edit schedule', () => { ref.close(); createJob(ctx, id); }, { icon: 'edit' }), button(job.health === 'disabled' ? 'Resume' : 'Pause', () => { void ctx.run({ type: 'job.toggle', id }); ref.close(); }, { icon: job.health === 'disabled' ? 'play' : 'pause' }), button('Run sample now', () => ctx.dialogs.confirm('Run this sample job?', 'Create one local demonstration result. This does not authorize a new recurrence.', () => { void ctx.run({ type: 'job.run', id }); ref.close(); }, { label: 'Run sample' }), { icon: 'play', disabled: job.health !== 'ok' }), button('Remove schedule', () => ctx.dialogs.confirm('Remove scheduled work?', 'Future sample runs stop. Previously delivered results remain in the ledger.', () => { void ctx.run({ type: 'job.delete', id }); ref.close(); }, { danger: true, label: 'Remove schedule' }), { icon: 'trash', variant: 'danger' })));
}
