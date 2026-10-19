import type { ApprovalAttempt, ApprovalResult, ApprovalScenario, TaskApproval, TaskApprovalPort } from '../session/task-approvals.js';
/** Deterministic, in-memory approval source. No network request or tool is run. */
export class ScriptedTaskApprovals implements TaskApprovalPort {
    private requests: TaskApproval[] = [];
    private readonly listeners = new Set<() => void>();
    private readonly outcomes = new Map<string, {
        attempt: ApprovalAttempt;
        result: ApprovalResult;
    }>();
    private readonly held = new Map<string, ApprovalAttempt>();
    private serial = 0;
    private fault: 'none' | 'failed' | 'unknown' = 'none';
    private disposed = false;
    constructor(private readonly publish: (requests: readonly TaskApproval[]) => void, private readonly chatExists: (chatId: string) => boolean) { }
    getSnapshot(): readonly TaskApproval[] { return structuredClone(this.requests); }
    subscribe(listener: () => void): () => void { this.listeners.add(listener); return () => this.listeners.delete(listener); }
    preview(scenario: ApprovalScenario, chatId: string): void {
        if (this.disposed || !this.chatExists(chatId))
            return;
        this.fault = scenario === 'failed' || scenario === 'unknown' ? scenario : 'none';
        // Old preview requests are cancelled, not granted, when another demo starts.
        this.requests = this.requests.map(item => item.status === 'pending' ? { ...item, status: 'cancelled', report: 'This local preview was replaced. No permission was granted.' } : item);
        this.held.clear();
        const id = `task-approval-${++this.serial}`;
        const base: TaskApproval = { id, revision: 1, chatId, threadId: `approval-work-${this.serial}`, kind: 'network', title: 'Access packages.example.test', description: 'hatchling wants to download software needed for this task.', resource: 'packages.example.test · GET /tools/manifest.json', detail: 'Only the package manifest will be requested. No conversation content, credentials, or files will be shared. Allow once covers one request; Allow for task is limited to this task and this host.', scopes: ['once', 'task'], status: scenario === 'expired' ? 'expired' : 'pending', time: 'just now', report: scenario === 'expired' ? 'The request expired before a decision was received. No permission was granted. A new request is needed to continue.' : 'Waiting for your decision. The download has not started.' };
        this.requests.unshift(base);
        if (scenario === 'multiple')
            this.requests.splice(1, 0, { ...base, id: `task-approval-${++this.serial}`, kind: 'tool', title: 'Run the project setup check', description: 'hatchling wants to inspect the sample project before installing tools.', resource: 'sample-project · inspect --read-only', detail: 'Read-only inspection of this sample project. No file changes, purchases, or outgoing messages. This approval is for one check only.', scopes: ['once'], fields: [{ label: 'Operation', value: 'Run a read-only check', source: 'broker' }, { label: 'Project', value: 'sample-project', source: 'broker' }, { label: 'Why', value: 'Check the setup notes in setup\u202Etxt.exe before installing tools.', source: 'agent' }] });
        this.requests = this.requests.slice(0, 50);
        this.emit();
    }
    respond(attempt: ApprovalAttempt): Promise<ApprovalResult> {
        const previous = this.outcomes.get(attempt.id);
        if (previous) {
            const same = previous.attempt.requestId === attempt.requestId && previous.attempt.revision === attempt.revision && previous.attempt.decision === attempt.decision;
            return Promise.resolve(same ? previous.result : { state: 'rejected', detail: 'This attempt ID already belongs to a different decision.' });
        }
        const request = this.requests.find(item => item.id === attempt.requestId);
        let result: ApprovalResult;
        if (this.disposed || !request || request.status !== 'pending' || request.revision !== attempt.revision || !this.chatExists(request.chatId))
            result = { state: 'rejected', detail: 'This request changed or is no longer pending. Review the current request.' };
        else if (attempt.decision !== 'deny' && !request.scopes.includes(attempt.decision))
            result = { state: 'rejected', detail: 'That permission scope was not requested.' };
        else if (this.held.has(request.id))
            result = { state: 'unknown', detail: 'A decision for this request is awaiting confirmation. Check its status; no additional decision was sent.' };
        else if (this.fault === 'failed') {
            this.fault = 'none';
            result = { state: 'rejected', detail: 'Your decision was not saved. Nothing was approved. Try again when ready.' };
        }
        else if (this.fault === 'unknown') {
            this.fault = 'none';
            this.held.set(request.id, { ...attempt });
            result = { state: 'unknown', detail: 'Decision not confirmed. It may have been received. Check status without resending.' };
        }
        else {
            this.settle(attempt);
            result = { state: 'accepted', detail: 'Decision saved in the local preview.' };
        }
        this.outcomes.set(attempt.id, { attempt: { ...attempt }, result });
        return Promise.resolve(result);
    }
    refresh(id: string): Promise<void> {
        const held = this.held.get(id);
        if (held) {
            this.held.delete(id);
            this.settle(held);
        }
        return Promise.resolve();
    }
    private settle(attempt: ApprovalAttempt): void {
        this.requests = this.requests.map(request => {
            if (request.id !== attempt.requestId || request.status !== 'pending' || request.revision !== attempt.revision || !this.chatExists(request.chatId))
                return request;
            return { ...request, status: attempt.decision === 'deny' ? 'denied' : attempt.decision === 'task' ? 'allowed-task' : 'allowed-once', time: 'just now', report: attempt.decision === 'deny' ? 'You declined this request. The requested action did not run.' : `${attempt.decision === 'task' ? 'Allowed only for this task and resource' : 'Allowed for one request only'}. This is a local preview; no software was downloaded and no external tool ran.` };
        });
        this.emit();
    }
    reset(): void {
        this.requests = [];
        this.held.clear();
        this.outcomes.clear();
        this.fault = 'none';
        this.emit();
    }
    private emit(): void {
        if (this.disposed)
            return;
        this.publish(this.getSnapshot());
        for (const listener of this.listeners)
            listener();
    }
    dispose(): void { this.disposed = true; this.listeners.clear(); this.held.clear(); }
}
