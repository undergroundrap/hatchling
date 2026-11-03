import { domainApprovalRecords } from '../approvals/domain.js';
import { taskApprovalRecord } from '../approvals/surfaces.js';
import type { AppContext } from '../../app/context.js';
import type { ApprovalRecord } from '../../protocol/models.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, iconButton } from '../../components/ui.js';
const outcomeLabels: Record<ApprovalRecord['outcome'], string> = { 'timed-out': 'Timed out', task: 'Allowed for this task', always: 'Site always allowed', denied: 'Declined' };
export function renderApprovals(ctx: AppContext): HTMLElement {
    const root = h('div', { class: 'approvals-panel' }, h('div', { class: 'context-section-heading' }, h('h3', {}, 'Approvals history'), iconButton('settings', 'Manage permissions', () => ctx.openSettings('permissions'), 'context-heading-action')));
    for (const request of ctx.approvals.requests.toSorted((a, b) => Number(b.status === 'pending') - Number(a.status === 'pending')))
        root.append(taskApprovalRecord(ctx, request));
    root.append(...domainApprovalRecords(ctx));
    for (const record of ctx.state.approvalHistory) {
        const details = h('details', { class: 'approval-record', 'data-approval': record.id });
        details.append(h('summary', {}, h('span', { class: 'approval-icon' }, icon('globe', 17), icon('lock', 9)), h('span', { class: 'approval-copy' }, h('strong', {}, record.title), h('span', { class: 'approval-description' }, record.description), h('span', { class: 'approval-meta' }, `${outcomeLabels[record.outcome]} · ${record.time}`)), icon('chevron', 12)), h('div', { class: 'approval-report' }, h('p', {}, record.report), ctx.state.mode === 'scripted' ? h('span', { class: 'tiny muted' }, 'Synthetic sample history · no external action') : null));
        root.append(details);
    }
    // Nothing asked yet: say what will be here, rather than show a blank.
    if (root.childElementCount === 1)
        root.append(h('p', { class: 'muted text-small' }, 'Nothing to decide yet. When the agent needs your yes before it acts, the request appears here, and so does what you decided.'));
    root.append(h('div', { class: 'context-trust-links' }, button('Permissions & active grants', () => ctx.openSettings('permissions'), { icon: 'shield', variant: 'ghost' }), button('Connected services', () => ctx.openSettings('connectors'), { icon: 'link', variant: 'ghost' })));
    return root;
}
