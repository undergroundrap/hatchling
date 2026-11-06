// The first-run checklist: each step, and the command for it, to run in a
// terminal the owner opened. It ticks itself: the steps are read from the broker,
// again each time they come back to this page. The chat offers it and never gives
// way to it: what's set up is read after the chat is on screen, and someone may
// already be typing.
import type { AppContext } from '../../app/context.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, emptyState } from '../../components/ui.js';
import { connectionBanner } from '../chat/reliability-ui.js';
import { commandLine, commandRow } from '../settings/live-data.js';
import { setupFinished, setupProgress, setupReminder, setupSteps, STEP_STATUS } from '../../session/setup.js';
import type { SetupStep } from '../../session/setup.js';
const INTRO = 'Run each command in a terminal you opened yourself. Anything secret you type goes there, and never through this app.';
const RESTARTS = 'Each one restarts Hatchling, so this page goes offline for a moment. The command says what to run next, and the address it gives brings you back, with that step ticked.';
const INTRO_WINDOWS = 'Each step is in the Start menu, and ticks itself here when it’s done. Anything secret is typed in the window it opens, and never through this app.';
const RESTARTS_WINDOWS = 'Each one restarts Hatchling, so this page goes offline for a moment. Open Hatchling from the Start menu to come back.';
function nowSeconds(): number {
    return Math.floor(Date.now() / 1000);
}
function stepCard(ctx: AppContext, step: SetupStep, number: number): HTMLElement {
    const mark = step.status === 'done' ? icon('check', 16) : h('span', {}, step.status === 'skipped' ? '–' : String(number));
    const card = h('section', { class: `setup-step is-${step.status}`, 'aria-label': step.title }, h('span', { class: 'setup-mark', 'aria-label': STEP_STATUS[step.status] }, mark), h('div', { class: 'setup-step-body' }, h('h2', {}, step.title), h('p', {}, step.said)));
    const body = card.querySelector('.setup-step-body');
    const row = step.commands.length > 1 ? commandRow : commandLine;
    if (step.commands.length > 0)
        body?.append(h('div', { class: 'settings-group' }, ...step.commands.map(command => row(ctx, command))));
    if (step.optional && step.status === 'next' && step.id === 'vault')
        body?.append(button('I don’t have a key', () => { ctx.setup?.remember('no-key'); ctx.render(); }, { variant: 'ghost' }));
    return card;
}
/** Reads what's set up again, and says so only if it did. */
function checkAgain(ctx: AppContext): void {
    void ctx.setup?.refresh().then(() => {
        if (ctx.delivery.connection.kind === 'ready' && ctx.setup?.current().kind === 'read')
            ctx.toast('Up to date.');
    });
}
function closing(ctx: AppContext, finished: boolean): HTMLElement {
    const close = (): void => { ctx.setup?.remember('closed'); ctx.navigate('chat'); };
    return h('footer', { class: 'setup-actions' }, button(finished ? 'Start chatting' : 'Not now', close, { variant: finished ? 'primary' : 'ghost' }), button('Check again', () => { checkAgain(ctx); }, { icon: 'refresh', variant: 'ghost' }));
}
/** Above the chat from a first visit until the owner closes it, finished or not. */
export function setupNotice(ctx: AppContext): HTMLElement | null {
    const port = ctx.setup;
    const setup = port?.current();
    const model = ctx.model?.current();
    if (port === undefined || setup?.kind !== 'read' || model?.kind !== 'read' || port.remembers('closed'))
        return null;
    const steps = setupSteps(model.model, setup.setup, port.remembers('no-key'), nowSeconds());
    const finished = setupFinished(steps);
    const { title, note } = setupReminder(steps);
    const close = (): void => { port.remember('closed'); ctx.render(); };
    const copy = h('div', { class: 'setup-notice-copy' }, h('strong', {}, title), h('p', {}, note));
    const actions = finished ? [button('Close', close, { variant: 'ghost' })] : [button('Not now', close, { variant: 'ghost' }), button('Continue', () => { ctx.navigate('setup'); }, { variant: 'primary' })];
    return h('section', { class: 'connector-conversation-banner setup-notice', 'aria-label': 'Setup' }, copy, ...actions);
}
export function renderSetup(ctx: AppContext): HTMLElement {
    const page = h('div', { class: 'page setup-page reading-wide', 'data-scroll': 'setup' });
    const heading = (...notes: readonly string[]): HTMLElement => h('header', { class: 'page-header' }, h('div', { class: 'setup-intro' }, h('h1', {}, 'Set up Hatchling'), ...notes.map(note => h('p', { class: 'muted' }, note))));
    const banner = connectionBanner(ctx);
    if (banner !== null)
        page.append(banner);
    const port = ctx.setup;
    const setup = port?.current();
    const model = ctx.model?.current();
    if (port === undefined || setup === undefined || model === undefined) {
        page.append(emptyState('settings', 'Nothing to set up here.', 'This is a sample workspace. A real install shows what’s set up on it.'));
        return page;
    }
    if (setup.kind === 'failed' || model.kind === 'failed') {
        const detail = setup.kind === 'failed' ? setup.detail : model.kind === 'failed' ? model.detail : '';
        page.append(heading(detail), closing(ctx, false));
        return page;
    }
    if (setup.kind === 'unread' || model.kind === 'unread') {
        page.append(heading('Looking at what’s set up…'));
        return page;
    }
    const steps = setupSteps(model.model, setup.setup, port.remembers('no-key'), nowSeconds());
    const finished = setupFinished(steps);
    const windows = setup.setup.windowsSetup;
    const notes = finished ? ['You’re all set. This page stays in Settings, under General.'] : [`${setupProgress(steps)}. ${windows ? INTRO_WINDOWS : INTRO}`, windows ? RESTARTS_WINDOWS : RESTARTS];
    page.append(heading(...notes), h('div', { class: 'setup-steps' }, ...steps.map((step, at) => stepCard(ctx, step, at + 1))), closing(ctx, finished));
    return page;
}
