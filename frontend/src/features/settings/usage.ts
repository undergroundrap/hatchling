// Settings' Usage, connected to Hatchling: what the model really did, as the
// broker counted it, against the limits the owner set. Today and this month are
// meters when a limit is set; each of the last seven days is a bar, scaled to the
// busiest; the week's total is written out, and each model's share. The limits
// form sets both at once.
import type { AppContext } from '../../app/context.js';
import { h, valueOf } from '../../components/dom.js';
import { field } from '../../components/ui.js';
import type { Usage, UsageDay, UsageLimits, UsagePort } from '../../session/usage.js';
const COMPACT = new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 1 });
/** `n` tokens or calls, short: 48.2K, 1.3M. */
export function compact(n: number): string {
    return COMPACT.format(n);
}
const plural = (n: number, one: string, many: string): string => `${compact(n)} ${n === 1 ? one : many}`;
const tokensOf = (day: {
    readonly inputTokens: number;
    readonly outputTokens: number;
}): number => day.inputTokens + day.outputTokens;
/** The day `date` (`YYYY-MM-DD`) as a short weekday, read at noon so no clock change moves it. */
export function weekday(date: string): string {
    const at = new Date(`${date}T12:00:00`);
    return Number.isNaN(at.getTime()) ? date : at.toLocaleDateString('en', { weekday: 'short' });
}
/** One line of what a span of days added up to. */
export function summary(days: readonly UsageDay[]): string {
    const total = days.reduce((sum, day) => ({ calls: sum.calls + day.calls, inputTokens: sum.inputTokens + day.inputTokens, outputTokens: sum.outputTokens + day.outputTokens, unreported: sum.unreported + day.unreported }), { calls: 0, inputTokens: 0, outputTokens: 0, unreported: 0 });
    if (total.calls === 0)
        return 'No model calls.';
    const unreported = total.unreported ? ` · ${plural(total.unreported, 'call', 'calls')} didn’t report usage` : '';
    return `${plural(total.calls, 'model call', 'model calls')} · ${compact(total.inputTokens)} read · ${compact(total.outputTokens)} written${unreported}`;
}
/** The most a limit may be, as the broker keeps it. */
const MAX_LIMIT = 1e15;
const SUFFIXES: Readonly<Record<string, number>> = { '': 1, k: 1e3, m: 1e6, b: 1e9 };
/**
 * A limit as the owner wrote it: `null` for none (left empty), `undefined` for
 * what isn't one. Thousands may be set apart, and K, M or B multiply.
 */
export function parseTokens(text: string): number | null | undefined {
    const written = text.trim().toLowerCase().replace(/[,_\s]/g, '');
    if (written === '')
        return null;
    const match = /^(\d+(?:\.\d+)?)([kmb]?)$/.exec(written);
    const tokens = match ? Math.round(Number(match[1]) * (SUFFIXES[match[2] ?? ''] ?? 1)) : NaN;
    return Number.isSafeInteger(tokens) && tokens >= 1 && tokens <= MAX_LIMIT ? tokens : undefined;
}
/** A limit as the form shows it: short when it's whole, `''` for none. */
export function tokensText(limit: number | null): string {
    if (limit === null)
        return '';
    for (const [suffix, size] of [['B', 1e9], ['M', 1e6], ['K', 1e3]] as const) {
        if (limit >= size && limit % size === 0)
            return `${limit / size}${suffix}`;
    }
    return String(limit);
}
/** The month `YYYY-MM` by name, read mid-month so no clock moves it. */
export function monthName(month: string): string {
    const at = new Date(`${month}-15T12:00:00`);
    return Number.isNaN(at.getTime()) ? 'This month' : at.toLocaleDateString('en', { month: 'long' });
}
/** A span's tokens, and against its limit when one is set: a meter that says when it turns. */
function spanMeter(label: string, spent: number, limit: number | null, turns: string, detail: string | null): HTMLElement {
    const used = limit === null ? plural(spent, 'token', 'tokens') : `${compact(spent)} of ${compact(limit)} tokens`;
    const meter = h('div', { class: 'usage-meter' }, h('div', { class: 'split-row' }, h('span', {}, label), h('span', { class: 'quota-used' }, used)));
    if (detail !== null)
        meter.append(h('p', {}, detail));
    if (limit !== null) {
        const reached = spent >= limit;
        meter.classList.toggle('reached', reached);
        meter.append(h('meter', { min: 0, max: limit, value: Math.min(spent, limit), 'aria-label': `${label}: ${used}` }), h('p', { role: reached ? 'status' : undefined }, reached ? `Limit reached. Hatchling won’t call the model until ${turns}, unless you raise it.` : `The limit turns ${turns === 'midnight' ? 'at midnight' : 'on the 1st'}.`));
    }
    return meter;
}
/** The limits form: both at once, each empty for none. */
function limitsForm(ctx: AppContext, port: UsagePort, limits: UsageLimits): HTMLElement {
    const form = h('form', { class: 'stack usage-limits', 'aria-label': 'Usage limits' }, h('p', {}, 'Set the most tokens Hatchling may use. Past a limit it refuses every model call, for chats and scheduled work alike, until the limit turns. A call already under way finishes. Write 500K or 2M; leave one empty for no limit.'), field('Most tokens a day', 'daily', tokensText(limits.daily), { placeholder: 'No limit' }), field('Most tokens a month', 'monthly', tokensText(limits.monthly), { placeholder: 'No limit' }), h('div', { class: 'feed-prompt-actions' }, h('button', { class: 'button primary', type: 'submit' }, 'Save limits')));
    form.addEventListener('submit', (event) => {
        event.preventDefault();
        const daily = parseTokens(valueOf(form, 'daily'));
        const monthly = parseTokens(valueOf(form, 'monthly'));
        if (daily === undefined || monthly === undefined) {
            ctx.toast('Write a number of tokens, like 500K or 2M, or leave it empty for no limit.', true);
            return;
        }
        void port.setLimits({ daily, monthly }).then((result) => {
            ctx.toast(result.message, !result.ok);
            ctx.render();
        });
    });
    return form;
}
/** The seven days as bars, oldest first, each scaled to the busiest. */
function bars(days: readonly UsageDay[]): HTMLElement {
    const busiest = Math.max(1, ...days.map(tokensOf));
    const chart = h('div', { class: 'usage-days', role: 'img', 'aria-label': `Tokens each day, the last ${days.length} days` });
    [...days].reverse().forEach((day, at, all) => {
        const tokens = tokensOf(day);
        const label = `${weekday(day.date)} ${day.date}: ${plural(tokens, 'token', 'tokens')}, ${plural(day.calls, 'model call', 'model calls')}`;
        const height = tokens === 0 ? 0 : Math.max(4, Math.round((tokens / busiest) * 100));
        chart.append(h('div', { class: `usage-day${at === all.length - 1 ? ' today' : ''}`, title: label }, h('span', { class: 'usage-track' }, h('span', { class: 'usage-bar', style: `height: ${height}%` })), h('span', { class: 'usage-day-label' }, at === all.length - 1 ? 'Today' : weekday(day.date))));
    });
    return chart;
}
function counted(usage: Usage): HTMLElement[] {
    const today = usage.days.slice(0, 1);
    const week = usage.days;
    const weekTokens = week.reduce((sum, day) => sum + tokensOf(day), 0);
    const parts = [
        spanMeter('Today', usage.spent.today, usage.limits.daily, 'midnight', summary(today)),
        spanMeter(monthName(usage.spent.monthOf), usage.spent.month, usage.limits.monthly, 'the 1st', null),
        bars(week),
        h('div', { class: 'usage-meter' }, h('div', { class: 'split-row' }, h('span', {}, `Last ${week.length} days`), h('span', { class: 'quota-used' }, plural(weekTokens, 'token', 'tokens'))), h('p', {}, summary(week))),
    ];
    if (usage.models.length) {
        const share = (tokens: number): string => (weekTokens ? `${Math.round((tokens / weekTokens) * 100)}%` : plural(0, 'token', 'tokens'));
        parts.push(h('ul', { class: 'usage-models' }, ...usage.models.map(model => h('li', { class: 'split-row' }, h('span', {}, model.model), h('span', { class: 'muted' }, `${share(tokensOf(model))} · ${plural(model.calls, 'call', 'calls')}`)))));
    }
    return parts;
}
/** The Usage section, from what `ctx`'s usage port last read. */
export function liveUsage(ctx: AppContext): HTMLElement {
    const port = ctx.usage;
    const state = port?.current();
    const group = h('div', { class: 'settings-group usage-group' });
    if (state === undefined || state.kind === 'unread')
        group.append(h('p', {}, 'Reading what your model used…'));
    else if (state.kind === 'failed')
        group.append(h('p', {}, `Hatchling couldn’t say what your model used: ${state.detail}`));
    else if (!state.usage.counted)
        group.append(h('p', {}, 'Hatchling counts model usage when scheduling is on (HATCHLING_SCHEDULER_DIR): every chat runs on it.'));
    else
        group.append(...counted(state.usage), h('p', { class: 'usage-note' }, 'Counted by Hatchling from what your model server reports with each reply: chats, scheduled work and subagents. A call whose server reports no usage counts nothing toward a limit.'));
    const section = h('section', { class: 'general-section' }, h('h3', {}, 'Usage'), group);
    if (port && state?.kind === 'read' && state.usage.counted)
        section.append(h('h3', {}, 'Limits'), h('div', { class: 'settings-group usage-group' }, limitsForm(ctx, port, state.usage.limits)));
    return section;
}
