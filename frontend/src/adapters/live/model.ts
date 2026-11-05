// The model Hatchling uses, read when the connection opens and whenever the owner
// refreshes it, and chosen by the owner: answered with the model as it then
// stands. Only the broker says what the server has; this app never guesses.
import type { LiveClient } from './client.js';
import type { ActionRefusal, ModelState as WireModel } from './protocol/generated.js';
import type { CommandResult } from '../../protocol/models.js';
import type { ModelPort, ModelState, ModelView } from '../../session/model.js';
import { whereShort } from '../../session/model.js';
const REFUSED = 'This connection may not read the model.';
const UNEXPECTED = 'Hatchling answered with something else.';
/** Why a choice wasn't kept, by the broker's refusal. */
const NOT_SET: Readonly<Record<ActionRefusal, string>> = {
    bad_request: 'Hatchling didn’t take that model: its server has none by that name.',
    unavailable: 'Hatchling couldn’t keep your choice. It keeps one only when scheduling is on; its log says if a write failed.',
    not_granted: 'This connection may not choose the model.',
    not_connected: 'Not connected.',
};
/** Why a switch didn't happen, by the broker's refusal. */
const NOT_SWITCHED: Readonly<Record<ActionRefusal, string>> = {
    bad_request: 'There’s no other model set up to switch to: sudo hatchling-broker use-hosted, or use-local, sets one up.',
    unavailable: 'Hatchling couldn’t switch: its settings don’t let it reach that server, or scheduling is off. sudo hatchling-broker doctor says which.',
    not_granted: 'This connection may not switch the model.',
    not_connected: 'Not connected.',
};
/** The broker's word, in the app's. */
export function modelOf(model: WireModel): ModelView {
    return {
        endpoint: model.endpoint ?? null,
        configured: model.configured ?? null,
        chosen: model.chosen ?? null,
        available: model.available ?? null,
        window: model.window ?? null,
        size: model.size ?? null,
        onCpu: model.on_cpu ?? null,
        other: model.other ? { endpoint: model.other.endpoint ?? null, name: model.other.name ?? null } : null,
        switched: model.switched ?? false,
    };
}
export class LiveModel implements ModelPort {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    #state: ModelState = { kind: 'unread' };
    #ready = false;
    constructor(client: LiveClient, changed: () => void, nextId: () => string) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        client.subscribe(() => this.#watch());
    }
    current(): ModelState {
        return this.#state;
    }
    async refresh(): Promise<void> {
        const reply = await this.#client.request(this.#nextId(), { action: 'read_model' });
        if (reply.kind !== 'answered')
            this.#state = { kind: 'failed', detail: reply.detail };
        else if (reply.result.result === 'model')
            this.#state = { kind: 'read', model: modelOf(reply.result.model) };
        else
            this.#state = { kind: 'failed', detail: reply.result.result === 'refused' ? REFUSED : UNEXPECTED };
        this.#changed();
    }
    async choose(name: string | null): Promise<CommandResult> {
        const reply = await this.#client.request(this.#nextId(), { action: 'set_model', ...(name === null ? {} : { name }) });
        if (reply.kind !== 'answered')
            return { ok: false, message: reply.detail };
        if (reply.result.result === 'refused')
            return { ok: false, message: NOT_SET[reply.result.reason] };
        if (reply.result.result !== 'model')
            return { ok: false, message: UNEXPECTED };
        const model = modelOf(reply.result.model);
        this.#state = { kind: 'read', model };
        this.#changed();
        return { ok: true, message: name === null ? `Back to ${model.configured ?? 'the installed model'}, from your next message.` : `Hatchling uses ${name} from your next message.` };
    }
    async switchServer(other: boolean): Promise<CommandResult> {
        const reply = await this.#client.request(this.#nextId(), { action: 'switch_model_server', other });
        if (reply.kind !== 'answered')
            return { ok: false, message: reply.detail };
        if (reply.result.result === 'refused')
            return { ok: false, message: NOT_SWITCHED[reply.result.reason] };
        if (reply.result.result !== 'model')
            return { ok: false, message: UNEXPECTED };
        const model = modelOf(reply.result.model);
        this.#state = { kind: 'read', model };
        this.#changed();
        return { ok: true, message: `Hatchling uses ${model.configured ?? 'the other model'}, ${whereShort(model.endpoint)}, from your next message.` };
    }
    /** Read it each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh();
        this.#ready = ready;
    }
}
