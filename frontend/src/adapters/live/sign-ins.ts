// The sign-ins the broker fills for the owner's signed-in sites (WO-030), as
// the broker lists them: by label and origin, never a username or password.
// Loaded when the connection opens and after each change. The app can list
// and delete them. It can't add one: a sign-in to keep is typed at the owner's
// own terminal, and the UI socket refuses one.
import type { CommandResult } from '../../protocol/models.js';
import type { LiveClient, Reply } from './client.js';
import type { SignInOp, SignInResult, SiteSignIn } from './protocol/generated.js';
const DELETED = 'Deleted. The agent can no longer sign in with it.';
const GONE = 'This sign-in was already gone.';
const OFF = 'Hatchling keeps no sign-ins: the operator has not given it a place to.';
const FAILED = "Hatchling couldn't change its sign-ins, so nothing changed. Its log says why.";
const UNCONFIRMED = 'Hatchling answered with something else, so this is not confirmed. Check the list again.';
export class LiveSignIns {
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
    async delete(id: string): Promise<CommandResult> {
        const reply = await this.#client.request(this.#nextId(), { action: 'manage_sign_ins', op: { op: 'delete', sign_in_id: id } });
        if (reply.kind !== 'answered')
            return { ok: false, message: reply.detail };
        const result = reply.result;
        if (result.result === 'refused')
            return { ok: false, message: 'This connection may not change the sign-ins, so nothing changed.' };
        if (result.result !== 'sign_ins')
            return { ok: false, message: UNCONFIRMED };
        const outcome = result.outcome;
        switch (outcome.result) {
            case 'deleted':
                await this.refresh();
                return { ok: true, message: DELETED };
            case 'stored':
                return { ok: false, message: UNCONFIRMED };
            case 'not_found':
                await this.refresh();
                return { ok: false, message: GONE };
            case 'off':
                return { ok: false, message: OFF };
            case 'refused':
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
