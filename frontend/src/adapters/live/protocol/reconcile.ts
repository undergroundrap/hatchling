// Client-side replay reconciliation . Events carry strictly increasing
// sequence numbers. The ledger applies each one at most once, keeps the mark
// to resume from after a reconnect, and never presents a decided confirmation
// again, however often its request is replayed.
import type { ConfirmationRequest, Decision, UiAction, UiEvent } from './generated.js';
/** What applying one event did. */
export type Applied = 'applied' | 'duplicate';
export class EventLedger {
    #mark: number | undefined;
    readonly #pending = new Map<string, ConfirmationRequest>();
    readonly #decided = new Map<string, Decision>();
    /** The `after_seq` to resume from: the highest sequence applied, or `null` before any. */
    afterSeq(): number | null {
        return this.#mark ?? null;
    }
    /** Apply one event. An event at or below the mark is a duplicate and changes nothing. */
    apply(event: UiEvent): Applied {
        if (this.#mark !== undefined && event.seq <= this.#mark)
            return 'duplicate';
        this.#mark = event.seq;
        const body = event.body;
        if (body.kind === 'confirmation')
            this.#offer(body);
        else if (body.kind === 'confirmation_decided') {
            this.#decided.set(body.confirmation_id, body.decision);
            this.#pending.delete(body.confirmation_id);
        }
        return 'applied';
    }
    #offer(request: ConfirmationRequest): void {
        if (this.#decided.has(request.confirmation_id))
            return;
        const current = this.#pending.get(request.confirmation_id);
        if (current === undefined || request.revision > current.revision)
            this.#pending.set(request.confirmation_id, request);
    }
    /** Confirmations awaiting a decision, in the order they were first offered. */
    pending(): readonly ConfirmationRequest[] {
        return [...this.#pending.values()];
    }
    /** The decision recorded for a confirmation, if one was. */
    decision(confirmationId: string): Decision | undefined {
        return this.#decided.get(confirmationId);
    }
    /**
     * The action deciding a pending confirmation, at the revision and action
     * digest on screen. A
     * confirmation that is not pending (decided, or never offered) gets no
     * action: a decision is never sent twice.
     */
    decide(confirmationId: string, decision: Decision): UiAction | undefined {
        const request = this.#pending.get(confirmationId);
        if (request === undefined)
            return undefined;
        return { action: 'decide', confirmation_id: confirmationId, revision: request.revision, digest: request.digest, decision };
    }
}
