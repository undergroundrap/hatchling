// A new conversation, started by the owner (WO-026). The app's header sends
// it straight to the broker, which gives the agent nothing said before it.
// It is never a Command, so automation can't start one: what the agent is
// given is the owner's to reset.
import type { CommandResult } from '../protocol/models.js';
export interface ConversationPort {
    /** Start a new conversation in `chatId`. */
    start(chatId: string): Promise<CommandResult>;
}
