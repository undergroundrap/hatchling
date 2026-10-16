// The live browser: sessions, page snapshots, the action log and
// who is driving, mapped onto the existing browser and activity views.
//
// Control is never ambiguous, and the UI never moves it. A session starts
// with the agent driving; control then moves only by the handoffs the broker
// reports, and only as the shared table allows (control.ts). A handoff the
// table does not allow is a broken protocol, not a state: the connection ends.
// While control changes hands nobody drives, so no action from either side is
// shown as done, and the view's controls are disabled.
//
// The agent never resumes on a stale page. Handing back records the newest
// frame the user saw; a resume counts only with a newer frame, one the agent
// captured after the handback. Until then the session stays switching.
import type { ActivityRow, ActivityThread, BrowserTask, BrowserTaskState, Message } from '../../protocol/models.js';
import type { Actor, BrowserAction, BrowserFrame, BrowserSession, BrowserState, Control, Handoff } from './protocol/generated.js';
import { imageUrl } from './artifacts.js';
import { appChat } from './chat.js';
import { controlAfter, drives, switching } from './control.js';
import { clockTime, count, quoted } from './text.js';
const STATES: Readonly<Record<BrowserState, BrowserTaskState>> = { queued: 'queued', running: 'running', parked: 'parked', completed: 'completed', failed: 'failed', cancelled: 'cancelled' };
const ENDED: ReadonlySet<BrowserState> = new Set(['completed', 'failed', 'cancelled']);
const STEPS: Readonly<Record<Control, string | null>> = {
    agent: null,
    user: null,
    to_user: 'Handing control to you. Nothing runs until it settles.',
    to_agent: 'Handing control back to hatchling, which looks at the page again before it acts. Nothing runs until it settles.',
};
const DRIVERS: Readonly<Record<Control, string>> = { agent: 'Hatchling is driving', user: 'You are driving', to_user: 'Switching control', to_agent: 'Switching control' };
interface Logged {
    readonly by: Actor;
    readonly narration: string;
    readonly at: string;
}
interface Desk {
    session: BrowserSession;
    control: Control;
    frame: BrowserFrame | null;
    /** The newest frame the user had seen when they handed back. */
    handedBackAt: number | null;
    readonly log: Logged[];
    ignored: number;
    readonly firstSeen: string;
}
export class BrowserDesk {
    readonly #desks = new Map<string, Desk>();
    session(session: BrowserSession, at: string): void {
        const desk = this.#desks.get(session.session_id);
        if (desk === undefined)
            this.#desks.set(session.session_id, { session, control: 'agent', frame: null, handedBackAt: null, log: [], ignored: 0, firstSeen: at });
        else
            desk.session = session;
    }
    /** A newer page snapshot replaces the last; an older or repeated one is ignored. */
    frame(frame: BrowserFrame): void {
        const desk = this.#desks.get(frame.session_id);
        if (desk !== undefined && (desk.frame === null || frame.sequence > desk.frame.sequence))
            desk.frame = frame;
    }
    /** An action is logged only if whoever took it was driving; anything else is counted and not shown. */
    action(action: BrowserAction, at: string): void {
        const desk = this.#desks.get(action.session_id);
        if (desk === undefined)
            return;
        if (drives(desk.control, action.by))
            desk.log.push({ by: action.by, narration: action.narration, at });
        else
            desk.ignored++;
    }
    /**
     * Apply a handoff the broker reported. Returns a sentence, having changed
     * nothing, when the table does not allow it. A resume without a frame newer
     * than the handback is held: the session stays switching.
     */
    handoff(sessionId: string, handoff: Handoff, frameSequence: number): string | undefined {
        const desk = this.#desks.get(sessionId);
        if (desk === undefined)
            return undefined;
        const next = controlAfter(desk.control, handoff);
        if (next === undefined)
            return `Hatchling reported a browser handoff (${handoff}) that is not allowed while ${DRIVERS[desk.control].toLowerCase()}, so the app disconnected rather than guess who is driving.`;
        if (handoff === 'resumed' && desk.handedBackAt !== null && frameSequence <= desk.handedBackAt)
            return undefined;
        if (handoff === 'hand_back')
            desk.handedBackAt = Math.max(frameSequence, desk.frame?.sequence ?? 0);
        if (next === 'agent')
            desk.handedBackAt = null;
        desk.control = next;
        return undefined;
    }
    /** Whether a session exists and has not ended. */
    live(sessionId: string): boolean {
        const desk = this.#desks.get(sessionId);
        return desk !== undefined && !ENDED.has(desk.session.state);
    }
    /** Who drives a session, if it exists. */
    control(sessionId: string): Control | undefined {
        return this.#desks.get(sessionId)?.control;
    }
    /** The newest frame of a session the UI has, or 0. */
    lastFrame(sessionId: string): number {
        return this.#desks.get(sessionId)?.frame?.sequence ?? 0;
    }
    /** Fresh browser task records. */
    tasks(): BrowserTask[] {
        return [...this.#desks.values()].map(desk => {
            const { session } = desk;
            const step = STEPS[desk.control] ?? session.step;
            const task: BrowserTask = {
                id: session.session_id,
                title: session.title,
                site: session.site,
                step,
                statusTitle: switching(desk.control) ? 'Switching control' : desk.control === 'user' ? 'You have control' : 'Working',
                state: STATES[session.state],
                controller: switching(desk.control) ? 'switching' : desk.control === 'user' ? 'user' : 'agent',
                attemptsRemaining: 0,
                constraints: [],
                granted: [],
                credentialReference: null,
                challenge: 'none',
                continuedFrom: null,
                blocker: '',
                lastConfirmedState: session.last_confirmed,
                ...(typeof session.chat_id === 'string' ? { chatId: appChat(session.chat_id) } : {}),
            };
            const frame = desk.frame;
            return frame === null ? task : { ...task, frame: { sequence: frame.sequence, page: 'home', title: frame.title, query: '', capturedAt: frame.captured_at, image: frame.image ? imageUrl(frame.image) : null } };
        });
    }
    /** The browser card in its chat, for each session a chat asked for. */
    messages(): Message[] {
        return [...this.#desks.values()].flatMap(desk => typeof desk.session.chat_id === 'string' ? [{ id: `browser:${desk.session.session_id}`, chatId: appChat(desk.session.chat_id), time: clockTime(desk.firstSeen), source: null, reaction: null, kind: 'browser', role: 'agent', taskId: desk.session.session_id } satisfies Message] : []);
    }
    /** Each session's action log, as an activity thread. */
    threads(): ActivityThread[] {
        return [...this.#desks.values()].map(desk => {
            const id = desk.session.session_id;
            const rows: ActivityRow[] = desk.log.map((entry, index) => ({ id: `${id}:action-${index + 1}`, workId: `${id}:action-${index + 1}`, laneId: id, title: entry.narration, subtitle: entry.by === 'user' ? 'Done by you' : 'Done by hatchling', icon: 'globe', report: quoted(entry.narration), startedAt: entry.at, endedAt: entry.at, durationMs: null, outputFailed: false, status: 'success', phase: 'action' }));
            const ended = ENDED.has(desk.session.state);
            const ignored = desk.ignored === 0 ? '' : ` ${count(desk.ignored, 'action', 'actions')} arrived from whoever was not driving and ${desk.ignored === 1 ? 'was' : 'were'} not shown.`;
            const summary = `${count(rows.length, 'action', 'actions')} on the page.${ignored}`;
            return {
                id: `browser:${id}`,
                name: 'Browser session',
                subtitle: `${count(rows.length, 'action', 'actions')} logged · ${DRIVERS[desk.control].toLowerCase()}`,
                statusTitle: ended ? 'Session ended' : DRIVERS[desk.control],
                state: desk.session.state === 'failed' ? 'failed' : ended ? 'completed' : 'running',
                verdict: null,
                summary,
                finishMessage: summary,
                time: clockTime(desk.firstSeen),
                startedAt: desk.firstSeen,
                endedAt: null,
                evidenceArtifactId: null,
                lanes: [{ id, name: 'Browser', parentId: null, rows }],
            };
        });
    }
}
