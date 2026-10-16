// The live adapter's connection to the broker, over a transport the
// host supplies. It speaks the frontend protocol: `hello`, then
// `subscribe` from the last event applied, then events and action results.
// Every frame is checked against the schema before it is read; one that does
// not conform ends the connection loudly instead of being skipped, because a
// skipped event would be lost for good.
import type { ConnectionState } from '../../session/chat-session.js';
import { decodeResponse, encodeRequest } from './protocol/decode.js';
import type { Decision, UiAction, UiActionResult, UiEvent } from './protocol/generated.js';
import { EventLedger } from './protocol/reconcile.js';
import { hello, readHandshake } from './protocol/version.js';
/** What the transport reports. A connection ends with exactly one `closed`. */
export interface TransportHandlers {
    opened(): void;
    received(text: string): void;
    closed(detail: string): void;
}
/**
 * One ordered, reliable, message-framed duplex channel ( transport).
 * `send` never throws: a frame that cannot be sent ends the connection.
 */
export interface UiTransport {
    connect(handlers: TransportHandlers): void;
    send(text: string): void;
    close(): void;
}
/** What became of an action: answered, never sent, or sent with no answer. */
export type Reply = {
    readonly kind: 'answered';
    readonly result: UiActionResult;
} | {
    readonly kind: 'unsent';
    readonly detail: string;
} | {
    readonly kind: 'unknown';
    readonly detail: string;
};
const CONNECTING: ConnectionState = { kind: 'reconnecting', detail: 'Connecting to Hatchling…' };
const READY: ConnectionState = { kind: 'ready', detail: '' };
const UNREADABLE = 'Hatchling sent a message this version of the app cannot read, so the app disconnected rather than miss it. Reload the app, or restart the broker, so both are the same version.';
const REPEATED = 'Hatchling repeated its handshake in the middle of the session, so the app disconnected.';
const UNANSWERED = 'The connection closed before Hatchling answered. It may still have been received.';
const NOT_CONNECTED = 'The app is not connected to Hatchling, so nothing was sent.';
export class LiveClient {
    readonly #transport: UiTransport;
    readonly #name: string;
    readonly #token: string | undefined;
    readonly #sink: (event: UiEvent) => string | undefined;
    readonly #ledger = new EventLedger();
    readonly #listeners = new Set<() => void>();
    readonly #replies = new Map<string, Promise<Reply>>();
    readonly #settlers = new Map<string, (reply: Reply) => void>();
    #phase: 'idle' | 'greeting' | 'open' | 'ended' = 'idle';
    #generation = 0;
    #connection: ConnectionState = CONNECTING;
    /**
     * `sink` receives each event once, in order, before listeners hear of it.
     * It returns a sentence, having changed nothing, when an event breaks the
 * protocol's rules (control moved by a handoff the table does not
     * allow). That ends the connection, and the event is not counted as
     * applied, so a reconnect does not skip past it.
     */
    constructor(transport: UiTransport, name: string, sink: (event: UiEvent) => string | undefined, token?: string) {
        this.#transport = transport;
        this.#name = name;
        this.#sink = sink;
        this.#token = token;
    }
    connection(): ConnectionState {
        return this.#connection;
    }
    subscribe(listener: () => void): () => void {
        this.#listeners.add(listener);
        return () => { this.#listeners.delete(listener); };
    }
    /** Connect, or reconnect after a close. A connection already open or opening is kept. */
    connect(): void {
        if (this.#phase !== 'idle')
            return;
        const generation = ++this.#generation;
        const current = (): boolean => generation === this.#generation;
        this.#phase = 'greeting';
        this.#update(CONNECTING);
        this.#transport.connect({
            opened: () => {
                if (current())
                    this.#transport.send(encodeRequest(hello(this.#name, undefined, this.#token)));
            },
            received: text => {
                if (current())
                    this.#receive(text);
            },
            closed: detail => {
                if (current())
                    this.#lost({ kind: 'offline', detail }, 'idle');
            },
        });
    }
    /** Ask again for every event after the last one applied. Never resends an action. */
    resync(): void {
        if (this.#phase === 'open')
            this.#transport.send(encodeRequest({ type: 'subscribe', after_seq: this.#ledger.afterSeq() }));
    }
    /** The decision the broker recorded for a confirmation, if it has. */
    decision(confirmationId: string): Decision | undefined {
        return this.#ledger.decision(confirmationId);
    }
    /** The action deciding a pending confirmation at its current revision, if it is pending. */
    decide(confirmationId: string, decision: Decision): UiAction | undefined {
        return this.#ledger.decide(confirmationId, decision);
    }
    /**
     * Send an action under the caller's request id. While one is waiting, the
     * same id gets the same answer and nothing is sent twice.
     */
    request(requestId: string, action: UiAction): Promise<Reply> {
        const waiting = this.#replies.get(requestId);
        if (waiting !== undefined)
            return waiting;
        if (this.#phase !== 'open')
            return Promise.resolve({ kind: 'unsent', detail: NOT_CONNECTED });
        const reply = new Promise<Reply>(resolve => { this.#settlers.set(requestId, resolve); });
        this.#replies.set(requestId, reply);
        this.#transport.send(encodeRequest({ type: 'action', request_id: requestId, action }));
        return reply;
    }
    /** Close for good. */
    dispose(): void {
        this.#lost({ kind: 'offline', detail: 'The app closed its connection.' }, 'ended');
        this.#transport.close();
        this.#listeners.clear();
    }
    #receive(text: string): void {
        const response = decodeResponse(text);
        if (response === undefined) {
            this.#fail(UNREADABLE);
            return;
        }
        if (this.#phase === 'greeting') {
            const handshake = readHandshake(response);
            if (handshake.status !== 'open') {
                this.#fail(handshake.message);
                return;
            }
            this.#phase = 'open';
            this.#update(READY);
            this.resync();
            return;
        }
        switch (response.type) {
            case 'event': {
                const mark = this.#ledger.afterSeq();
                if (mark !== null && response.seq <= mark)
                    return;
                const broken = this.#sink(response);
                if (broken !== undefined) {
                    this.#fail(broken);
                    return;
                }
                this.#ledger.apply(response);
                this.#notify();
                return;
            }
            case 'action_result':
                this.#settle(response.request_id, { kind: 'answered', result: response.result });
                return;
            case 'welcome':
            case 'incompatible':
                this.#fail(REPEATED);
                return;
        }
    }
    /** End the connection from this side, saying why. */
    #fail(detail: string): void {
        this.#lost({ kind: 'unavailable', detail }, 'idle');
        this.#transport.close();
    }
    #lost(connection: ConnectionState, phase: 'idle' | 'ended'): void {
        this.#generation++;
        this.#phase = phase;
        for (const requestId of [...this.#settlers.keys()])
            this.#settle(requestId, { kind: 'unknown', detail: UNANSWERED });
        this.#update(connection);
    }
    /** Answer a waiting request once; an answer for no waiting request is dropped. */
    #settle(requestId: string, reply: Reply): void {
        this.#settlers.get(requestId)?.(reply);
        this.#settlers.delete(requestId);
        this.#replies.delete(requestId);
    }
    #update(connection: ConnectionState): void {
        this.#connection = connection;
        this.#notify();
    }
    #notify(): void {
        for (const listener of [...this.#listeners])
            listener();
    }
}
