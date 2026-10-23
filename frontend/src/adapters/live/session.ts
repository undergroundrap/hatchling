// Sending chat messages, through the UI's existing delivery surface
// (ChatSessionPort). A send becomes the typed `send_message` action under the
// UI's own request id, so a retried request is never sent twice. Delivery is
// what the broker said: accepted with the message's id, refused with a
// reason, or unknown when no answer came. A timeout is never a rejection.
import type { ChatSessionPort, ChatSessionSnapshot, DeliveryOutcome, SendRequest } from '../../session/chat-session.js';
import type { ChatBook } from './chat.js';
import { wireChat } from './chat.js';
import type { LiveClient } from './client.js';
import type { ActionRefusal, UiAction } from './protocol/generated.js';
const REFUSALS: Readonly<Record<ActionRefusal, string>> = {
    not_connected: 'Hatchling is not connected to its broker, so the message was not sent.',
    bad_request: 'Hatchling could not read the message, so it was not sent.',
    unavailable: 'Hatchling cannot take messages right now, so it was not sent.',
    not_granted: 'This connection is not allowed to send messages, so it was not sent.',
};
const UNCONFIRMED: DeliveryOutcome = { state: 'unknown', detail: 'Hatchling has not confirmed this message. Check the conversation before sending it again.' };
export class LiveChatSession implements ChatSessionPort {
    readonly #client: LiveClient;
    readonly #chats: ChatBook;
    readonly #latest: (artifactId: string) => number | undefined;
    readonly #sends = new Map<string, Promise<DeliveryOutcome>>();
    /** `latest` names an artifact's newest revision, which is the one an attachment sends. */
    constructor(client: LiveClient, chats: ChatBook, latest: (artifactId: string) => number | undefined) {
        this.#client = client;
        this.#chats = chats;
        this.#latest = latest;
    }
    /** No history-loading states yet: history arrives as events, after the connection opens. */
    getSnapshot(): ChatSessionSnapshot {
        return { connection: this.#client.connection(), histories: {} };
    }
    subscribe(listener: () => void): () => void {
        return this.#client.subscribe(listener);
    }
    /** One outcome per request id: a repeated request gets the first outcome. */
    send(request: SendRequest): Promise<DeliveryOutcome> {
        const prior = this.#sends.get(request.id);
        if (prior !== undefined)
            return prior;
        const outcome = this.#send(request);
        this.#sends.set(request.id, outcome);
        return outcome;
    }
    /** What became of a request. With no way to ask the broker yet, an unanswered one stays unknown. */
    async checkDelivery(requestId: string): Promise<DeliveryOutcome> {
        return await this.#sends.get(requestId) ?? UNCONFIRMED;
    }
    /** Ask again for every event since the last one applied. */
    retryHistory(): Promise<void> {
        this.#client.resync();
        return Promise.resolve();
    }
    reconnect(): Promise<void> {
        this.#client.connect();
        return Promise.resolve();
    }
    async #send(request: SendRequest): Promise<DeliveryOutcome> {
        const command = request.command;
        if (!this.#chats.has(command.chatId))
            return { state: 'rejected', detail: 'This conversation is no longer available.' };
        let action: UiAction;
        if (command.type === 'chat.attach') {
            const revision = this.#latest(command.artifactId);
            if (revision === undefined)
                return { state: 'rejected', detail: 'This file is no longer available to send.' };
            action = { action: 'attach_artifact', chat_id: wireChat(command.chatId), artifact_id: command.artifactId, revision };
        }
        else
            action = { action: 'send_message', chat_id: wireChat(command.chatId), text: command.text };
        const reply = await this.#client.request(request.id, action);
        if (reply.kind === 'unsent')
            return { state: 'rejected', detail: reply.detail };
        if (reply.kind === 'unknown')
            return { state: 'unknown', detail: reply.detail };
        switch (reply.result.result) {
            case 'message_accepted':
                return { state: 'accepted', messageId: reply.result.message_id };
            case 'refused':
                return { state: 'rejected', detail: REFUSALS[reply.result.reason] };
            case 'decided':
            case 'decision_rejected':
            case 'schedule':
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
            case 'code_taken':
            case 'code_declined':
            case 'code_refused':
                return UNCONFIRMED;
        }
    }
}
