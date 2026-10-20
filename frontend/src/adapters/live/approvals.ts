// The approval cards, from the broker's confirmations . The UI's
// existing approval surface (TaskApprovalPort) offers Allow once and Deny;
// each becomes the typed `decide` action at the revision the user saw.
//
// A decision is checked again at dispatch, against the confirmation as the
// broker last stated it, not against the card that was drawn: one that is no
// longer pending, or has a newer revision, is never sent. The broker checks
// once more and has the last word. Whether anything is allowed is the
// broker's decision, never this adapter's: it carries the user's answer.
import type { ApprovalAttempt, ApprovalResult, ApprovalStatus, TaskApproval, TaskApprovalPort } from '../../session/task-approvals.js';
import type { LiveClient, Reply } from './client.js';
import type { ActionRefusal, ConfirmationRequest, DecisionRejection, UiEvent } from './protocol/generated.js';
import { MAIN_CHAT, appChat } from './chat.js';
import { clockTime } from './text.js';
const REJECTIONS: Readonly<Record<DecisionRejection, string>> = {
    unknown: 'Hatchling no longer has this request, so nothing was decided.',
    stale_revision: 'This request changed after it was shown. Review the new version before deciding.',
    expired: 'This request expired before the decision arrived, so nothing was decided.',
    already_decided: 'This request was already decided.',
    not_offered: 'This request can only be approved once, so nothing was decided.',
};
/** What a broker rejection says about the request itself. */
const CLOSES: Readonly<Partial<Record<DecisionRejection, ApprovalStatus>>> = { unknown: 'cancelled', expired: 'expired' };
const REFUSALS: Readonly<Record<ActionRefusal, string>> = {
    not_connected: 'Hatchling is not connected to its broker, so nothing was decided.',
    bad_request: 'Hatchling could not read the decision, so nothing was decided.',
    unavailable: 'Hatchling cannot take decisions right now, so nothing was decided.',
    not_granted: 'This connection is not allowed to take decisions, so nothing was decided.',
};
const rejected = (detail: string): ApprovalResult => ({ state: 'rejected', detail });
/**
 * The request's fields as plain text, in the broker's review order: what the
 * broker resolved, then the agent's own words, labeled as the agent's.
 */
export const reviewText = (request: ConfirmationRequest): string => request.fields
    .map((field) => field.source === 'agent' ? `From the agent, ${field.label}: ${field.value}` : `${field.label}: ${field.value}`)
    .join('\n');
const unknown = (detail: string): ApprovalResult => ({ state: 'unknown', detail });
export class LiveApprovals implements TaskApprovalPort {
    readonly #client: LiveClient;
    /** Each confirmation's newest revision, and when it was offered. */
    readonly #seen = new Map<string, {
        readonly request: ConfirmationRequest;
        readonly at: string;
    }>();
    /** Requests the broker said are closed, without a decision. */
    readonly #closed = new Map<string, ApprovalStatus>();
    readonly #attempts = new Map<string, Promise<ApprovalResult>>();
    readonly #sending = new Set<string>();
    constructor(client: LiveClient) {
        this.#client = client;
    }
    /** Record a confirmation event, which the client has already applied once. */
    observe(event: UiEvent): void {
        const body = event.body;
        if (body.kind !== 'confirmation')
            return;
        const seen = this.#seen.get(body.confirmation_id);
        if (seen !== undefined && seen.request.revision >= body.revision)
            return;
        this.#seen.set(body.confirmation_id, { request: body, at: event.ts });
        this.#closed.delete(body.confirmation_id);
    }
    getSnapshot(): readonly TaskApproval[] {
        return [...this.#seen.values()].map(({ request, at }) => this.#card(request, at));
    }
    subscribe(listener: () => void): () => void {
        return this.#client.subscribe(listener);
    }
    /** One answer per attempt id: a repeated attempt gets the first answer, and nothing is sent twice. */
    respond(attempt: ApprovalAttempt): Promise<ApprovalResult> {
        const prior = this.#attempts.get(attempt.id);
        if (prior !== undefined)
            return prior;
        const result = this.#dispatch(attempt);
        this.#attempts.set(attempt.id, result);
        return result;
    }
    /** Reconcile only: ask the broker for what has happened since. Never resends a decision. */
    refresh(): Promise<void> {
        this.#client.resync();
        return Promise.resolve();
    }
    #card(request: ConfirmationRequest, at: string): TaskApproval {
        const decision = this.#client.decision(request.confirmation_id);
        const status: ApprovalStatus = decision === 'approve' ? 'allowed-once' : decision === 'deny' ? 'denied' : this.#closed.get(request.confirmation_id) ?? 'pending';
        const expires = typeof request.expires_at === 'string' ? clockTime(request.expires_at) : '';
        return {
            id: request.confirmation_id,
            revision: request.revision,
            chatId: appChat(request.chat_id ?? MAIN_CHAT),
            threadId: '',
            kind: 'tool',
            title: 'Hatchling asks before it continues',
            description: request.summary,
            resource: expires === '' ? 'This request does not expire.' : `This request expires at ${expires} UTC.`,
            detail: 'Allow once answers this request as described, and only this version of it. Your answer is recorded once.',
            scopes: ['once'],
            status,
            time: clockTime(at),
            report: reviewText(request),
            fields: request.fields,
        };
    }
    async #dispatch(attempt: ApprovalAttempt): Promise<ApprovalResult> {
        if (attempt.decision === 'task')
            return rejected('Allowing a whole task is not offered. Choose Allow once or Deny.');
        if (this.#sending.has(attempt.requestId))
            return unknown('An answer to this request is already on its way. Check its status instead of answering again.');
        const action = this.#client.decide(attempt.requestId, attempt.decision === 'once' ? 'approve' : 'deny');
        if (action?.action !== 'decide' || this.#closed.has(attempt.requestId))
            return rejected('This request is no longer waiting for an answer.');
        if (action.revision !== attempt.revision)
            return rejected(REJECTIONS.stale_revision);
        this.#sending.add(attempt.requestId);
        const reply = await this.#client.request(attempt.id, action);
        this.#sending.delete(attempt.requestId);
        return this.#settle(attempt.requestId, reply);
    }
    #settle(requestId: string, reply: Reply): ApprovalResult {
        if (reply.kind === 'unsent')
            return rejected(reply.detail);
        if (reply.kind === 'unknown')
            return unknown(reply.detail);
        const result = reply.result;
        switch (result.result) {
            case 'decided':
                return { state: 'accepted', detail: 'Hatchling recorded your answer.' };
            case 'decision_rejected': {
                const closes = CLOSES[result.reason];
                if (closes !== undefined)
                    this.#closed.set(requestId, closes);
                return rejected(REJECTIONS[result.reason]);
            }
            case 'refused':
                return rejected(REFUSALS[result.reason]);
            case 'message_accepted':
            case 'schedule':
            case 'skill':
            case 'artifact_opened':
            case 'artifact_shared':
            case 'share_refused':
            case 'artifact_saved':
            case 'file_refused':
            case 'artifact_refused':
            case 'control_accepted':
            case 'control_refused':
            case 'browser_stopped':
                return unknown('Hatchling answered with something other than a decision, so the answer is not confirmed. Check its status.');
        }
    }
}
