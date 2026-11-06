// What's set up on this machine, read when the connection opens and whenever the
// owner comes back from their terminal. Only the broker says; this app never
// guesses, and what it reads is counts and a date, never a key.
import type { LiveClient } from './client.js';
import type { SetupState as WireSetup } from './protocol/generated.js';
import type { SetupChoice, SetupPort, SetupState, SetupView } from '../../session/setup.js';
const REFUSED = 'Hatchling couldn’t say what’s set up. sudo hatchling-broker doctor checks everything.';
const UNEXPECTED = 'Hatchling answered with something else.';
function storageKey(choice: SetupChoice): string {
    return `hatchling.setup.${choice}`;
}
/** The broker's word, in the app's. */
export function setupOf(setup: WireSetup): SetupView {
    return {
        tier: setup.vault.tier,
        keys: setup.vault.keys,
        codes: setup.vault.codes,
        codesUnconfirmed: setup.vault.codes_unconfirmed ?? false,
        lastBackup: setup.last_backup ?? null,
    };
}
export class LiveSetup implements SetupPort {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    #state: SetupState = { kind: 'unread' };
    readonly #chosen = new Set<SetupChoice>();
    #ready = false;
    constructor(client: LiveClient, changed: () => void, nextId: () => string) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        client.subscribe(() => this.#watch());
    }
    current(): SetupState {
        return this.#state;
    }
    async refresh(): Promise<void> {
        const reply = await this.#client.request(this.#nextId(), { action: 'read_setup' });
        if (reply.kind !== 'answered') {
            // Nothing learned: what was read still stands, and the connection's banner says why.
            if (this.#state.kind !== 'read')
                this.#state = { kind: 'failed', detail: reply.detail };
        }
        else if (reply.result.result === 'setup')
            this.#state = { kind: 'read', setup: setupOf(reply.result.setup) };
        else
            this.#state = { kind: 'failed', detail: reply.result.result === 'refused' ? REFUSED : UNEXPECTED };
        this.#changed();
    }
    /** Whether `choice` was made, here or in an earlier visit in this browser. */
    remembers(choice: SetupChoice): boolean {
        if (this.#chosen.has(choice))
            return true;
        try {
            return window.localStorage.getItem(storageKey(choice)) === 'yes';
        }
        catch {
            return false;
        }
    }
    /** Holds `choice` for this page, and for later visits if this browser keeps anything. */
    remember(choice: SetupChoice): void {
        this.#chosen.add(choice);
        try {
            window.localStorage.setItem(storageKey(choice), 'yes');
        }
        catch {
            this.#changed();
        }
    }
    /** Read it each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh();
        this.#ready = ready;
    }
}
