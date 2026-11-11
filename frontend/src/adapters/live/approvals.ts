// The approval cards, from the broker's confirmations . The UI's
// existing approval surface (TaskApprovalPort) offers Allow once and Deny,
// and Allow for task and Allow always where the request offers them (a
// site the agent asks to read, WO-028): the first lasts for the agent's
// session, which is its task, the second until the owner revokes it. Each becomes the typed `decide` action at the revision the user saw.
//
// A decision is checked again at dispatch, against the confirmation as the
// broker last stated it, not against the card that was drawn: one that is no
// longer pending, or has a newer revision, is never sent. The broker checks
// once more and has the last word. Whether anything is allowed is the
// broker's decision, never this adapter's: it carries the user's answer.
//
// A request the broker closes without a decision (it lapsed, or the session
// that asked ended) is closed here when the broker says so, on the stream: its
// card stops offering answers, and a replay never shows it as waiting again.
//
// The decisions the owner made before this connection, which the broker
// keeps, are read when it opens: the history shows them as they were decided,
// across a restart.
import type { ApprovalAttempt, ApprovalResult, ApprovalStatus, TaskApproval, TaskApprovalPort } from '../../session/task-approvals.js';
import type { LiveClient, Reply } from './client.js';
import type { ActionRefusal, ConfirmationClosure, ConfirmationRequest, Decision, DecisionRecord, DecisionRejection, UiEvent } from './protocol/generated.js';
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
/** What the broker closing a request without a decision makes of its card. */
const CLOSURES: Readonly<Record<ConfirmationClosure, ApprovalStatus>> = { expired: 'expired', withdrawn: 'cancelled' };
const DAY_MS = 24 * 60 * 60 * 1000;
/** The local calendar day a moment falls on, as a day count. */
const localDay = (at: number): number => {
    const day = new Date(at);
    return Math.round(new Date(day.getFullYear(), day.getMonth(), day.getDate()).getTime() / DAY_MS);
};
/**
 * When a request stops waiting, in the words a person would use on the day it
 * was offered: `09:10`, `tomorrow at 09:10`, or the date and the time. On the
 * person's own clock, as every other time in the app is.
 */
export const expiryWords = (expiresAt: string, offeredAt: string): string => {
    const time = clockTime(expiresAt);
    const days = localDay(Date.parse(expiresAt)) - localDay(Date.parse(offeredAt));
    if (!(days > 0))
        return time;
    if (days === 1)
        return `tomorrow at ${time}`;
    const day = new Date(Date.parse(expiresAt));
    return `${day.getFullYear()}-${String(day.getMonth() + 1).padStart(2, '0')}-${String(day.getDate()).padStart(2, '0')} at ${time}`;
};
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
    /** Requests the broker closed on the stream: for good, whatever is replayed after. */
    readonly #ended = new Map<string, ApprovalStatus>();
    readonly #attempts = new Map<string, Promise<ApprovalResult>>();
    readonly #sending = new Set<string>();
    readonly #nextId: () => string;
    readonly #listeners = new Set<() => void>();
    /** The decisions the broker kept, the newest first. */
    #history: readonly DecisionRecord[] = [];
    #ready = false;
    constructor(client: LiveClient, nextId: () => string) {
        this.#client = client;
        this.#nextId = nextId;
        client.subscribe(() => this.#watch());
    }
    /** Read the decisions the broker kept: each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.#remember();
        this.#ready = ready;
    }
    async #remember(): Promise<void> {
        const reply: Reply = await this.#client.request(this.#nextId(), { action: 'list_decisions' });
        if (reply.kind !== 'answered' || reply.result.result !== 'decisions')
            return;
        this.#history = reply.result.decisions;
        for (const listener of [...this.#listeners])
            listener();
    }
    /** Record a confirmation event, which the client has already applied once. */
    observe(event: UiEvent): void {
        const body = event.body;
        if (body.kind === 'confirmation_closed') {
            this.#ended.set(body.confirmation_id, CLOSURES[body.reason]);
            return;
        }
        if (body.kind !== 'confirmation')
            return;
        const seen = this.#seen.get(body.confirmation_id);
        if (seen !== undefined && seen.request.revision >= body.revision)
            return;
        this.#seen.set(body.confirmation_id, { request: body, at: event.ts });
        this.#closed.delete(body.confirmation_id);
    }
    getSnapshot(): readonly TaskApproval[] {
        // The newest first, as the history after it is: what was just asked
        // for leads the queue, not what has waited longest.
        const live = [...this.#seen.values()].reverse().map(({ request, at }) => this.#card(request, at));
        // What was decided before this connection, as it was decided.
        const kept = this.#history
            .filter(record => !this.#seen.has(record.confirmation_id))
            .map(record => this.#card({ confirmation_id: record.confirmation_id, revision: 0, chat_id: MAIN_CHAT, summary: record.summary, expires_at: null, digest: '', fields: record.fields, session: false, always: false }, record.decided_at, record.decision));
        return [...live, ...kept];
    }
    subscribe(listener: () => void): () => void {
        this.#listeners.add(listener);
        const unsubscribe = this.#client.subscribe(listener);
        return () => {
            this.#listeners.delete(listener);
            unsubscribe();
        };
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
    #card(request: ConfirmationRequest, at: string, kept?: Decision): TaskApproval {
        const decision = kept ?? this.#client.decision(request.confirmation_id);
        const status: ApprovalStatus = decision === 'approve' ? 'allowed-once' : decision === 'approve_session' ? 'allowed-task' : decision === 'approve_always' ? 'allowed-always' : decision === 'deny' ? 'denied' : this.#ended.get(request.confirmation_id) ?? this.#closed.get(request.confirmation_id) ?? 'pending';
        const forTask = request.session === true;
        const always = request.always === true;
        // The broker's own words for approving, when it has them: Remember it.
        const approve = typeof request.approve === 'string' ? request.approve : 'Allow once';
        const expires = typeof request.expires_at === 'string' && clockTime(request.expires_at) !== '' ? expiryWords(request.expires_at, at) : '';
        return {
            id: request.confirmation_id,
            revision: request.revision,
            chatId: appChat(request.chat_id ?? MAIN_CHAT),
            threadId: '',
            kind: 'tool',
            title: 'Hatchling asks before it continues',
            description: request.summary,
            resource: expires === '' ? 'This request does not expire.' : `This request waits for your answer until ${expires}.`,
            detail: [
                `${approve} answers this request as described, and only this version of it.`,
                forTask ? "Allow for task also answers every request like it until the agent's session ends; in a chat, that's this answer." : '',
                always ? 'Allow always answers every request like it, in every session, until you revoke it in Permissions.' : '',
                'Your answer is recorded once.',
            ].filter(part => part !== '').join(' '),
            scopes: [...(['once'] as const), ...(forTask ? (['task'] as const) : []), ...(always ? (['always'] as const) : [])],
            ...(approve === 'Allow once' ? {} : { approveLabel: approve }),
            status,
            time: clockTime(at),
            report: reviewText(request),
            fields: request.fields,
        };
    }
    async #dispatch(attempt: ApprovalAttempt): Promise<ApprovalResult> {
        const offered = this.#seen.get(attempt.requestId)?.request;
        if (attempt.decision === 'task' && offered?.session !== true)
            return rejected('Allowing a whole task is not offered. Choose Allow once or Deny.');
        if (attempt.decision === 'always' && offered?.always !== true)
            return rejected('Allowing always is not offered. Choose another answer.');
        if (this.#sending.has(attempt.requestId))
            return unknown('An answer to this request is already on its way. Check its status instead of answering again.');
        const action = this.#client.decide(attempt.requestId, attempt.decision === 'once' ? 'approve' : attempt.decision === 'task' ? 'approve_session' : attempt.decision === 'always' ? 'approve_always' : 'deny');
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
            case 'conversation_started':
            case 'schedule':
            case 'setup':
            case 'sites':
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
            case 'input_done':
            case 'input_failed':
            case 'signed_in_kept':
            case 'profiles':
            case 'sign_ins':
            case 'usage':
            case 'model':
            case 'feed':
            case 'suggestions':
            case 'goals':
            case 'memory':
            case 'decisions':
            case 'code_taken':
            case 'code_declined':
            case 'code_refused':
                return unknown('Hatchling answered with something other than a decision, so the answer is not confirmed. Check its status.');
        }
    }
}
