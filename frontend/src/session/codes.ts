// One-time codes a signed-in site asks for (WO-030). The user types the
// code in the browser's view; this port sends it straight to the broker,
// which types it into the page once and forgets it. It is never a Command,
// so it never reaches the command log, automation or the app's state.
import type { CommandResult } from '../protocol/models.js';
export interface CodePort {
    /** Give `code` for the code request the browser `sessionId` is waiting on. */
    give(sessionId: string, code: string): Promise<CommandResult>;
}
