// Closing one of the owner's goals, as reached or set aside. Connected to
// Hatchling a closed goal is kept, and can't be reopened, so how it ended is
// asked when it's closed. Never a Command: a Command's close names no ending.
import type { CommandResult } from '../protocol/models.js';
export interface GoalsPort {
    /** Close the goal `id`, `achieved` or set aside. */
    close(id: string, achieved: boolean): Promise<CommandResult>;
}
