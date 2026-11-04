// Chats and their messages from `chat_message` events . A message's
// id is stable across replays, so a replayed message replaces itself rather
// than appearing twice. Message text is untrusted: the chat view renders a
// text message through the markdown renderer, so it arrives with its link
// forms broken apart (see text.ts). System notices render as plain text.
import type { Chat, Message } from '../../protocol/models.js';
import type { ActivityEvent, ChatMessage, UiEventBody } from './protocol/generated.js';
import { trailLine } from './activity.js';
/** A file sent in a chat . */
export type Attachment = Extract<UiEventBody, {
    readonly kind: 'attachment';
}>;
import { clockTime, inertLinks } from './text.js';
/** The main chat's id: always present, whatever the history holds. */
export const MAIN_CHAT = 'main';
/** The broker's local channel, which the app shows as its main chat. */
export const BROKER_MAIN_CHAT = 'local';
/** The app's chat for a chat id the broker sent. */
export function appChat(wire: string): string {
    return wire === BROKER_MAIN_CHAT ? MAIN_CHAT : wire;
}
/** The broker's chat id for one of the app's chats. */
export function wireChat(app: string): string {
    return app === MAIN_CHAT ? BROKER_MAIN_CHAT : app;
}
function message(entry: ChatMessage, at: string): Message {
    const base = { id: entry.message_id, chatId: appChat(entry.chat_id), time: clockTime(at), source: null, reaction: null };
    switch (entry.author) {
        case 'user':
            return { ...base, kind: 'text', role: 'user', text: inertLinks(entry.text) };
        case 'agent':
            return { ...base, kind: 'text', role: 'agent', text: inertLinks(entry.text) };
        case 'system':
            return { ...base, kind: 'notice', role: 'agent', text: entry.text, evidenceId: null };
    }
}
/** A run's record, as the line under the answer it wrote. */
interface Trail {
    readonly threadId: string;
    readonly line: string;
}
/** The conversations, built from chat events in sequence order. */
export class ChatBook {
    readonly #updated = new Map<string, string>([[MAIN_CHAT, '']]);
    readonly #messages = new Map<string, Message>();
    /** The run that wrote each answer, by its cell id, as the broker said. */
    readonly #runs = new Map<string, string>();
    /** Each run's record, by its cell id. */
    readonly #trails = new Map<string, Trail>();
    /** Apply one message, at its event's time. */
    apply(entry: ChatMessage, at: string): void {
        this.#updated.set(appChat(entry.chat_id), at);
        this.#messages.set(entry.message_id, message(entry, at));
        if (entry.author === 'agent' && typeof entry.run === 'string')
            this.#runs.set(entry.message_id, entry.run);
    }
    /** A run's record, a chat's or scheduled work's: the line under the answer it wrote, opening the record. */
    trail(event: ActivityEvent): void {
        if (event.event === 'run_summary')
            this.#trails.set(event.cell_id, { threadId: event.event_id, line: trailLine(event.tools) });
    }
    /** A file sent in a chat: an attachment message naming the artifact. Its words are plain text. */
    attach(entry: Attachment, at: string): void {
        this.#updated.set(appChat(entry.chat_id), at);
        this.#messages.set(entry.message_id, { id: entry.message_id, chatId: appChat(entry.chat_id), time: clockTime(at), source: null, reaction: null, kind: 'attachment', role: entry.author === 'user' ? 'user' : 'agent', text: entry.text, artifactId: entry.artifact_id });
    }
    /** Whether a chat exists, so an action never targets one that does not. */
    has(chatId: string): boolean {
        return this.#updated.has(chatId);
    }
    /** Fresh chat records. The wire has no chat titles yet, so other chats share one plain name. */
    chats(): Chat[] {
        return [...this.#updated].map(([id, at]) => ({ id, title: id === MAIN_CHAT ? 'Main' : 'Conversation', channelId: null, pinned: false, archived: false, unread: false, ...(at === '' ? {} : { updatedAt: at }) }));
    }
    /**
     * Fresh message records, in the order each was first seen; an answer whose
     * run's record has come is followed by the line saying what the run used.
     */
    messages(): Message[] {
        return [...this.#messages.values()].flatMap(entry => {
            const run = this.#runs.get(entry.id);
            const trail = run === undefined ? undefined : this.#trails.get(run);
            const copy: Message = { ...entry };
            if (trail === undefined)
                return [copy];
            const used: Message = { id: `${entry.id}:trail`, chatId: entry.chatId, time: entry.time, source: null, reaction: null, kind: 'notice', role: 'agent', text: trail.line, evidenceId: trail.threadId };
            return [copy, used];
        });
    }
}
