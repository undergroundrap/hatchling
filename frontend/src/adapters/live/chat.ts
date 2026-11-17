// Chats and their messages from `chat_message` events . A message's
// id is stable across replays, so a replayed message replaces itself rather
// than appearing twice. Message text is untrusted: the chat view renders a
// text message through the markdown renderer, so it arrives with its link
// forms broken apart (see text.ts). System notices render as plain text.
import type { Chat, Message } from '../../protocol/models.js';
import type { ActivityEvent, ChatMessage, FileOffered, FileWithheld, UiEventBody } from './protocol/generated.js';
import { trailLine } from './activity.js';
/** A file sent in a chat . */
export type Attachment = Extract<UiEventBody, {
    readonly kind: 'attachment';
}>;
import type { FileIn } from './inbox.js';
import { SECRET_SUFFIX, isInboxName, isLibraryName, sizeWords } from '../../session/inbox.js';
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
/** What the chat asks under an answer whose run ran out of steps. */
export const KEEP_ASK = 'The agent ran out of steps before it finished. Keep what you asked for later, on your Ideas page?';
/** The one choice it offers. */
export const KEEP = 'Keep for later';
/** How the offer to keep is named, after the answer it is under. */
const KEEP_SUFFIX = ':keep';
/** What the owner is told of a file given that holds a secret Hatchling keeps. */
/** Why a file the agent tried to give was withheld. */
export const WITHHELD = 'it holds a secret Hatchling keeps. Giving files is off for this conversation; a new conversation turns it back on.';
export const HOLDS_SECRET = 'This file holds a secret Hatchling keeps, or it couldn’t be checked. Hatchling reads what’s in it, and with a hosted model, so does the model’s provider. Removing it from the agent’s inbox stops later reads; it can’t undo one already made.';
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
    /** The run behind each answer in the owner's own chat that ran out of steps, by the answer's id. */
    readonly #unfinished = new Map<string, string>();
    /** The runs whose unfinished asks the owner kept. */
    readonly #kept = new Set<string>();
    /** Apply one message, at its event's time. */
    apply(entry: ChatMessage, at: string): void {
        this.#updated.set(appChat(entry.chat_id), at);
        this.#messages.set(entry.message_id, message(entry, at));
        if (entry.author === 'agent' && typeof entry.run === 'string') {
            this.#runs.set(entry.message_id, entry.run);
            // The broker's word that its run ran out of steps: the owner may keep what they asked.
            if (entry.out_of_steps === true && entry.chat_id === BROKER_MAIN_CHAT)
                this.#unfinished.set(entry.message_id, entry.run);
        }
    }
    /** The run an offer to keep is for, by the offer's id; none for any other message. */
    keepable(id: string): string | undefined {
        return id.endsWith(KEEP_SUFFIX) ? this.#unfinished.get(id.slice(0, -KEEP_SUFFIX.length)) : undefined;
    }
    /** The owner kept what they asked in `run`: its offer shows the choice made. */
    kept(run: string): void {
        this.#kept.add(run);
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
    /**
     * A file the owner gave the agent, as the broker put it (WO-041): a notice
     * in the app's words, by the inbox's name for it, which is plain ASCII.
     * The owner's own name for it is never in the text: the chat shows it
     * beside the notice, inert. One holding a secret Hatchling keeps, or that
     * couldn't be checked, is followed by a warning.
     */
    fileIn(entry: FileIn, at: string): void {
        const chatId = appChat(entry.chat_id);
        const time = clockTime(at);
        this.#updated.set(chatId, at);
        const where = isInboxName(entry.placed_as) ? ` It was put in its inbox as ${entry.placed_as}, ${sizeWords(entry.bytes)}.` : '';
        this.#messages.set(entry.message_id, { id: entry.message_id, chatId, time, source: null, reaction: null, kind: 'notice', role: 'agent', text: `You gave Hatchling a file.${where}`, evidenceId: null });
        if (entry.holds_secret)
            this.#messages.set(`${entry.message_id}${SECRET_SUFFIX}`, { id: `${entry.message_id}${SECRET_SUFFIX}`, chatId, time, source: null, reaction: null, kind: 'error', role: 'agent', text: HOLDS_SECRET, evidenceId: null });
    }
    /**
     * The broker's word that the agent gave the owner a file (WO-041): a
     * notice of the app's own, by the broker's plain name for it, which
     * is shown only when it is one.
     */
    fileOffered(entry: FileOffered, at: string): void {
        const chatId = appChat(entry.chat_id);
        this.#updated.set(chatId, at);
        const named = isLibraryName(entry.file.name) ? `: ${entry.file.name}, ${sizeWords(entry.file.bytes)}` : `, ${sizeWords(entry.file.bytes)}`;
        this.#messages.set(entry.message_id, { id: entry.message_id, chatId, time: clockTime(at), source: null, reaction: null, kind: 'notice', role: 'agent', text: `Hatchling gave you a file${named}. It is kept in Hatchling’s Library.`, evidenceId: null });
    }
    /** The broker's word that a file the agent tried to give the owner was withheld. */
    fileWithheld(entry: FileWithheld, at: string): void {
        const chatId = appChat(entry.chat_id);
        this.#updated.set(chatId, at);
        const name = entry.name ?? null;
        const named = name !== null && isLibraryName(name) ? `, ${name},` : '';
        this.#messages.set(entry.message_id, { id: entry.message_id, chatId, time: clockTime(at), source: null, reaction: null, kind: 'error', role: 'agent', text: `Hatchling tried to give you a file${named} and it was withheld: ${WITHHELD}`, evidenceId: null });
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
     * run's record has come is followed by the line saying what the run used,
     * and one whose run ran out of steps by the offer to keep what was asked.
     */
    messages(): Message[] {
        return [...this.#messages.values()].flatMap(entry => {
            const run = this.#runs.get(entry.id);
            const trail = run === undefined ? undefined : this.#trails.get(run);
            const shown: Message[] = [{ ...entry }];
            if (trail !== undefined)
                shown.push({ id: `${entry.id}:trail`, chatId: entry.chatId, time: entry.time, source: null, reaction: null, kind: 'notice', role: 'agent', text: trail.line, evidenceId: trail.threadId });
            const unfinished = this.#unfinished.get(entry.id);
            if (unfinished !== undefined)
                shown.push({ id: `${entry.id}${KEEP_SUFFIX}`, chatId: entry.chatId, time: entry.time, source: null, reaction: null, kind: 'choice', role: 'agent', text: KEEP_ASK, options: [KEEP], selected: this.#kept.has(unfinished) ? KEEP : null });
            return shown;
        });
    }
}
