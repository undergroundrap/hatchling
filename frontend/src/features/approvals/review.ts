import type { ApprovalAttempt, ApprovalDecision, ApprovalResult, TaskApproval, TaskApprovalPort } from '../../session/task-approvals.js';
export interface ApprovalFeedback {
    readonly phase: 'submitting' | 'checking' | 'unknown' | 'error';
    readonly detail: string;
    readonly revision: number;
}
/** An answer that reached a request too late: nothing was decided, and the person is told so. */
export interface ApprovalNotice {
    readonly id: string;
    readonly chatId: string;
    readonly title: string;
    readonly detail: string;
}
/** UI-only in-flight state. Authoritative permission decisions stay in the adapter. */
export class ApprovalReview {
    private readonly feedback = new Map<string, ApprovalFeedback>();
    /** By request: kept until dismissed, since the request itself has left the queue. */
    private readonly unanswered = new Map<string, ApprovalNotice>();
    private readonly attempts = new Map<string, ApprovalAttempt>();
    private readonly unsubscribe: (() => void) | undefined;
    private readonly timers = new Set<ReturnType<typeof setTimeout>>();
    private readonly prefix = `approval-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    private serial = 0;
    private disposed = false;
    constructor(readonly port: TaskApprovalPort | undefined, private readonly changed: () => void, private readonly timeoutMs = 12000) {
        this.unsubscribe = port?.subscribe(() => { this.sync(); this.changed(); });
    }
    get requests(): readonly TaskApproval[] { return this.port?.getSnapshot() ?? []; }
    state(id: string): ApprovalFeedback | undefined { return this.feedback.get(id); }
    pending(chatId?: string): readonly TaskApproval[] { return this.requests.filter(item => item.status === 'pending' && (!chatId || item.chatId === chatId)); }
    /** The answers that reached a closed request, oldest first: each says nothing was decided. */
    notices(chatId?: string): readonly ApprovalNotice[] { return [...this.unanswered.values()].filter(item => !chatId || item.chatId === chatId); }
    dismiss(id: string): void {
        if (this.unanswered.delete(id))
            this.changed();
    }
    private sync(): void {
        for (const [id, feedback] of this.feedback) {
            const request = this.requests.find(item => item.id === id);
            if (!request || request.status !== 'pending' || request.revision !== feedback.revision) {
                this.feedback.delete(id);
                this.attempts.delete(id);
            }
        }
    }
    async respond(request: TaskApproval, decision: ApprovalDecision): Promise<void> {
        const current = this.requests.find(item => item.id === request.id);
        const feedback = this.feedback.get(request.id);
        if (!this.port || !current || current.status !== 'pending' || current.revision !== request.revision || (feedback && feedback.phase !== 'error'))
            return;
        if (decision !== 'deny' && !current.scopes.includes(decision))
            return;
        // An explicitly rejected attempt is safe to try again. Unknown attempts are not.
        const attempt: ApprovalAttempt = { id: `${this.prefix}-${++this.serial}`, requestId: request.id, revision: request.revision, decision };
        this.attempts.set(request.id, attempt);
        this.feedback.set(request.id, { phase: 'submitting', detail: 'Saving your decision…', revision: request.revision });
        this.changed();
        const result = await this.watch(Promise.resolve().then(() => this.port?.respond(attempt) ?? { state: 'rejected', detail: 'Approval service unavailable.' }));
        // Another attempt took this one's place: its answer is not this
        // request's any more. An attempt that is simply gone is different:
        // the request stopped waiting while the answer was on its way, and
        // what came of the answer is still owed to the person.
        const latest = this.attempts.get(request.id);
        if (this.disposed || (latest !== undefined && latest.id !== attempt.id))
            return;
        this.sync();
        if (this.requests.some(item => item.id === request.id && item.status === 'pending' && item.revision === request.revision)) {
            this.feedback.set(request.id, { phase: result.state === 'rejected' ? 'error' : 'unknown', revision: request.revision, detail: result.state === 'accepted' ? 'Decision received; waiting for its confirmed status. Check status before continuing.' : result.detail });
        }
        else if (result.state === 'rejected') {
            // The request closed under the answer, so it has left the queue as
            // a decided one does. The person is told nothing was decided,
            // never left to think their answer took.
            this.unanswered.set(request.id, { id: request.id, chatId: request.chatId, title: request.description, detail: result.detail });
        }
        this.changed();
    }
    async check(id: string): Promise<void> {
        const request = this.requests.find(item => item.id === id);
        const current = this.feedback.get(id);
        if (!this.port || !request || request.status !== 'pending' || current?.phase === 'checking' || current?.phase === 'submitting')
            return;
        this.feedback.set(id, { phase: 'checking', detail: 'Checking the decision status…', revision: request.revision });
        this.changed();
        const result = await this.watch(Promise.resolve().then(() => this.port?.refresh(id)).then((): ApprovalResult => ({ state: 'accepted', detail: '' })));
        if (this.disposed)
            return;
        this.sync();
        if (this.requests.some(item => item.id === id && item.status === 'pending' && item.revision === request.revision))
            this.feedback.set(id, { phase: 'unknown', revision: request.revision, detail: result.state === 'unknown' ? result.detail : 'The decision is still not confirmed. No decision was resent. Check again shortly.' });
        this.changed();
    }
    private watch(operation: Promise<ApprovalResult>): Promise<ApprovalResult> {
        const unknown: ApprovalResult = { state: 'unknown', detail: 'Decision not confirmed. It may have been received. Check status; do not approve again.' };
        return new Promise(resolve => {
            const timer = setTimeout(() => { this.timers.delete(timer); resolve(unknown); }, this.timeoutMs);
            this.timers.add(timer);
            void operation.then(result => { clearTimeout(timer); this.timers.delete(timer); resolve(result); }, () => { clearTimeout(timer); this.timers.delete(timer); resolve(unknown); });
        });
    }
    dispose(): void {
        this.disposed = true;
        this.unsubscribe?.();
        for (const timer of this.timers)
            clearTimeout(timer);
        this.timers.clear();
        this.unanswered.clear();
    }
}
