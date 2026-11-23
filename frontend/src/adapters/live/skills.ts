// The signed skills the broker found (WO-046), and which the owner enabled.
// Loaded when the connection opens; enabled and disabled only by the owner,
// here. Enabling names the digest the owner was shown, so a skill changed
// since is refused, not enabled. The list is the broker's, never this app's
// guess: until the broker answers it is empty.
import type { CommandResult, Skill } from '../../protocol/models.js';
import type { LiveClient, Reply } from './client.js';
import type { SkillListing, SkillsOp, SkillsResult } from './protocol/generated.js';
const ENABLED = 'Enabled. The agent can use it from its next step, with what it declares and nothing more.';
const DISABLED = 'Disabled. The agent can no longer use it, in any task.';
const CHANGED = 'This skill changed since you were shown it, so nothing was enabled. Review it again.';
const GONE = 'Hatchling no longer finds this skill.';
export const SKILLS_OFF = 'Hatchling has no skills: the operator has not given it any.';
const FAILED = "Hatchling couldn't change its skills, so nothing changed. Its log says why.";
const REFUSED = 'This connection may not change the skills, so nothing changed.';
const UNCONFIRMED = 'Hatchling answered with something else, so this is not confirmed. Check the list again.';
/** A listing as the app shows it. */
function shown(listing: SkillListing): Skill {
    return {
        name: listing.name,
        version: listing.version,
        description: listing.description,
        signer: listing.signer,
        digest: listing.digest,
        capabilities: [...listing.capabilities],
        instructions: listing.instructions ?? null,
        program: listing.program ?? null,
        files: listing.files.map(file => ({ path: file.path, bytes: file.bytes })),
        state: listing.state,
    };
}
export class LiveSkills {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    #skills: readonly Skill[] = [];
    #ready = false;
    constructor(client: LiveClient, changed: () => void, nextId: () => string) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        client.subscribe(() => this.#watch());
    }
    /** The skills found, by name. */
    skills(): readonly Skill[] {
        return this.#skills;
    }
    /** Ask the broker for the list: it reads the skills as they are now. */
    async refresh(): Promise<void> {
        const outcome = await this.#ask({ op: 'list' });
        if (outcome?.result !== 'listed')
            return;
        this.#skills = outcome.skills.map(shown);
        this.#changed();
    }
    /** Enable `name` as the owner was shown it, at `digest`. */
    enable(name: string, digest: string): Promise<CommandResult> {
        return this.#change({ op: 'enable', name, digest }, 'enabled', ENABLED);
    }
    /** Disable `name`: it stops at once. */
    disable(name: string): Promise<CommandResult> {
        return this.#change({ op: 'disable', name }, 'disabled', DISABLED);
    }
    async #change(op: SkillsOp, done: 'enabled' | 'disabled', message: string): Promise<CommandResult> {
        const reply = await this.#client.request(this.#nextId(), { action: 'manage_skills', op });
        if (reply.kind !== 'answered')
            return { ok: false, message: reply.detail };
        const result = reply.result;
        if (result.result === 'refused')
            return { ok: false, message: REFUSED };
        if (result.result !== 'skills')
            return { ok: false, message: UNCONFIRMED };
        switch (result.outcome.result) {
            case 'enabled':
            case 'disabled':
                if (result.outcome.result !== done)
                    return { ok: false, message: UNCONFIRMED };
                await this.refresh();
                return { ok: true, message };
            case 'changed':
                await this.refresh();
                return { ok: false, message: CHANGED };
            case 'not_found':
                await this.refresh();
                return { ok: false, message: GONE };
            case 'off':
                return { ok: false, message: SKILLS_OFF };
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
    async #ask(op: SkillsOp): Promise<SkillsResult | undefined> {
        const reply: Reply = await this.#client.request(this.#nextId(), { action: 'manage_skills', op });
        return reply.kind === 'answered' && reply.result.result === 'skills' ? reply.result.outcome : undefined;
    }
}
