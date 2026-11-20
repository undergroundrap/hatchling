// The owner's standing grants (WO-036), as the broker keeps them: each a
// key, a scope (24 hours or always), when it was made and when it ends.
// Loaded when the connection opens and after the owner gives a standing yes;
// revoked only by the owner, here, by key. The list is the broker's, never
// this app's guess: until the broker answers it is empty.
import type { CommandResult } from '../../protocol/models.js';
import type { LiveClient, Reply } from './client.js';
import type { GrantOp, GrantResult, StandingGrant, UiEvent } from './protocol/generated.js';
const REVOKED = 'Revoked. The agent asks again the next time it wants a page on this site, in every task, running ones included.';
const GONE = 'This site was no longer allowed.';
const OFF = 'Hatchling keeps no standing grants: the operator has not given it a place to.';
const FAILED = "Hatchling couldn't change its standing grants, so nothing was revoked. Its log says why.";
const UNCONFIRMED = 'Hatchling answered with something else, so the revoke is not confirmed. Check the list again.';
export class LiveSites {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    #grants: readonly StandingGrant[] = [];
    #ready = false;
    constructor(client: LiveClient, changed: () => void, nextId: () => string) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        client.subscribe(() => this.#watch());
    }
    /** The standing grants, in the order the owner made them. */
    grants(): readonly StandingGrant[] {
        return this.#grants;
    }
    /** A standing yes changes the list: ask for it again. */
    observe(event: UiEvent): void {
        if (event.body.kind === 'confirmation_decided' && (event.body.decision === 'approve_always' || event.body.decision === 'approve_day'))
            void this.refresh();
    }
    /** Ask the broker for the list. */
    async refresh(): Promise<void> {
        const outcome = await this.#ask({ op: 'list' });
        if (outcome?.result !== 'listed')
            return;
        this.#grants = outcome.grants;
        this.#changed();
    }
    /** Revoke the grant for `site`, by its key, as listed: it covers nothing from then on. */
    async revoke(site: string): Promise<CommandResult> {
        // The key as listed; a site not listed is still the broker's to say.
        const key = this.#grants.find(entry => entry.target === site)?.key ?? `web.visit:${site}`;
        const reply = await this.#client.request(this.#nextId(), { action: 'manage_grants', op: { op: 'revoke', key } });
        if (reply.kind !== 'answered')
            return { ok: false, message: reply.detail };
        const result = reply.result;
        if (result.result === 'refused')
            return { ok: false, message: 'This connection may not change the standing grants, so nothing was revoked.' };
        if (result.result !== 'grants')
            return { ok: false, message: UNCONFIRMED };
        switch (result.outcome.result) {
            case 'revoked':
                await this.refresh();
                return { ok: true, message: REVOKED };
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
    async #ask(op: GrantOp): Promise<GrantResult | undefined> {
        const reply: Reply = await this.#client.request(this.#nextId(), { action: 'manage_grants', op });
        return reply.kind === 'answered' && reply.result.result === 'grants' ? reply.result.outcome : undefined;
    }
}
