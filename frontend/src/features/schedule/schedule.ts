import type { AppContext } from '../../app/context.js';
import { h, valueOf } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, emptyState, field, infoRow, menu, pill, selectField, tabs, toggle } from '../../components/ui.js';
import { attentionSection } from '../context/schedules.js';
export function renderSchedule(ctx: AppContext): HTMLElement {
    const page = h('div', { class: 'page schedule-page reading-wide', 'data-scroll': 'schedule' }, h('header', { class: 'page-header' }, h('h1', {}, 'Scheduled work'), button('New schedule', () => createJob(ctx), { icon: 'plus', variant: 'primary' })), h('p', { class: 'page-intro muted' }, 'A clear time, a useful result, and a stopping point.'), tabs([{ id: 'all', label: 'All work' }, { id: 'time', label: 'Time-based' }, { id: 'hook', label: 'Event hooks' }], ctx.ui.scheduleFilter, (id) => { ctx.ui.scheduleFilter = id; ctx.render(); }));
    const attention = attentionSection(ctx);
    if (attention)
        page.append(attention);
    const jobs = ctx.state.jobs.filter((job) => ctx.ui.scheduleFilter === 'all' || job.kind === ctx.ui.scheduleFilter);
    for (const job of jobs)
        page.append(h('article', { class: 'job-card' }, h('header', {}, h('span', { class: 'feature-icon' }, icon(job.kind === 'hook' ? 'link' : 'clock', 22)), h('div', { class: 'row-copy' }, h('h2', {}, job.title), h('p', { class: 'muted text-small' }, job.schedule)), pill(job.health, job.health === 'ok' ? 'success' : job.health === 'errored' ? 'failed' : ''), menu(`Manage ${job.title}`, [{ label: 'Edit schedule', icon: 'edit', action: () => createJob(ctx, job.id) }, { label: job.health === 'disabled' ? 'Enable schedule' : 'Pause schedule', icon: job.health === 'disabled' ? 'play' : 'pause', action: () => { void ctx.run({ type: 'job.toggle', id: job.id }); } }, { label: 'Run sample now', icon: 'play', disabled: job.health !== 'ok', action: () => ctx.dialogs.confirm('Run this sample job?', 'This creates a local demonstration result and records its delivery. A one-shot job then disables itself.', () => { void ctx.run({ type: 'job.run', id: job.id }); }, { label: 'Run sample' }) }, { label: 'Remove schedule', icon: 'trash', danger: true, action: () => ctx.dialogs.confirm('Remove scheduled work?', 'Future runs stop. Previously delivered results stay in the delivery ledger.', () => { void ctx.run({ type: 'job.delete', id: job.id }); }, { danger: true, label: 'Remove schedule' }) }])), h('p', { class: 'job-limit' }, icon('shield', 14), job.limit), job.kind === 'hook' ? h('p', { class: 'text-small muted' }, `${job.source} → ${job.event}`) : null, h('div', { class: 'job-dates' }, h('div', {}, h('span', { class: 'eyebrow' }, 'Last occurrence'), h('p', {}, job.lastRun)), h('div', {}, h('span', { class: 'eyebrow' }, 'Next'), h('p', {}, job.nextRun)))));
    if (!jobs.length)
        page.append(emptyState('clock', 'No scheduled work in this view.'));
    page.append(h('div', { class: 'notice muted' }, icon('clock', 20), ctx.state.mode === 'live' ? 'Hatchling runs these whether or not this page is open. Results arrive in your chat.' : 'Schedules are local fixtures. This frontend does not execute background work when the page is closed.'), button('Notification preferences', () => notificationPreferences(ctx), { icon: 'bell' }));
    return page;
}
/** How often time-based work repeats, as the form offers it. */
type Repeat = 'once' | 'minutes' | 'daily' | 'weekdays' | 'weekly' | 'cron';
const REPEATS: readonly {
    value: Repeat;
    label: string;
}[] = [
    { value: 'once', label: 'Once' },
    { value: 'minutes', label: 'Every few minutes' },
    { value: 'daily', label: 'Every day' },
    { value: 'weekdays', label: 'Every weekday' },
    { value: 'weekly', label: 'Every week' },
    { value: 'cron', label: 'Custom (cron)' },
];
const DAYS: readonly {
    value: string;
    label: string;
}[] = [
    { value: 'mon', label: 'Monday' }, { value: 'tue', label: 'Tuesday' }, { value: 'wed', label: 'Wednesday' },
    { value: 'thu', label: 'Thursday' }, { value: 'fri', label: 'Friday' }, { value: 'sat', label: 'Saturday' }, { value: 'sun', label: 'Sunday' },
];
/** Tomorrow's date on the person's clock, as a date input writes it. */
function tomorrow(): string {
    const day = new Date(Date.now() + 86400000);
    return `${day.getFullYear()}-${String(day.getMonth() + 1).padStart(2, '0')}-${String(day.getDate()).padStart(2, '0')}`;
}
/**
 * The timing the form's fields describe, in the one form the adapters read
 * (`once 2026-11-04 08:00`, `every 5 minutes`, `daily 08:00`, `weekdays 08:00`,
 * `weekly mon 08:00`, `cron <five fields>`), and the same in words.
 */
export function composeWhen(repeat: Repeat, fields: {
    date: string;
    time: string;
    minutes: string;
    day: string;
    cron: string;
}): {
    when: string;
    words: string;
} {
    const day = DAYS.find((item) => item.value === fields.day)?.label ?? fields.day;
    switch (repeat) {
        case 'once': return { when: `once ${fields.date} ${fields.time}`, words: `once, on ${fields.date} at ${fields.time}` };
        case 'minutes': return { when: `every ${fields.minutes} minutes`, words: `every ${fields.minutes} minutes` };
        case 'daily': return { when: `daily ${fields.time}`, words: `every day at ${fields.time}` };
        case 'weekdays': return { when: `weekdays ${fields.time}`, words: `every weekday at ${fields.time}` };
        case 'weekly': return { when: `weekly ${fields.day} ${fields.time}`, words: `every ${day} at ${fields.time}` };
        case 'cron': return { when: `cron ${fields.cron.trim()}`, words: `on the cron schedule ${fields.cron.trim()}` };
    }
}
/** One editor for the compact context pane and the full workspace. Recurrence is always reapproved. */
export function createJob(ctx: AppContext, id?: string): void {
    const existing = ctx.state.jobs.find((job) => job.id === id);
    let kind: 'time' | 'hook' = existing?.kind ?? 'time';
    let recurring = existing?.recurring ?? false;
    let repeat: Repeat = existing?.recurring ? 'daily' : 'once';
    const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
    const draft = { title: existing?.title ?? '', schedule: existing?.schedule ?? '', source: existing?.source ?? 'GitHub', event: existing?.event ?? 'Pull request opened', date: tomorrow(), time: '09:00', minutes: '15', day: 'mon', cron: '' };
    const body = h('div', { class: 'stack' });
    const ref = ctx.dialogs.open(existing ? 'Edit schedule' : 'Arrange a useful result', body);
    const render = (): void => {
        const repeats = (): boolean => kind === 'time' ? repeat !== 'once' : recurring;
        const approvalLabel = (): string => repeats() ? (existing ? 'Save and approve this recurrence' : 'Create and approve this recurrence') : (existing ? 'Save one run only' : 'Create one run only');
        const form = h('form', { class: 'stack' }, tabs([{ id: 'time', label: 'At a time' }, { id: 'hook', label: 'When something changes' }], kind, (next) => { kind = next; render(); }), field('What should happen?', 'title', draft.title, { required: true, placeholder: kind === 'time' ? 'Check the disk is under 90% full and tell me if not' : 'Prepare a reading shortlist' }));
        const approval = h('button', { class: 'button primary', type: 'submit' }, approvalLabel());
        if (kind === 'time') {
            const repeatField = selectField('Repeat', 'repeat', REPEATS, repeat);
            repeatField.addEventListener('change', () => { repeat = REPEATS.find((item) => item.value === valueOf(form, 'repeat'))?.value ?? 'once'; render(); });
            form.append(repeatField);
            if (repeat === 'once')
                form.append(h('div', { class: 'field-grid' }, field('Date', 'date', draft.date, { type: 'date', required: true }), field('Time', 'time', draft.time, { type: 'time', required: true })));
            else if (repeat === 'minutes')
                form.append(selectField('Every', 'minutes', ['5', '10', '15', '30'].map((value) => ({ value, label: `${value} minutes` })), draft.minutes));
            else if (repeat === 'weekly')
                form.append(h('div', { class: 'field-grid' }, selectField('Day', 'day', DAYS, draft.day), field('Time', 'time', draft.time, { type: 'time', required: true })));
            else if (repeat === 'cron')
                form.append(field('Cron expression · minute hour day month weekday', 'cron', draft.cron, { required: true, placeholder: '0 8 * * 1-5' }));
            else
                form.append(field('Time', 'time', draft.time, { type: 'time', required: true }));
            form.append(h('p', { class: 'muted text-small' }, `Times are in ${zone}, this browser's time zone.`), h('p', { class: 'notice text-small' }, 'The timing and repeat above are the only work you authorize. Routine or unchanged results stay silent.'), approval);
        }
        else {
            form.append(field('Delivery timing', 'schedule', draft.schedule, { required: true, placeholder: 'When the chosen event arrives' }), selectField('Connected source', 'source', ctx.state.connectors.filter((item) => item.state === 'connected').map((item) => ({ value: item.name, label: item.name })), draft.source), selectField('Event', 'event', [{ value: 'Pull request opened', label: 'Pull request opened' }, { value: 'New message', label: 'New message' }, { value: 'File changed', label: 'File changed' }], draft.event));
            form.append(toggle('Repeat this schedule', recurring, (value) => { recurring = value; approval.textContent = approvalLabel(); }, 'Off means exactly one delivery, then the job is disabled.'), h('p', { class: 'notice text-small' }, 'The timing and recurrence above are the only work you authorize. Routine or unchanged results stay silent.'), approval);
        }
        const remember = (): void => {
            draft.title = valueOf(form, 'title');
            if (kind === 'time') {
                for (const name of ['date', 'time', 'minutes', 'day', 'cron'] as const) {
                    if (form.elements.namedItem(name))
                        draft[name] = valueOf(form, name);
                }
            }
            else {
                draft.schedule = valueOf(form, 'schedule');
                draft.source = valueOf(form, 'source');
                draft.event = valueOf(form, 'event');
            }
        };
        form.addEventListener('input', remember);
        form.addEventListener('change', remember);
        form.addEventListener('submit', (event) => {
            event.preventDefault();
            remember();
            const timing = kind === 'time' ? composeWhen(repeat, draft) : { when: draft.schedule, words: draft.schedule };
            const submit = (): void => {
                const fields = { title: draft.title, kind, schedule: timing.when, recurring: repeats(), source: kind === 'hook' ? draft.source : '', event: kind === 'hook' ? draft.event : '' };
                void ctx.run(existing ? { type: 'job.update', id: existing.id, ...fields } : { type: 'job.create', ...fields }).then((result) => {
                    if (result.ok)
                        ref.close();
                });
            };
            if (repeats())
                ctx.dialogs.confirm('Approve recurring work?', `Run “${draft.title}” ${timing.words}${kind === 'time' ? ` (${zone})` : ''}, until you remove it. This is separate from approval for a single run.`, submit, { label: 'Approve recurrence' });
            else
                submit();
        });
        body.replaceChildren(form);
    };
    render();
}
export function renderNotifications(ctx: AppContext): HTMLElement {
    const page = h('div', { class: 'page notifications-page reading-wide', 'data-scroll': 'notifications' }, h('header', { class: 'page-header' }, h('h1', {}, 'Your daily digest'), button('Preferences', () => notificationPreferences(ctx), { icon: 'settings' })), h('p', { class: 'page-intro muted' }, `One edition around ${ctx.state.settings.digestTime}. Urgent items arrive on their own.`));
    const delivered = ctx.state.notifications.filter((item) => item.stage === 'delivered');
    page.append(h('h2', { class: 'feed-date' }, 'Wednesday, September 23'));
    for (const item of delivered)
        page.append(h('article', { class: `digest-card ${item.read ? 'read' : ''}` }, h('header', {}, h('span', { class: 'row-icon' }, icon(item.urgent ? 'warning' : 'bell', 20)), h('h3', {}, item.title), h('time', { class: 'tiny muted' }, item.time)), h('p', {}, item.text), h('div', { class: 'action-row' }, item.evidenceId ? button('Open evidence', () => ctx.openThread(item.evidenceId ?? ''), { icon: 'file', variant: 'ghost' }) : null, item.offer ? button(item.offer, () => { void ctx.run({ type: 'notification.answer', id: item.id }); ctx.navigate('chat'); }, { variant: 'primary' }) : null, !item.read ? button('Mark read', () => { void ctx.run({ type: 'notification.read', id: item.id }, true); }, { icon: 'check', variant: 'ghost' }) : pill('Read'))));
    const candidates = ctx.state.notifications.filter((item) => item.stage === 'candidate');
    const staging = h('details', { class: 'digest-staging' }, h('summary', {}, `${candidates.length} verified candidate awaiting the next quiet moment`), ...candidates.map((item) => infoRow(item.title, item.text, 'clock')), h('p', { class: 'muted text-small' }, 'Producer → verified candidate → daily composer → delivery ledger. No duplicate delivery is created from an already delivered item.'));
    page.append(staging, h('h2', { class: 'section-heading' }, 'Delivery ledger'), h('div', { class: 'ledger' }, ...delivered.map((item) => h('div', {}, h('time', {}, item.time), h('span', {}, item.title), pill(item.urgent ? 'Urgent · sent alone' : 'Daily edition')))));
    return page;
}
export function notificationPreferences(ctx: AppContext): void {
    const settings = ctx.state.settings;
    const form = h('form', { class: 'stack' }, field('Daily edition time', 'time', settings.digestTime, { type: 'time', required: true }), h('div', { class: 'field-grid' }, field('Quiet hours begin', 'start', settings.quietStart, { type: 'time' }), field('Waking hours begin', 'end', settings.quietEnd, { type: 'time' })), field('Daily allowance', 'allowance', String(settings.dailyAllowance), { type: 'number', min: 1, max: 10 }), h('p', { class: 'muted text-small' }, 'Regular editions wait for waking hours and a quiet moment. Urgent items go immediately and alone. No-op updates stay silent.'), h('button', { class: 'button primary', type: 'submit' }, 'Save preferences'));
    const ref = ctx.dialogs.open('Notification preferences', form);
    form.addEventListener('submit', (event) => {
        event.preventDefault();
        void ctx.run({ type: 'settings.digest', time: valueOf(form, 'time'), start: valueOf(form, 'start'), end: valueOf(form, 'end'), allowance: Number(valueOf(form, 'allowance')) }).then((result) => {
            if (result.ok)
                ref.close();
        });
    });
}
