// Starting an idea from the Ideas page: which chat its text is put in, and the
// conversation of its own it is given there. A new conversation is the owner's
// to start: this is called from their own press on the page, never from a
// Command, so automation can't reset what the agent is given.
import type { ConversationPort } from '../../session/conversations.js';
/** A chat as starting an idea sees it. */
export interface IdeaChat {
    readonly id: string;
    readonly channelId: string | null;
    readonly archived?: boolean;
}
/** The owner's own chat with Hatchling. */
const MAIN = 'main';
/**
 * What starting an idea puts in the chat box: its title and what it would do.
 * Never its note: of a card kept from a chat, that holds where the run said it
 * stopped, the agent's unchecked words, which a new run isn't given.
 */
export function ideaText(idea: {
    readonly title: string;
    readonly summary: string;
}): string {
    return `${idea.title}: ${idea.summary}`;
}
/**
 * The chat an idea's text is put in: the open one, unless that is a connected
 * app's or isn't there, and then the owner's own. None when that chat is
 * archived or there is none: the text then goes where the chat box is, with no
 * new conversation.
 */
export function ideaChat(chats: readonly IdeaChat[], openId: string): string | null {
    const open = chats.find(chat => chat.id === openId);
    const own = open && open.channelId === null ? open : chats.find(chat => chat.id === MAIN);
    return own && own.channelId === null && !own.archived ? own.id : null;
}
/**
 * Gives the started idea `idea` a conversation of its own in `chatId`, so the
 * agent is given nothing said before it, and Hatchling knows which idea the
 * conversation is for. Answers with a sentence for the owner when one
 * couldn't be started, and with nothing when it was, or when there is no way
 * to start one here (a preview) or no chat to start it in.
 */
export async function freshConversation(port: ConversationPort | undefined, chatId: string | null, idea: string): Promise<string | null> {
    if (!port || chatId === null)
        return null;
    const started = await port.start(chatId, idea);
    return started.ok ? null : `${started.message} Your idea is in the chat box of the conversation that is open.`;
}
/** What starting an idea uses of the app. */
export interface IdeaStart {
    readonly conversations: ConversationPort | undefined;
    readonly state: {
        readonly chats: readonly IdeaChat[];
    };
    readonly ui: {
        readonly chatId: string;
    };
    toast(message: string, error?: boolean): void;
    prefillComposer(text: string): void;
}
/**
 * Starts the idea `id` the owner pressed: a conversation of its own first, then its
 * text in the chat box, for them to read, change and send. The text is put
 * there whether or not the conversation could be started; when it couldn't,
 * the owner is told before they send anything. Nothing is sent.
 */
export async function startIdea(app: IdeaStart, id: string, text: string): Promise<void> {
    const unstarted = await freshConversation(app.conversations, ideaChat(app.state.chats, app.ui.chatId), id);
    if (unstarted !== null)
        app.toast(unstarted, true);
    app.prefillComposer(text);
}
