import type { TaskApproval } from '../../session/task-approvals.js';
import type { ConnectionState } from '../../session/chat-session.js';
import type { AppState, DeepReadonly, IconName } from '../../protocol/models.js';
export interface AgentStatus {
    label: string;
    detail: string;
    kind: 'connected' | 'working' | 'waiting' | 'paused';
    icon: IconName;
    threadId: string | null;
}
/** Status is derived from active adapter work, never a cosmetic clock or random rotating phrase. */
export function agentStatus(state: DeepReadonly<AppState>, connection?: ConnectionState, approvals: readonly TaskApproval[] = []): AgentStatus {
    if (connection && connection.kind !== 'ready') {
        const labels = { offline: 'Offline', reconnecting: 'Reconnecting', restarting: 'Workspace restarting', unavailable: 'Service unavailable', 'sign-in-required': 'Session needs attention', locked: 'Locked' };
        return { label: labels[connection.kind], detail: connection.detail, kind: 'waiting', icon: ['reconnecting', 'restarting'].includes(connection.kind) ? 'refresh' : 'warning', threadId: null };
    }
    const approval = approvals.find(request => request.status === 'pending');
    if (approval)
        return { label: 'Waiting for approval', detail: approval.description, kind: 'waiting', icon: 'shield', threadId: approval.threadId };
    const active = state.threads.find((thread) => thread.state === 'running');
    if (state.turn === 'interrupted')
        return { label: 'Work is paused', detail: 'Resume the saved activity when ready.', kind: 'paused', icon: 'pause', threadId: state.threads.find((thread) => thread.state === 'interrupted')?.id ?? null };
    if (state.turn === 'stalled')
        return { label: 'Waiting on a source', detail: 'Work has stopped until the required source is available.', kind: 'waiting', icon: 'clock', threadId: null };
    if (state.turn === 'awaiting')
        return { label: 'Waiting for confirmation', detail: 'No sensitive action continues before approval.', kind: 'waiting', icon: 'shield', threadId: active?.id ?? null };
    if (active)
        return { label: active.statusTitle, detail: active.subtitle, kind: 'working', icon: 'run', threadId: active.id };
    const browser = state.browserTasks.find((task) => task.state === 'running' || task.state === 'parked');
    if (browser)
        return { label: browser.controller === 'user' ? 'Waiting for you' : browser.state === 'parked' ? 'Waiting for confirmation' : browser.statusTitle, detail: browser.step, kind: browser.controller === 'user' || browser.state === 'parked' ? 'waiting' : 'working', icon: 'browser', threadId: null };
    if (state.turn === 'working')
        return { label: 'Working on it', detail: 'Waiting for the first concrete activity update.', kind: 'working', icon: 'run', threadId: null };
    return { label: 'Connected', detail: state.mode === 'scripted' ? 'Connected to the local scripted adapter. No live agent.' : 'Adapter ready. No task is running; network health is not exposed by this contract.', kind: 'connected', icon: 'connected', threadId: null };
}
