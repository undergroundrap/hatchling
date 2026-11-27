// The requests the owner's standing grants covered (WO-036), as the broker
// keeps them, in a log of their own: each with the address the agent asked
// for and the grant that let it through without asking. Loaded when the
// connection opens and as runs go on, one request at a time; the list is
// the broker's, never this app's guess. Shown in Activity as one thread.
import type { ActivityRow, ActivityThread } from '../../protocol/models.js';
import type { LiveClient } from './client.js';
import type { ActivityEvent, CoveredRecord } from './protocol/generated.js';
import { clockTime } from './text.js';
const LANE = 'covered:requests';
/** The run the broker keeps the Local API's own activity under: no grant covers any of it. */
const API_RUN = 'api';
/** A grant's scope, in the owner's words. */
const grantWords = (record: CoveredRecord): string => record.scope === 'day' ? 'your 24-hour grant' : 'your always grant';
/** The site a grant's key names: what follows its kind. */
const siteOf = (key: string): string => key.slice(key.indexOf(':') + 1);
function row(record: CoveredRecord): ActivityRow {
    const how = record.via === 'browser' ? 'Opened in the agent’s browser' : 'Read';
    const url = record.url ?? null;
    const title = url ?? `${record.count} requests to ${siteOf(record.key)} by one task`;
    const said = url === null
        ? `Its task had sent ${record.count} requests under ${grantWords(record)} by then; past the first, they are counted, not each kept.`
        : `${how}, the address as the agent asked for it.`;
    return {
        id: `covered:${record.at}:${record.key}:${record.task}:${record.count}`,
        workId: record.task,
        laneId: LANE,
        title,
        subtitle: `Allowed by ${grantWords(record)} for ${siteOf(record.key)}`,
        icon: 'globe',
        report: `${said} You weren’t asked: ${record.key} covers it. Revoke it in Permissions to be asked again.`,
        startedAt: record.at,
        endedAt: record.at,
        durationMs: null,
        outputFailed: false,
        status: 'success',
        phase: 'action',
    };
}
export class LiveCovered {
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    #covered: readonly CoveredRecord[] = [];
    #ready = false;
    #asking = false;
    #again = false;
    constructor(client: LiveClient, changed: () => void, nextId: () => string) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
        client.subscribe(() => this.#watch());
    }
    /** The requests covered, the newest first. */
    covered(): readonly CoveredRecord[] {
        return this.#covered;
    }
    /**
     * A run went on: a grant may have covered something. Ask again. Not for
     * what this app's own calls to the broker did (#747): each of those is
     * kept as activity too, the list's own among them, so asking again for
     * one would ask forever.
     */
    observe(event: ActivityEvent): void {
        if (event.run_id !== API_RUN)
            void this.refresh();
    }
    /** Ask the broker for the list; one request at a time, and once more if asked meanwhile. */
    async refresh(): Promise<void> {
        if (this.#asking) {
            this.#again = true;
            return;
        }
        this.#asking = true;
        const reply = await this.#client.request(this.#nextId(), { action: 'list_covered' });
        this.#asking = false;
        if (reply.kind === 'answered' && reply.result.result === 'covered') {
            this.#covered = reply.result.covered;
            this.#changed();
        }
        if (this.#again) {
            this.#again = false;
            await this.refresh();
        }
    }
    /** Activity's thread of what the grants covered; none until something was. */
    thread(): ActivityThread | undefined {
        const [newest] = this.#covered;
        if (newest === undefined)
            return undefined;
        const oldest = this.#covered.reduce((_, record) => record, newest);
        const requests = this.#covered.length === 1 ? '1 request' : `${this.#covered.length} requests`;
        const summary = `${requests} your standing grants let through without asking, the newest first.`;
        return {
            id: 'covered',
            name: 'Allowed by your standing grants',
            subtitle: `${requests} · newest ${clockTime(newest.at)}`,
            statusTitle: 'Recorded',
            state: 'completed',
            verdict: null,
            summary,
            finishMessage: summary,
            time: clockTime(newest.at),
            startedAt: oldest.at,
            endedAt: null,
            evidenceArtifactId: null,
            lanes: [{ id: LANE, name: 'Covered without asking', parentId: null, rows: this.#covered.map(row) }],
            pendingSteps: [],
        };
    }
    /** Load the list each time the connection opens. */
    #watch(): void {
        const ready = this.#client.connection().kind === 'ready';
        if (ready && !this.#ready)
            void this.refresh();
        this.#ready = ready;
    }
}
