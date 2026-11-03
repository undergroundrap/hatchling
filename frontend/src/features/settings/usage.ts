// Settings' Usage, connected to Hatchling: what the model really did, as the
// broker counted it. Each of the last seven days is a bar, scaled to the busiest;
// today's and the week's totals are written out, and each model's share.
import { h } from '../../components/dom.js';
import type { Usage, UsageDay, UsageState } from '../../session/usage.js';
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
        h('div', { class: 'usage-meter' }, h('div', { class: 'split-row' }, h('span', {}, 'Today'), h('span', { class: 'quota-used' }, plural(today.reduce((sum, day) => sum + tokensOf(day), 0), 'token', 'tokens'))), h('p', {}, summary(today))),
        bars(week),
        h('div', { class: 'usage-meter' }, h('div', { class: 'split-row' }, h('span', {}, `Last ${week.length} days`), h('span', { class: 'quota-used' }, plural(weekTokens, 'token', 'tokens'))), h('p', {}, summary(week))),
    ];
    if (usage.models.length) {
        const share = (tokens: number): string => (weekTokens ? `${Math.round((tokens / weekTokens) * 100)}%` : plural(0, 'token', 'tokens'));
        parts.push(h('ul', { class: 'usage-models' }, ...usage.models.map(model => h('li', { class: 'split-row' }, h('span', {}, model.model), h('span', { class: 'muted' }, `${share(tokensOf(model))} · ${plural(model.calls, 'call', 'calls')}`)))));
    }
    return parts;
}
/** The Usage section, from `state`. */
export function liveUsage(state: UsageState | undefined): HTMLElement {
    const group = h('div', { class: 'settings-group usage-group' });
    if (state === undefined || state.kind === 'unread')
        group.append(h('p', {}, 'Reading what your model used…'));
    else if (state.kind === 'failed')
        group.append(h('p', {}, `Hatchling couldn’t say what your model used: ${state.detail}`));
    else if (!state.usage.counted)
        group.append(h('p', {}, 'Hatchling counts model usage when scheduling is on (HATCHLING_SCHEDULER_DIR): every chat runs on it.'));
    else
        group.append(...counted(state.usage), h('p', { class: 'usage-note' }, 'Counted by Hatchling from what your model server reports with each reply: chats, scheduled work and subagents. There’s no plan or quota; limits you set come next.'));
    return h('section', { class: 'general-section' }, h('h3', {}, 'Usage'), group);
}
