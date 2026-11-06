import type { Command } from '../protocol/models.js';
/** Optional frontend capability. This is NOT an addition to the frozen wire protocol.
 * A live adapter must implement authoritative receipts and deduplicate request IDs.
 * A timeout or thrown exception is always unknown, never proof of rejection. */
export type ChatSendCommand = Extract<Command, {
    type: 'chat.send' | 'chat.attach';
}>;
export type ConnectionKind = 'ready' | 'offline' | 'reconnecting' | 'restarting' | 'unavailable' | 'sign-in-required' | 'locked';
export interface ConnectionState {
    readonly kind: ConnectionKind;
    readonly detail: string;
}
export interface HistoryState {
    readonly phase: 'ready' | 'loading' | 'error';
    readonly hasCache: boolean;
    readonly detail: string;
}
export interface ChatSessionSnapshot {
    readonly connection: ConnectionState;
    readonly histories: Readonly<Record<string, HistoryState>>;
}
export type DeliveryOutcome = {
    readonly state: 'accepted';
    readonly messageId: string | null;
} | {
    readonly state: 'rejected';
    readonly detail: string;
} | {
    readonly state: 'unknown';
    readonly detail: string;
};
export interface SendRequest {
    readonly id: string;
    readonly command: ChatSendCommand;
}
export type PendingPhase = 'queued' | 'sending' | 'failed' | 'unconfirmed';
export interface SavedSend {
    readonly id: string;
    readonly batchId: string;
    readonly command: ChatSendCommand;
    readonly phase: PendingPhase;
    readonly detail: string;
}
export interface SavedDraft {
    readonly chatId: string;
    readonly text: string;
    readonly attachments: readonly string[];
}
export interface ChatRecovery {
    readonly version: 1;
    readonly drafts: readonly SavedDraft[];
    readonly outbox: readonly SavedSend[];
}
export type ReliabilityScenario = 'history-loading' | 'history-error' | 'history-stale' | 'slow-send' | 'send-failed' | 'send-unknown' | 'vm-restarting' | 'offline' | 'service-unavailable' | 'session-expired' | 'attachment-failed' | 'ready';
export interface ChatSessionPort {
    getSnapshot(): ChatSessionSnapshot;
    subscribe(listener: () => void): () => void;
    send(request: SendRequest): Promise<DeliveryOutcome>;
    checkDelivery(requestId: string): Promise<DeliveryOutcome>;
    retryHistory(chatId: string): Promise<void>;
    reconnect(): Promise<void>;
    loadRecovery?(): ChatRecovery | null;
    saveRecovery?(recovery: ChatRecovery): boolean;
    /** Present only on a scripted adapter; no production fault controls. */
    preview?(scenario: ReliabilityScenario, chatId: string): void;
}
