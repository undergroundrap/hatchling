/** Optional frontend capability, separate from the frozen product protocol.
 * A live implementation must return authoritative requests, validate the exact
 * revision and scope, deduplicate attempts, and persist decisions server-side.
 * Neither an optimistic click nor a lost response is an approval. */
export type ApprovalDecision = 'once' | 'task' | 'day' | 'always' | 'deny';
export type ApprovalStatus = 'pending' | 'allowed-once' | 'allowed-task' | 'allowed-day' | 'allowed-always' | 'denied' | 'expired' | 'cancelled';
/** One field of what a request asks, and who stated it: the broker resolved it, or the agent wrote it. */
export interface ReviewField {
    readonly label: string;
    readonly value: string;
    readonly source: 'broker' | 'agent';
}
export interface TaskApproval {
    readonly id: string;
    readonly revision: number;
    readonly chatId: string;
    readonly threadId: string;
    readonly kind: 'network' | 'tool' | 'connector';
    readonly title: string;
    readonly description: string;
    readonly resource: string;
    readonly detail: string;
    readonly scopes: readonly ('once' | 'task' | 'day' | 'always')[];
    /** The grant a standing yes would make, as its source names it: `web.visit:https://a.example`. */
    readonly grantKey?: string;
    /** What approving it does, when its source says: the one approve button's words, for Allow once. */
    readonly approveLabel?: string;
    readonly status: ApprovalStatus;
    readonly time: string;
    readonly report: string;
    /** The request's fields, when its source states them: shown whole, in review order. */
    readonly fields?: readonly ReviewField[];
    /** Each thing a batch asks for, in order, with its own fields (WO-055): the memories on one card. */
    readonly items?: readonly (readonly ReviewField[])[];
}
export interface ApprovalAttempt {
    readonly id: string;
    readonly requestId: string;
    readonly revision: number;
    readonly decision: ApprovalDecision;
    /** The items kept of a batch, by place on this revision (WO-055); none named keeps them all. */
    readonly keep?: readonly number[];
}
export interface ApprovalResult {
    readonly state: 'accepted' | 'rejected' | 'unknown';
    readonly detail: string;
}
export type ApprovalScenario = 'network' | 'multiple' | 'batch' | 'expired' | 'failed' | 'unknown' | 'lapsed';
export interface TaskApprovalPort {
    getSnapshot(): readonly TaskApproval[];
    subscribe(listener: () => void): () => void;
    respond(attempt: ApprovalAttempt): Promise<ApprovalResult>;
    /** Reconcile only. Must never replay a decision or grant permission. */
    refresh(requestId: string): Promise<void>;
    /** Scripted local demonstrations only; absent on a live implementation. */
    preview?(scenario: ApprovalScenario, chatId: string): void;
}
