// The sign-ins the broker fills for the owner's signed-in sites (WO-030), as
// the broker lists them: by label and origin, never a username or password.
// Loaded when the connection opens and after each change; kept or deleted
// only by the owner, here. A kept sign-in goes to the broker as its own
// request, never as a Command.
import type { CommandResult } from '../../protocol/models.js';
import type { SignInEntry, SignInPort } from '../../session/sign-ins.js';
import type { LiveClient, Reply } from './client.js';
import type { SignInOp, SignInRefusal, SignInResult, SiteSignIn } from './protocol/generated.js';
const KEPT = 'Kept. The agent can sign in with it on this site; it never sees the password.';
const DELETED = 'Deleted. The agent can no longer sign in with it.';
const GONE = 'This sign-in was already gone.';
const OFF = 'Hatchling keeps no sign-ins: the operator has not given it a place to.';
const FAILED = "Hatchling couldn't change its sign-ins, so nothing changed. Its log says why.";
const UNCONFIRMED = 'Hatchling answered with something else, so this is not confirmed. Check the list again.';
const REFUSALS: Readonly<Record<SignInRefusal, string>> = {
    no_profile: 'That signed-in site is gone, so nothing was kept.',
    bad_origin: 'Choose one of the site’s own addresses, so nothing was kept.',
    not_the_profiles: 'That address is not one of the signed-in site’s, so nothing was kept.',
    bad_label: 'Give the sign-in a name, without line breaks, so nothing was kept.',
    no_password: 'Type the password, so nothing was kept.',
    too_long: 'The name, username or password is too long, so nothing was kept.',
    full: 'Hatchling keeps as many sign-ins as it can. Delete one first.',
};
export class LiveSignIns implements SignInPort {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    #listed: readonly SiteSignIn[] = [];
    #ready = false;
    constructor(client: LiveClient, changed: () => void, nextId: () => string) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        client.subscribe(() => this.#watch());
    }
    /** The sign-ins of the signed-in site `profileId`, oldest first. */
    of(profileId: string): {
        id: string;
        label: string;
        origin: string;
    }[] {
        return this.#listed.filter(listed => listed.profile_id === profileId).map(listed => ({ id: listed.sign_in_id, label: listed.label, origin: listed.origin }));
    }
    /** Ask the broker for the list. */
    async refresh(): Promise<void> {
        const outcome = await this.#ask({ op: 'list' });
        if (outcome?.result !== 'listed')
            return;
        this.#listed = outcome.sign_ins;
        this.#changed();
    }
    store(entry: SignInEntry): Promise<CommandResult> {
        return this.#change({ op: 'store', profile_id: entry.profileId, origin: entry.origin, label: entry.label, username: entry.username, password: entry.password }, 'stored', KEPT);
    }
    delete(id: string): Promise<CommandResult> {
        return this.#change({ op: 'delete', sign_in_id: id }, 'deleted', DELETED);
    }
    async #change(op: SignInOp, done: 'stored' | 'deleted', message: string): Promise<CommandResult> {
        const reply = await this.#client.request(this.#nextId(), { action: 'manage_sign_ins', op });
        if (reply.kind !== 'answered')
            return { ok: false, message: reply.detail };
        const result = reply.result;
        if (result.result === 'refused')
            return { ok: false, message: 'This connection may not change the sign-ins, so nothing changed.' };
        if (result.result !== 'sign_ins')
            return { ok: false, message: UNCONFIRMED };
        const outcome = result.outcome;
        switch (outcome.result) {
            case 'stored':
            case 'deleted':
                if (outcome.result !== done)
                    return { ok: false, message: UNCONFIRMED };
                await this.refresh();
                return { ok: true, message };
            case 'refused':
                return { ok: false, message: REFUSALS[outcome.reason] };
            case 'not_found':
                await this.refresh();
                return { ok: false, message: GONE };
            case 'off':
                return { ok: false, message: OFF };
            case 'failed':
            case 'listed':
                return { ok: false, message: FAILED };
        }
    }
    /** Load the list each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh();
        this.#ready = ready;
    }
    async #ask(op: SignInOp): Promise<SignInResult | undefined> {
        const reply: Reply = await this.#client.request(this.#nextId(), { action: 'manage_sign_ins', op });
        return reply.kind === 'answered' && reply.result.result === 'sign_ins' ? reply.result.outcome : undefined;
    }
}
