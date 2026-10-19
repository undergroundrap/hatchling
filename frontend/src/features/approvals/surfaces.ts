import { domainPendingCount } from './domain.js';
import type { AppContext } from '../../app/context.js';
import type { IconName } from '../../protocol/models.js';
import type { ApprovalDecision, ApprovalStatus, TaskApproval } from '../../session/task-approvals.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, iconButton } from '../../components/ui.js';
import { avatarSource } from '../context/presence.js';
import { reviewFields } from './fields.js';
const labels: Record<ApprovalStatus, string> = { pending: 'Pending approval', 'allowed-once': 'Allowed once', 'allowed-task': 'Allowed for this task', denied: 'Declined', expired: 'Timed out', cancelled: 'Cancelled' };
const selected = new WeakMap<AppContext, Map<string, string>>();
export function approvalLabel(status: ApprovalStatus): string { return labels[status]; }
function glyph(request: TaskApproval): IconName { return request.kind === 'network' ? 'globe' : request.kind === 'tool' ? 'code' : 'link'; }
function openHistory(ctx: AppContext): void {
    if (document.body.classList.contains('has-browser-session'))
        ctx.dock.close();
    ctx.ui.contextTab = 'approvals';
    ctx.ui.contextOpen = true;
    ctx.render();
    document.getElementById('context-tab-approvals')?.focus({ preventScroll: true });
}
function decisionActions(ctx: AppContext, request: TaskApproval): HTMLElement {
    const feedback = ctx.approvals.state(request.id);
    const offline = ctx.delivery.connection.kind !== 'ready';
    const blocked = ctx.state.chats.find(chat => chat.id === request.chatId)?.archived === true;
    const busy = feedback?.phase === 'submitting' || feedback?.phase === 'checking';
    const actions = h('div', { class: 'task-approval-actions' });
    if (feedback?.phase === 'unknown' || feedback?.phase === 'checking') {
        actions.append(button(feedback.phase === 'checking' ? 'Checking status…' : 'Check status', () => { void ctx.approvals.check(request.id); }, { id: `approval-check-${request.id}`, icon: 'refresh', disabled: offline || busy, variant: 'primary' }));
        return actions;
    }
    const decide = (decision: ApprovalDecision): void => {
        if (ctx.delivery.connection.kind !== 'ready' || ctx.state.chats.find(chat => chat.id === request.chatId)?.archived)
            return;
        void ctx.approvals.respond(request, decision).then(() => {
            // Never steal focus from a new draft or another conversation on a late response.
            if (ctx.ui.chatId === request.chatId && document.activeElement === document.body)
                document.getElementById('message-input')?.focus({ preventScroll: true });
        });
    };
    actions.append(button('Deny', () => decide('deny'), { id: `approval-deny-${request.id}`, disabled: offline || blocked || busy }));
    if (request.scopes.includes('task'))
        actions.append(button('Allow for task', () => decide('task'), { id: `approval-task-${request.id}`, disabled: offline || blocked || busy }));
    if (request.scopes.includes('once'))
        actions.append(button('Allow once', () => decide('once'), { id: `approval-once-${request.id}`, disabled: offline || blocked || busy, variant: 'primary' }));
    return actions;
}
/** Composer and history refer to the same adapter request, never duplicated state. */
export function renderTaskApprovalQueue(ctx: AppContext): HTMLElement | null {
    const pending = ctx.approvals.pending(ctx.ui.chatId);
    if (!pending.length)
        return null;
    let selection = selected.get(ctx);
    if (!selection) {
        selection = new Map();
        selected.set(ctx, selection);
    }
    const request = pending.find(item => item.id === selection.get(ctx.ui.chatId)) ?? pending[0];
    if (!request)
        return null;
    const index = pending.indexOf(request);
    const feedback = ctx.approvals.state(request.id);
    const header = h('header', { class: 'task-approval-heading' }, h('img', { src: avatarSource(ctx), alt: '', width: 28, height: 28 }), h('span', {}, icon('shield', 14), 'Task approval needed'), button('Approvals history', () => openHistory(ctx), { variant: 'link', id: 'open-task-approval-history' }));
    const copy = h('div', { class: 'task-approval-copy' }, h('strong', { id: `approval-title-${request.id}` }, request.title), h('p', {}, request.description));
    const card = h('section', { class: 'task-approval-card', 'data-task-approval': request.id, 'aria-labelledby': `approval-title-${request.id}`, 'aria-busy': feedback?.phase === 'submitting' || feedback?.phase === 'checking' }, header, h('div', { class: 'task-approval-request' }, h('span', { class: 'task-approval-icon' }, icon(glyph(request), 20)), copy));
    if (request.fields?.length)
        card.append(reviewFields(request.fields));
    const details = h('details', { class: 'task-approval-details', id: `approval-details-${request.id}` }, h('summary', { id: `approval-scope-${request.id}` }, 'Review scope', icon('chevron', 12)), h('p', { class: 'task-approval-resource' }, request.resource), h('p', {}, request.detail));
    const previousDetails = document.getElementById(details.id);
    details.open = previousDetails instanceof HTMLDetailsElement && previousDetails.open;
    card.append(details);
    if (feedback)
        card.append(h('p', { class: `task-approval-feedback ${feedback.phase}`, role: 'status', 'aria-live': 'polite' }, icon(feedback.phase === 'submitting' || feedback.phase === 'checking' ? 'refresh' : 'warning', 14), feedback.detail));
    if (ctx.delivery.connection.kind !== 'ready')
        card.append(h('p', { class: 'task-approval-feedback', role: 'status' }, icon('pause', 14), 'Reconnect to decide. Permissions are never queued or automatically approved.'));
    else if (ctx.state.chats.find(chat => chat.id === ctx.ui.chatId)?.archived)
        card.append(h('p', { class: 'task-approval-feedback', role: 'status' }, 'Restore this chat to review its request.'));
    card.append(decisionActions(ctx, request));
    if (pending.length > 1) {
        const choose = (offset: number): void => {
            const next = pending[(index + offset + pending.length) % pending.length];
            if (next) {
                selection.set(ctx.ui.chatId, next.id);
                ctx.render();
            }
        };
        const previous = iconButton('chevron', 'Previous approval', () => choose(-1), 'previous-approval');
        previous.id = 'previous-task-approval';
        const next = iconButton('chevron', 'Next approval', () => choose(1));
        next.id = 'next-task-approval';
        card.append(h('footer', { class: 'task-approval-pagination' }, h('span', { role: 'status' }, `${index + 1} of ${pending.length} requests · each needs its own decision`), previous, next));
    }
    return h('div', { class: 'task-approval-dock' }, card);
}
export function taskApprovalRecord(ctx: AppContext, request: TaskApproval): HTMLElement {
    const feedback = ctx.approvals.state(request.id);
    const meta = request.status === 'pending' && feedback ? feedback.phase === 'submitting' ? 'Saving decision…' : feedback.phase === 'checking' ? 'Checking status…' : feedback.phase === 'unknown' ? 'Decision not confirmed' : 'Decision not saved' : labels[request.status];
    const details = h('details', { class: 'approval-record task-approval-record', 'data-approval': request.id, 'data-outcome': request.status, id: `approval-record-${request.id}` });
    details.append(h('summary', {}, h('span', { class: 'approval-icon' }, icon(glyph(request), 17)), h('span', { class: 'approval-copy' }, h('strong', {}, request.title), h('span', { class: 'approval-description' }, request.description), h('span', { class: 'approval-meta' }, `${meta} · ${request.time}`)), icon('chevron', 12)));
    const report = h('div', { class: 'approval-report' }, request.fields?.length ? reviewFields(request.fields) : null, h('p', { class: 'task-approval-resource' }, request.resource), h('p', {}, request.detail), h('p', {}, request.report));
    if (request.status === 'pending')
        report.append(button('Review in chat', () => {
            ctx.navigate('chat', request.chatId);
            if (innerWidth <= 1100) {
                ctx.ui.contextOpen = false;
                ctx.render();
            }
            document.getElementById(`approval-scope-${request.id}`)?.focus({ preventScroll: true });
        }, { icon: 'chat', id: `review-approval-${request.id}` }));
    if (ctx.state.threads.some(thread => thread.id === request.threadId))
        report.append(button('View task activity', () => ctx.openThread(request.threadId), { variant: 'link', icon: 'list' }));
    if (ctx.state.mode === 'scripted')
        report.append(h('span', { class: 'tiny muted' }, 'Local preview · no external action'));
    details.append(report);
    const previousDetails = document.getElementById(details.id);
    details.open = previousDetails instanceof HTMLDetailsElement && previousDetails.open;
    return details;
}
export function approvalActivitySummary(ctx: AppContext): HTMLElement | null {
    const pending = ctx.approvals.pending();
    const latest = pending[0] ?? ctx.approvals.requests[0];
    if (!latest) {
        const count = domainPendingCount(ctx);
        return count ? h('div', { class: 'approval-activity-summary', role: 'status' }, icon('shield', 17), h('div', {}, h('strong', {}, `${count} approval${count === 1 ? '' : 's'} needed`), h('p', {}, 'Review the pending task request before continuing.')), button('Review', () => openHistory(ctx), { variant: 'link', id: 'activity-review-approvals' })) : null;
    }
    return h('div', { class: 'approval-activity-summary', role: 'status' }, icon('shield', 17), h('div', {}, h('strong', {}, pending.length ? `${pending.length} approval${pending.length === 1 ? '' : 's'} needed` : labels[latest.status]), h('p', {}, latest.title)), button('Review', () => openHistory(ctx), { variant: 'link', id: 'activity-review-approvals' }));
}
