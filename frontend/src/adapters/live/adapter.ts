// The live ProtocolAdapter: the existing chat, activity,
// approval, artifact and browser views, and the agent's status, fed by the
// broker through the frontend protocol.
//
// The snapshot holds only what the broker has said. Every collection the
// protocol does not carry yet (goals, schedules, devices, ...) is empty,
// never seeded with sample data, and the commands that would act on them are
// refused with a sentence instead of simulated. The agent's status is the one
// it last reported, never inferred. Display preferences (theme, accent, size,
// panel) stay in the app.
import type { AppState, Command, CommandResult, DeepReadonly, ProtocolAdapter, Settings } from '../../protocol/models.js';
import { ActivityBook } from './activity.js';
import { LiveApprovals } from './approvals.js';
import { ChatBook } from './chat.js';
import { LiveClient } from './client.js';
import type { UiTransport } from './client.js';
import type { UiEvent } from './protocol/generated.js';
import { LiveChatSession } from './session.js';
import { LiveSites } from './sites.js';
import { Surfaces } from './surfaces.js';
const SETTINGS: Settings = { interfaceSize: 'comfortable', theme: 'dark', accent: 'white', avatarAccent: '#a78c57', keepChatPanelVisible: true, language: 'English', name: 'hatchling', vibe: '', feedPrompt: '', captchaStanding: false, telemetry: false, quietStart: '22:00', quietEnd: '08:00', digestTime: '08:30', dailyAllowance: 0, allowlist: [] };
const NOT_YET = 'Hatchling cannot do this from the app yet.';
export class LiveAdapter implements ProtocolAdapter {
    readonly mode = 'live';
    readonly chatSession: LiveChatSession;
    readonly taskApprovals: LiveApprovals;
    readonly #client: LiveClient;
    readonly #activity = new ActivityBook();
    readonly #chats = new ChatBook();
    readonly #surfaces: Surfaces;
    readonly #sites: LiveSites;
    readonly #listeners = new Set<() => void>();
    #settings: Settings = SETTINGS;
    #clock = '';
    #revision = 0;
    #serial = 0;
    #snapshot: AppState;
    /**
     * Connects at once. `name` is how this app introduces itself in the broker's
     * log; `token` is the UI token the page was given, when there is one.
     */
    constructor(transport: UiTransport, name = 'hatchling-frontend', token?: string) {
        this.#client = new LiveClient(transport, name, event => this.#observe(event), token);
        this.#surfaces = new Surfaces(this.#client, () => this.#publish(), () => `live-command-${++this.#serial}`);
        this.taskApprovals = new LiveApprovals(this.#client);
        this.#sites = new LiveSites(this.#client, () => this.#publish(), () => `live-command-${++this.#serial}`);
        this.chatSession = new LiveChatSession(this.#client, this.#chats, artifactId => this.#surfaces.shelf.latest(artifactId));
        this.#snapshot = this.#build();
        // Registered first, so the snapshot is current before any view hears of a change.
        this.#client.subscribe(() => this.#publish());
        this.#client.connect();
    }
    getSnapshot(): DeepReadonly<AppState> {
        return this.#snapshot;
    }
    subscribe(listener: () => void): () => void {
        this.#listeners.add(listener);
        return () => { this.#listeners.delete(listener); };
    }
    async execute(command: Command): Promise<CommandResult> {
        switch (command.type) {
            case 'chat.send': {
                const outcome = await this.chatSession.send({ id: `live-command-${++this.#serial}`, command });
                if (outcome.state !== 'accepted')
                    return { ok: false, message: outcome.detail };
                return outcome.messageId === null ? { ok: true, message: 'Sent.' } : { ok: true, message: 'Sent.', id: outcome.messageId };
            }
            case 'chat.attach': {
                const outcome = await this.chatSession.send({ id: `live-command-${++this.#serial}`, command });
                return outcome.state === 'accepted' ? { ok: true, message: 'Sent.' } : { ok: false, message: outcome.detail };
            }
            case 'chat.read':
                return { ok: true, message: 'Read.' };
            case 'artifact.open':
                return this.#surfaces.open(command.id);
            case 'artifact.download':
                return this.#surfaces.download(command.id);
            case 'artifact.publish':
                return this.#surfaces.share(command.id, command.approvedVersion);
            case 'artifact.import':
                return this.#surfaces.save(command.title, command.format, command.content, command.dataUrl);
            case 'browser.takeover':
                return this.#surfaces.take(command.id);
            case 'browser.resume':
                return this.#surfaces.giveBack(command.id);
            case 'browser.cancel':
                return this.#surfaces.stop(command.id);
            case 'settings.theme':
                return this.#prefer({ theme: command.value });
            case 'settings.accent':
                return this.#prefer({ accent: command.value });
            case 'settings.interface-size':
                return this.#prefer({ interfaceSize: command.value });
            case 'settings.chat-panel':
                return this.#prefer({ keepChatPanelVisible: command.value });
            case 'permission.website':
                // The broker adds a site only from the owner's Allow always on its request.
                return command.policy === 'allow'
                    ? { ok: false, message: "Allow a site from the agent's request for it, with Allow always." }
                    : this.#sites.revoke(command.host);
            case 'demo.scenario':
            case 'demo.advance':
            case 'demo.reset':
                return { ok: false, message: 'Sample scenarios are not available when the app is connected to Hatchling.' };
            default:
                return { ok: false, message: NOT_YET };
        }
    }
    dispose(): void {
        this.#client.dispose();
        this.#listeners.clear();
    }
    #prefer(change: Partial<Pick<Settings, 'theme' | 'accent' | 'interfaceSize' | 'keepChatPanelVisible'>>): CommandResult {
        this.#settings = { ...this.#settings, ...change };
        this.#publish();
        return { ok: true, message: 'Saved.' };
    }
    /** Apply one event. Returns a sentence, having changed nothing, when it breaks the protocol's rules. */
    #observe(event: UiEvent): string | undefined {
        const body = event.body;
        const broken = this.#surfaces.observe(body, event.ts);
        if (broken !== undefined)
            return broken;
        this.#clock = event.ts;
        switch (body.kind) {
            case 'activity':
                this.#activity.apply(body.event);
                return undefined;
            case 'chat_message':
                this.#chats.apply(body, event.ts);
                return undefined;
            case 'attachment':
                this.#chats.attach(body, event.ts);
                return undefined;
            case 'confirmation':
            case 'confirmation_decided':
                this.taskApprovals.observe(event);
                this.#sites.observe(event);
                return undefined;
            case 'artifact':
            case 'schedule':
            case 'agent_status':
            case 'artifact_listed':
            case 'browser_session':
            case 'browser_frame':
            case 'browser_action':
            case 'browser_control':
                // Build and schedule notices have no view yet; the surfaces took the rest.
                return undefined;
        }
    }
    /** A new snapshot, then the listeners: a view never reads a stale one. */
    #publish(): void {
        this.#revision++;
        this.#snapshot = this.#build();
        for (const listener of [...this.#listeners])
            listener();
    }
    #build(): AppState {
        const threads = [...this.#activity.threads(), ...this.#surfaces.threads()];
        return {
            websitePermissions: this.#sites.sites().map(site => ({ host: site.site, policy: 'allow' as const })),
            networkPermissions: [],
            version: 1,
            mode: 'live',
            revision: this.#revision,
            turn: this.#surfaces.turn(),
            clock: this.#clock,
            chats: this.#chats.chats(),
            messages: [...this.#chats.messages(), ...this.#surfaces.messages()],
            threads,
            artifacts: this.#surfaces.artifacts(),
            publications: [],
            systemEntries: [],
            goals: [],
            ideas: [],
            posts: [],
            connectors: [],
            channels: [],
            providers: [],
            purchases: [],
            browserTasks: this.#surfaces.browserTasks(),
            devices: [],
            permissions: [],
            approvalHistory: [],
            memory: [],
            jobs: [],
            notifications: [],
            settings: { ...this.#settings, allowlist: [] },
            spendingLimit: 0,
            spentToday: 0,
            secureItems: [],
        };
    }
}
