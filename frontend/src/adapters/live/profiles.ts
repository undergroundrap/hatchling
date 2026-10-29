// The sites the owner kept signed in for the agent (WO-030), as the broker
// keeps them. Loaded when the connection opens and after the owner keeps a
// sign-in; signed out or deleted only by the owner, here. The list is the
// broker's, never this app's guess: until the broker answers it is empty.
import type { CommandResult, SignedInSite } from '../../protocol/models.js';
import type { LiveClient, Reply } from './client.js';
import type { ProfileOp, ProfileResult } from './protocol/generated.js';
const SIGNED_OUT = 'Signed out. Its sites still open in a browser of their own, signed out, until you delete it.';
const DELETED = 'Deleted. The agent asks again the next time it wants a page on these sites.';
const GONE = 'This signed-in site was already gone.';
export const PROFILES_OFF = 'Hatchling keeps no signed-in sites: the operator has not given it a place to.';
export const PROFILES_FAILED = "Hatchling couldn't change its signed-in sites, so nothing changed. Its log says why.";
const UNCONFIRMED = 'Hatchling answered with something else, so this is not confirmed. Check the list again.';
export class LiveProfiles {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    #profiles: readonly SignedInSite[] = [];
    #ready = false;
    constructor(client: LiveClient, changed: () => void, nextId: () => string) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        client.subscribe(() => this.#watch());
    }
    /** The signed-in sites, the oldest kept first. */
    profiles(): readonly SignedInSite[] {
        return this.#profiles;
    }
    /** Ask the broker for the list. */
    async refresh(): Promise<void> {
        const outcome = await this.#ask({ op: 'list' });
        if (outcome?.result !== 'listed')
            return;
        this.#profiles = outcome.profiles.map(profile => ({ id: profile.profile_id, sites: [...profile.sites], signedIn: profile.signed_in, lastUsed: profile.last_used, signIns: [] }));
        this.#changed();
    }
    /** Sign out of `id`: its session is dropped, its sites kept. */
    signOut(id: string): Promise<CommandResult> {
        return this.#change({ op: 'sign_out', profile_id: id }, 'signed_out', SIGNED_OUT);
    }
    /** Delete `id`: its session and its sites. */
    delete(id: string): Promise<CommandResult> {
        return this.#change({ op: 'delete', profile_id: id }, 'deleted', DELETED);
    }
    async #change(op: ProfileOp, done: 'signed_out' | 'deleted', message: string): Promise<CommandResult> {
        const reply = await this.#client.request(this.#nextId(), { action: 'manage_profiles', op });
        if (reply.kind !== 'answered')
            return { ok: false, message: reply.detail };
        const result = reply.result;
        if (result.result === 'refused')
            return { ok: false, message: 'This connection may not change the signed-in sites, so nothing changed.' };
        if (result.result !== 'profiles')
            return { ok: false, message: UNCONFIRMED };
        switch (result.outcome.result) {
            case 'signed_out':
            case 'deleted':
                if (result.outcome.result !== done)
                    return { ok: false, message: UNCONFIRMED };
                await this.refresh();
                return { ok: true, message };
            case 'not_found':
                await this.refresh();
                return { ok: false, message: GONE };
            case 'off':
                return { ok: false, message: PROFILES_OFF };
            case 'failed':
            case 'listed':
                return { ok: false, message: PROFILES_FAILED };
        }
    }
    /** Load the list each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh();
        this.#ready = ready;
    }
    async #ask(op: ProfileOp): Promise<ProfileResult | undefined> {
        const reply: Reply = await this.#client.request(this.#nextId(), { action: 'manage_profiles', op });
        return reply.kind === 'answered' && reply.result.result === 'profiles' ? reply.result.outcome : undefined;
    }
}
