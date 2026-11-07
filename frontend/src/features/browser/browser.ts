import type { AppContext } from '../../app/context.js';
import type { BrowserTask, DeepReadonly } from '../../protocol/models.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, emptyState, pill } from '../../components/ui.js';
import { browserFrame, controlPill, switchingControl } from './session.js';
import { purchaseCard } from '../chat/purchase-card.js';
export function renderBrowser(ctx: AppContext): HTMLElement {
    const tasks = ctx.state.browserTasks;
    const task = tasks.find((item) => ['parked', 'running'].includes(item.state)) ?? tasks.find((item) => item.state === 'assigned') ?? tasks.at(-1);
    const page = h('div', { class: 'page browser-page', 'data-scroll': 'browser' }, h('header', { class: 'page-header' }, h('div', {}, h('h1', {}, 'Browser'), h('p', { class: 'muted text-small' }, 'A window into the work. Control stays with you.')), ctx.state.mode === 'scripted' ? button('Demo scenarios', ctx.openScenarios, { icon: 'play' }) : null));
    if (!task) {
        page.append(emptyState('browser', 'No task is holding the browser.'));
        return page;
    }
    page.append(h('div', { class: 'browser-status' }, h('div', { class: 'row-copy' }, h('h2', {}, task.title), h('p', { class: 'muted' }, task.step)), pill(task.state, task.state === 'parked' ? 'attention' : ''), pill(controlPill(task.controller).label, controlPill(task.controller).tone)));
    const controls = h('div', { class: 'browser-controls' });
    if (!(ctx.state.mode === 'live') && (task.state === 'assigned' || task.state === 'queued'))
        controls.append(button('Start sample task', () => { void ctx.run({ type: 'browser.start', id: task.id }); }, { icon: 'play', variant: 'primary' }));
    if (!['cancelled', 'completed'].includes(task.state))
        controls.append(task.controller === 'switching' ? switchingControl() : task.controller === 'agent' ? button('Take over', () => { void ctx.run({ type: 'browser.takeover', id: task.id }); }, { icon: 'device', variant: 'primary' }) : button('Hand control back', () => { void ctx.run({ type: 'browser.resume', id: task.id }); }, { icon: 'run' }), button('Stop task', () => ctx.dialogs.confirm('Stop this browser task?', 'No more actions will run. A submitted or unknown payment is not cancelled by stopping this task.', () => { void ctx.run({ type: 'browser.cancel', id: task.id }); }, { label: 'Stop task', danger: true }), { icon: 'stop', variant: 'danger' }));
    page.append(controls, h('div', { class: 'browser-window' }, h('div', { class: 'browser-chrome' }, h('span', { class: 'window-dots', 'aria-hidden': 'true' }, '● ● ●'), h('span', { class: 'address-bar' }, icon('lock', 13), `${task.site}${ctx.state.mode === 'scripted' ? ' · local scripted view' : ''}`), icon('more', 18)), simulatedViewport(ctx, task)), h('div', { class: 'browser-evidence' }, h('div', {}, h('h3', {}, 'Last confirmed page state'), h('p', {}, task.lastConfirmedState)), h('div', {}, h('h3', {}, 'Task boundaries'), h('ul', {}, ...task.constraints.map((constraint) => h('li', {}, constraint))), ctx.state.mode === 'live' ? null : h('p', {}, `${task.attemptsRemaining} attempts remaining`), task.credentialReference ? h('p', { class: 'tiny muted' }, `Protected reference: ${task.credentialReference}`) : null), h('div', {}, h('h3', {}, 'Active grants'), task.granted.length ? h('ul', {}, ...task.granted.map((grant) => h('li', {}, grant))) : h('p', {}, 'No challenge or payment permission granted.'), task.continuedFrom ? pill('Continued after takeover') : null)));
    page.append(h('h2', { class: 'section-heading' }, 'Browser slot & queue'), h('div', { class: 'task-queue' }, ...tasks.map((item) => h('div', { class: 'queue-row' }, icon(item.id === task.id ? 'browser' : 'clock', 18), h('div', { class: 'row-copy' }, h('strong', {}, item.title), h('p', { class: 'tiny muted' }, item.site)), pill(item.state), !(ctx.state.mode === 'live') && (item.state === 'queued' || item.state === 'assigned') ? button('Admit task', () => { void ctx.run({ type: 'browser.start', id: item.id }); }, { variant: 'ghost' }) : null))));
    page.append(h('p', { class: 'sample-footnote' }, ctx.state.mode === 'scripted' ? 'This viewport renders deterministic sample page state. No Chromium session, real merchant, stream, cookies, credential fill, or live order is connected.' : 'What you see here comes from Hatchling’s own browser, which runs apart from your machine.'));
    return page;
}
function simulatedViewport(ctx: AppContext, task: DeepReadonly<BrowserTask>): HTMLElement {
    if (ctx.state.mode !== 'scripted')
        return browserFrame(task, false, false);
    const challenge = task.challenge === 'waiting' || task.challenge === 'persistent';
    const viewport = h('div', { class: 'simulated-viewport' }, h('div', { class: 'sample-store-header' }, h('strong', {}, task.site === 'SampleSupply' ? 'SAMPLE / SUPPLY' : task.site.toUpperCase()), h('span', {}, 'Storefront preview')), h('div', { class: 'sample-store-heading' }, h('span', { class: 'eyebrow' }, 'FEWER, BETTER THINGS'), h('h2', {}, 'For the everyday,\nand the days away.')));
    if (challenge) {
        const persistent = task.challenge === 'persistent';
        const gate = h('div', { class: 'challenge-gate' }, icon('shield', 35), h('h3', {}, persistent ? 'This step needs your help' : 'A site challenge needs permission'), h('p', {}, `${task.site} · ${task.step}`), h('p', { class: 'muted' }, task.blocker), h('p', { class: 'text-small' }, persistent ? 'The allowed attempts are exhausted. No automatic retry will run. Take over, finish the visible challenge, then return control.' : 'Permission applies only to this challenge. You can also complete it yourself without granting the agent permission.'));
        if (task.controller === 'user')
            gate.append(button('Complete sample challenge yourself', () => { void ctx.run({ type: 'browser.solve', id: task.id }); }, { variant: 'primary', icon: 'check' }));
        else
            gate.append(h('div', { class: 'action-row' }, !persistent ? button('Allow this challenge', () => { void ctx.run({ type: 'browser.grant', id: task.id }); }, { variant: 'primary', icon: 'shield' }) : null, button('Solve it myself', () => { void ctx.run({ type: 'browser.takeover', id: task.id }); }, { icon: 'device' })));
        viewport.append(gate);
    }
    else {
        const purchase = ctx.state.purchases.find((item) => item.taskId === task.id);
        if (purchase)
            viewport.append(h('div', { class: 'browser-purchase' }, purchaseCard(ctx, purchase)));
        else
            viewport.append(h('div', { class: 'sample-product' }, h('div', { class: 'bag-illustration', 'aria-label': 'Abstract illustration of a canvas bag' }, h('span', { class: 'bag-handle' }), h('span', { class: 'bag-body' }, 'S / S')), h('div', {}, h('span', { class: 'eyebrow' }, 'THE WEEKEND TOTE'), h('h3', {}, 'A good companion.'), h('p', {}, 'Natural canvas. One thoughtfully sized carry-all.'), h('p', { class: 'product-price' }, '$68.00'), button('Open sample checkout review', () => { void ctx.run({ type: 'demo.scenario', scenario: 'purchase' }); }, { variant: 'primary', icon: 'cart' }))));
        if (task.controller === 'user')
            viewport.append(h('div', { class: 'takeover-hint' }, icon('device', 18), 'You control this sample session. Check the visible state before handing it back.'));
    }
    return viewport;
}
