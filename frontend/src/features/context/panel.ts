import { domainPendingCount } from '../approvals/domain.js';
import { approvalActivitySummary } from '../approvals/surfaces.js';
import { profileMounted } from './profile-motion.js';
import { avatarMenu } from './avatar.js';
import { setContextOpen, avatarSource } from './presence.js';
import { agentStatus } from './agent-status.js';
import type { AppContext, ContextTab } from '../../app/context.js';
import type { IconName } from '../../protocol/models.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { iconButton, menu } from '../../components/ui.js';
import { renderIdentity } from './identity.js';
import { renderApprovals } from './approvals.js';
import { renderContextSchedules } from './schedules.js';
const tabs: readonly {
    id: ContextTab;
    label: string;
    icon: IconName;
}[] = [
    { id: 'activity', label: 'Activity', icon: 'list' },
    { id: 'approvals', label: 'Approvals', icon: 'shield' },
    { id: 'schedule', label: 'Schedule', icon: 'clock' },
    { id: 'identity', label: 'Identity', icon: 'fingerprint' },
];
const selectors = new WeakMap<AppContext, {
    element: HTMLElement;
    select(tab: ContextTab): void;
}>();
export function selectContextTab(ctx: AppContext, tab: ContextTab): void {
    const mounted = selectors.get(ctx);
    if (mounted?.element.isConnected)
        mounted.select(tab);
    else {
        ctx.ui.contextTab = tab;
        ctx.render();
    }
}
function renderHistory(ctx: AppContext): HTMLElement {
    const root = h('div', { class: 'activity-history' }, h('div', { class: 'context-section-heading' }, h('h3', { id: 'today-heading' }, 'Today'), ctx.state.mode === 'scripted' ? menu('Activity options', [{ label: 'Preview active work', icon: 'play', action: () => { void ctx.run({ type: 'demo.scenario', scenario: 'bundle-files' }, true).then(() => ctx.openThread('live-bundle')); } }]) : null));
    const approvals = approvalActivitySummary(ctx);
    if (approvals)
        root.append(approvals);
    for (const thread of ctx.state.threads) {
        const glyph: IconName = thread.state === 'running' ? 'run' : thread.state === 'interrupted' ? 'pause' : thread.state === 'failed' ? 'close' : thread.evidenceArtifactId ? 'file' : 'check';
        const row = h('button', { class: 'activity-entry', type: 'button', 'data-state': thread.state, 'aria-label': `Open evidence: ${thread.name}`, 'aria-description': `${thread.state}. ${thread.subtitle}. ${thread.time}` }, h('span', { class: `activity-icon ${thread.state === 'running' ? 'is-running' : ''}` }, icon(glyph, 18)), h('span', { class: 'activity-copy' }, h('strong', {}, thread.name), h('span', {}, thread.subtitle), h('time', {}, thread.time)));
        row.addEventListener('click', () => ctx.openThread(thread.id));
        root.append(row);
    }
    return root;
}
/** Context tab selection is view state. It never navigates, grants permission, or starts a job. */
export function renderContextPanel(ctx: AppContext): HTMLElement {
    const status = agentStatus(ctx.state, ctx.delivery.connection, ctx.approvals.pending());
    const close = iconButton('close', 'Hide agent status', () => setContextOpen(ctx, false));
    close.removeAttribute('data-tooltip');
    const aside = h('aside', { class: 'context-panel', 'aria-label': 'Agent status and activity' }, h('div', { class: 'context-close' }, close), h('header', { class: 'agent-profile' }, h('div', { class: 'agent-portrait' }, h('img', { src: avatarSource(ctx), alt: `${ctx.state.settings.name} avatar`, width: 94, height: 94 }), ctx.state.mode === 'live' ? null : avatarMenu(ctx)), h('h2', { class: 'agent-name', title: ctx.state.settings.name }, ctx.state.settings.name), h('p', { class: `agent-status ${status.kind}`, role: 'status', title: status.detail, 'aria-live': 'polite' }, icon(status.icon, 16), h('span', { class: 'agent-status-label' }, status.label))));
    const profile = aside.querySelector<HTMLElement>('.agent-profile');
    if (profile)
        profile.replaceWith(profileMounted(ctx, profile, status.label, status.kind));
    const tablist = h('div', { class: 'context-tabs', role: 'tablist', 'aria-label': 'Agent context' });
    const indicator = h('span', { class: 'context-tab-indicator', 'aria-hidden': 'true' });
    tablist.append(indicator);
    const body = h('div', { class: 'context-body', id: 'context-tabpanel', role: 'tabpanel', tabindex: 0 });
    const drawBody = (): void => {
        body.id = 'context-tabpanel';
        body.setAttribute('aria-labelledby', `context-tab-${ctx.ui.contextTab}`);
        body.dataset['scroll'] = `context-${ctx.ui.contextTab}`;
        const content = ctx.ui.contextTab === 'activity' ? renderHistory(ctx) : ctx.ui.contextTab === 'approvals' ? renderApprovals(ctx) : ctx.ui.contextTab === 'schedule' ? renderContextSchedules(ctx) : renderIdentity(ctx);
        body.replaceChildren(content);
        body.scrollTop = ctx.ui.contextScroll.get(ctx.ui.contextTab) ?? 0;
    };
    const select = (id: ContextTab): void => {
        if (ctx.ui.contextTab === id)
            return;
        ctx.ui.contextScroll.set(ctx.ui.contextTab, body.scrollTop);
        ctx.ui.contextTab = id;
        indicator.style.transform = `translateX(${tabs.findIndex((tab) => tab.id === id) * 100}%)`;
        for (const node of tablist.querySelectorAll<HTMLButtonElement>('[role="tab"]')) {
            const selected = node.id === `context-tab-${id}`;
            node.classList.toggle('active', selected);
            node.setAttribute('aria-selected', String(selected));
            node.tabIndex = selected ? 0 : -1;
        }
        // Only the content changes. The pill and all four buttons keep their DOM identity.
        for (const animation of body.getAnimations())
            animation.cancel();
        drawBody();
        if (!matchMedia('(prefers-reduced-motion: reduce)').matches)
            body.animate([{ opacity: .4, transform: 'translateY(3px)' }, { opacity: 1, transform: 'translateY(0)' }], { duration: 180, easing: 'ease-out' });
    };
    for (const [index, tab] of tabs.entries()) {
        const node = iconButton(tab.icon, tab.label, () => select(tab.id), ctx.ui.contextTab === tab.id ? 'active' : '');
        node.id = `context-tab-${tab.id}`;
        if (tab.id === 'approvals' && (ctx.approvals.pending().length + domainPendingCount(ctx))) {
            node.append(h('span', { class: 'approval-tab-dot', 'aria-hidden': 'true' }));
            node.setAttribute('aria-description', `${(ctx.approvals.pending().length + domainPendingCount(ctx))} pending approvals`);
        }
        node.setAttribute('role', 'tab');
        node.setAttribute('aria-selected', String(ctx.ui.contextTab === tab.id));
        node.setAttribute('aria-controls', 'context-tabpanel');
        node.tabIndex = ctx.ui.contextTab === tab.id ? 0 : -1;
        node.addEventListener('keydown', (event) => {
            const next = event.key === 'ArrowRight' ? (index + 1) % tabs.length : event.key === 'ArrowLeft' ? (index - 1 + tabs.length) % tabs.length : event.key === 'Home' ? 0 : event.key === 'End' ? tabs.length - 1 : -1;
            const target = tabs[next];
            if (!target)
                return;
            event.preventDefault();
            select(target.id);
            document.getElementById(`context-tab-${target.id}`)?.focus({ preventScroll: true });
        });
        tablist.append(node);
    }
    indicator.style.transform = `translateX(${tabs.findIndex((tab) => tab.id === ctx.ui.contextTab) * 100}%)`;
    drawBody();
    body.addEventListener('scroll', () => ctx.ui.contextScroll.set(ctx.ui.contextTab, body.scrollTop), { passive: true });
    selectors.set(ctx, { element: aside, select });
    aside.append(tablist, body);
    return aside;
}
