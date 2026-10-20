// The sites the owner always allows the agent to read (WO-028), as the
// broker keeps them. Loaded when the connection opens and after the owner
// allows one always; revoked only by the owner, here. The list is the
// broker's, never this app's guess: until the broker answers it is empty.
import type { CommandResult } from '../../protocol/models.js';
import type { LiveClient, Reply } from './client.js';
import type { AllowedSite, SiteOp, SiteResult, UiEvent } from './protocol/generated.js';
const REVOKED = 'Revoked. The agent asks again the next time it wants a page on this site.';
const GONE = 'This site was no longer allowed.';
const OFF = 'Hatchling keeps no allowed sites: the operator has not given it a place to.';
const FAILED = "Hatchling couldn't change its allowed sites, so nothing was revoked. Its log says why.";
const UNCONFIRMED = 'Hatchling answered with something else, so the revoke is not confirmed. Check the list again.';
export class LiveSites {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    #sites: readonly AllowedSite[] = [];
    #ready = false;
    constructor(client: LiveClient, changed: () => void, nextId: () => string) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        client.subscribe(() => this.#watch());
    }
    /** The sites, in the order the owner allowed them. */
    sites(): readonly AllowedSite[] {
        return this.#sites;
    }
    /** A decision to allow a site always changes the list: ask for it again. */
    observe(event: UiEvent): void {
        if (event.body.kind === 'confirmation_decided' && event.body.decision === 'approve_always')
            void this.refresh();
    }
    /** Ask the broker for the list. */
    async refresh(): Promise<void> {
        const outcome = await this.#ask({ op: 'list' });
        if (outcome?.result !== 'listed')
            return;
        this.#sites = outcome.sites;
        this.#changed();
    }
    /** Revoke `site`: the next page on it asks the owner again. */
    async revoke(site: string): Promise<CommandResult> {
        const reply = await this.#client.request(this.#nextId(), { action: 'manage_sites', op: { op: 'revoke', site } });
        if (reply.kind !== 'answered')
            return { ok: false, message: reply.detail };
        const result = reply.result;
        if (result.result === 'refused')
            return { ok: false, message: 'This connection may not change the allowed sites, so nothing was revoked.' };
        if (result.result !== 'sites')
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
    async #ask(op: SiteOp): Promise<SiteResult | undefined> {
        const reply: Reply = await this.#client.request(this.#nextId(), { action: 'manage_sites', op });
        return reply.kind === 'answered' && reply.result.result === 'sites' ? reply.result.outcome : undefined;
    }
}
