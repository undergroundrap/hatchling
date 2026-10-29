// The sign-ins the broker fills for the user's signed-in sites (WO-030).
// Keeping one is the one path a site password takes through this app: the
// form hands it to this port, which sends it straight to the broker as its
// own request. It is never a Command, so it never reaches the command log,
// automation or the app's state; the state lists sign-ins by label only.
import type { CommandResult } from '../protocol/models.js';
/** A sign-in the user types to keep: for one signed-in site and one exact origin among its sites. */
export interface SignInEntry {
    readonly profileId: string;
    /** `https://host[:port]`. */
    readonly origin: string;
    readonly label: string;
    readonly username: string;
    readonly password: string;
}
export interface SignInPort {
    /** Keep `entry` with the broker. The answer says what was kept, or why not; never the secrets. */
    store(entry: SignInEntry): Promise<CommandResult>;
}
