import { ScriptedTaskApprovals } from './task-approvals.js';
import type { TaskApproval } from '../session/task-approvals.js';
import { ScriptedChatSession } from './chat-session.js';
import { ScriptedUsage } from './scripted-usage.js';
import { presentationSnapshot } from './presentation-snapshot.js';
import { identityDocumentId, patchIdentityDocument } from '../protocol/identity-document.js';
import { reduceBrowserSession } from './browser-fixtures.js';
import { advanceBundle } from './live-activity-fixtures.js';
import { advanceCi } from './activity-fixtures.js';
import type { AppState, Command, CommandResult, DeepReadonly, ProtocolAdapter } from '../protocol/models.js';
import { parseCommand } from '../protocol/codec.js';
import { refreshSystemIndex } from './surface-fixtures.js';
import { createFixtures } from './fixtures.js';
import { reduceChat } from './reducers/chat.js';
import { reduceContent } from './reducers/content.js';
import { reduceWorkspace, advanceIdeas } from './reducers/workspace.js';
import { reduceTrust, settlePurchases } from './reducers/trust.js';
import { advanceResearch, applyScenario } from './reducers/scenarios.js';
export interface StoragePort {
    getItem(key: string): string | null;
    setItem(key: string, value: string): void;
    removeItem(key: string): void;
}
export interface ScriptedOptions {
    recoveryStorage?: StoragePort | null;
    storage?: StoragePort | null;
    animate?: boolean;
}
// The reference fixture ids have changed: never replay v1 edits onto different conversations.
const STORAGE_KEY = 'hatchling.scripted.events.v10';
/** Only this adapter owns fixtures, persistence and scripted time. A live implementation replaces this one object. */
export class ScriptedAdapter implements ProtocolAdapter {
    readonly mode = 'scripted' as const;
    readonly chatSession: ScriptedChatSession;
    readonly taskApprovals: ScriptedTaskApprovals;
    /** The preview's usage: nothing counted, and a sample of the composer's context (WO-056). */
    readonly usage = new ScriptedUsage();
    private state: AppState = createFixtures();
    private readonly listeners = new Set<() => void>();
    private readonly browserScheduled = new Set<string>();
    private readonly timers = new Set<ReturnType<typeof setTimeout>>();
    private readonly storage: StoragePort | null;
    private readonly animate: boolean;
    private log: Command[] = [];
    constructor(options: ScriptedOptions = {}) {
        this.storage = options.storage ?? null;
        this.animate = options.animate ?? true;
        this.taskApprovals = new ScriptedTaskApprovals(this.publishApprovals, chatId => this.state.chats.some(chat => chat.id === chatId && !chat.archived));
        this.chatSession = new ScriptedChatSession(command => this.execute(command), () => this.getSnapshot(), options.recoveryStorage ?? null);
        try {
            const raw = this.storage?.getItem(STORAGE_KEY);
            const decoded: unknown = raw ? JSON.parse(raw) : [];
            if (Array.isArray(decoded) && decoded.length <= 5000) {
                for (const entry of decoded) {
                    const command = parseCommand(entry);
                    if (!command)
                        continue;
                    const result = this.apply(command);
                    if (result.ok)
                        this.log.push(command);
                }
            }
        }
        catch {
            this.state = createFixtures();
            this.log = [];
        }
        // Reload never resumes a browser worker merely because another UI command arrives.
        for (const task of this.state.browserTasks) {
            if (task.frame && task.state === 'running') {
                task.state = 'parked';
                task.controller = 'user';
                task.blocker = 'The local session was reloaded. Return control to continue.';
                const thread = this.state.threads.find(item => item.id === `watch-${task.id}`);
                if (thread)
                    thread.state = 'interrupted';
            }
        }
        // A reloaded scripted turn has no running timer. Resolve it from its saved state, without external work.
        if (this.state.turn === 'working') {
            this.state.turn = 'interrupted';
            for (const thread of this.state.threads)
                if (thread.state === 'running')
                    thread.state = 'interrupted';
        }
    }
    getSnapshot(): DeepReadonly<AppState> { return presentationSnapshot(this.state); }
    subscribe(listener: () => void): () => void { this.listeners.add(listener); return () => this.listeners.delete(listener); }
    execute(command: Command): Promise<CommandResult> {
        if (command.type === 'message.react') {
            const id = command.id;
            if (!this.state.messages.some(message => message.id === id) && this.getSnapshot().messages.some(message => message.id === id))
                command = { ...command, id: id.replace(/:bubble:\d+$/, '') };
        }
        if (command.type === 'demo.reset') {
            this.cancelTimers();
            this.taskApprovals.reset();
            this.state = createFixtures();
            this.log = [];
            try {
                this.storage?.removeItem(STORAGE_KEY);
            }
            catch { /* A storage denial must not prevent the in-memory demo. */ }
            this.notify();
            return Promise.resolve({ ok: true, message: 'Demo reset to its deterministic sample data.' });
        }
        if (command.type === 'demo.scenario' || command.type === 'turn.interrupt')
            this.cancelTimers();
        const result = this.apply(command);
        if (result.ok) {
            if (!['artifact.download', 'artifact.open', 'data.export', 'support.download'].includes(command.type))
                this.log.push(command);
            try {
                this.storage?.setItem(STORAGE_KEY, JSON.stringify(this.log));
            }
            catch { /* Quota/private mode: state remains usable in memory. */ }
            this.notify();
            if (this.animate) {
                for (const task of this.state.browserTasks) {
                    if (task.frame && task.state === 'running' && task.controller === 'agent' && task.frame.sequence < 3) {
                        this.schedule({ type: 'browser.advance', id: task.id, sequence: task.frame.sequence + 1 }, 2200);
                    }
                }
                if ((command.type === 'demo.scenario' && command.scenario === 'research') || (command.type === 'turn.resume' && this.state.threads.some((thread) => thread.id === 'live-research' && thread.state === 'running'))) {
                    this.schedule({ type: 'demo.advance', step: 1 }, 1600);
                    this.schedule({ type: 'demo.advance', step: 2 }, 4200);
                }
                if ((command.type === 'demo.scenario' && command.scenario === 'fix-ci') || (command.type === 'turn.resume' && this.state.threads.some((thread) => thread.id === 'live-ci' && thread.state === 'running'))) {
                    this.schedule({ type: 'demo.advance', step: 6 }, 2000);
                    this.schedule({ type: 'demo.advance', step: 7 }, 5000);
                }
                if ((command.type === 'demo.scenario' && command.scenario === 'bundle-files') || (command.type === 'turn.resume' && this.state.threads.some(thread => thread.id === 'live-bundle' && thread.state === 'running'))) {
                    this.schedule({ type: 'demo.advance', step: 8 }, 6500);
                    this.schedule({ type: 'demo.advance', step: 9 }, 13000);
                }
                if (command.type === 'purchase.submit')
                    this.schedule({ type: 'demo.advance', step: 3 }, 1100);
                if (command.type === 'idea.activate') {
                    this.schedule({ type: 'demo.advance', step: 4 }, 650);
                    this.schedule({ type: 'demo.advance', step: 5 }, 1800);
                }
            }
        }
        return Promise.resolve(result);
    }
    private apply(command: Command): CommandResult {
        const draft = structuredClone(this.state);
        let result: CommandResult | null = null;
        if (command.type === 'demo.scenario') {
            applyScenario(draft, command.scenario);
            result = { ok: true, message: 'Scripted scenario loaded.' };
        }
        else if (command.type === 'demo.advance') {
            if (command.step === 8 || command.step === 9)
                advanceBundle(draft, command.step);
            if (command.step === 6 || command.step === 7)
                advanceCi(draft, command.step);
            if (command.step <= 2)
                advanceResearch(draft, command.step);
            if (command.step === 3)
                settlePurchases(draft);
            if (command.step === 4 || command.step === 5)
                advanceIdeas(draft, command.step === 5);
            result = { ok: true, message: '' };
        }
        else {
            for (const reducer of [reduceBrowserSession, reduceChat, reduceContent, reduceWorkspace, reduceTrust]) {
                result = reducer(draft, command);
                if (result !== null)
                    break;
            }
        }
        if (result === null)
            return { ok: false, message: 'That action is not part of the scripted contract.' };
        if (result.ok) {
            if (draft.settings.name !== this.state.settings.name || draft.settings.vibe !== this.state.settings.vibe) {
                const identity = draft.memory.find(note => note.id === identityDocumentId);
                if (identity)
                    identity.body = patchIdentityDocument(identity.body, draft.settings.name, draft.settings.vibe);
            }
            const oldIds = new Set(this.state.messages.map((message) => message.id));
            for (const message of draft.messages) {
                if (!oldIds.has(message.id) && message.role === 'agent' && !['progress', 'notice'].includes(message.kind)) {
                    const chat = draft.chats.find((item) => item.id === message.chatId);
                    if (chat) {
                        chat.unread = true;
                        chat.updatedAt = draft.clock;
                    }
                }
            }
            refreshSystemIndex(draft);
            draft.revision++;
            this.state = draft;
        }
        return result;
    }
    private publishApprovals = (requests: readonly TaskApproval[]): void => {
        const draft = structuredClone(this.state);
        draft.threads = draft.threads.filter(thread => !thread.id.startsWith('approval-work-'));
        const threadIds = [...new Set(requests.map(request => request.threadId))];
        for (const id of threadIds.toReversed()) {
            const group = requests.filter(request => request.threadId === id);
            const first = group[0];
            if (!first)
                continue;
            const pending = group.filter(request => request.status === 'pending');
            const declined = group.some(request => request.status === 'denied' || request.status === 'expired' || request.status === 'cancelled');
            const summary = pending.length ? 'Waiting for your decision. No requested action has run.' : declined ? 'The request was declined, cancelled, or expired. No requested action ran.' : 'Your permission decision was saved. This local preview does not install software or contact any service.';
            draft.threads.unshift({ id, chatId: first.chatId, name: 'Install project tools', subtitle: pending.length ? `${pending.length} approval${pending.length === 1 ? '' : 's'} needed · ${pending[0]?.title ?? ''}` : summary, statusTitle: pending.length ? 'Waiting for approval' : 'Decision saved', state: pending.length || declined ? 'interrupted' : 'completed', verdict: null, finishMessage: summary, summary, time: 'just now', evidenceArtifactId: null, lanes: [{ id: 'main', name: 'hatchling', parentId: null, rows: group.filter(request => request.status !== 'pending').map(request => ({ id: request.id, workId: id, laneId: 'main', title: `Permission record: ${request.title}`, subtitle: request.status === 'allowed-once' ? 'Allowed once' : request.status === 'allowed-task' ? 'Allowed for this task' : request.status === 'denied' ? 'Declined' : request.status === 'expired' ? 'Timed out' : 'Cancelled', icon: 'globe', report: request.report, outputFailed: false, status: 'success', phase: 'action' })) }], pendingSteps: pending.map(request => ({ id: request.id, workId: id, laneId: 'main', title: request.title, subtitle: 'Pending approval', report: `${request.description}\n\n${request.detail}\n\nThe task is paused until you decide. No requested action has run.`, startedAt: null })) });
        }
        draft.revision++;
        this.state = draft;
        this.notify();
    };
    private schedule(command: Command, delay: number): void {
        const key = command.type === 'browser.advance' ? `${command.id}:${command.sequence}` : '';
        if (key && this.browserScheduled.has(key))
            return;
        if (key)
            this.browserScheduled.add(key);
        const timer = setTimeout(() => {
            this.timers.delete(timer);
            if (key)
                this.browserScheduled.delete(key);
            void this.execute(command);
        }, delay);
        this.timers.add(timer);
    }
    private notify(): void {
        for (const listener of this.listeners)
            listener();
    }
    private cancelTimers(): void {
        for (const timer of this.timers)
            clearTimeout(timer);
        this.timers.clear();
        this.browserScheduled.clear();
    }
    dispose(): void { this.taskApprovals.dispose(); this.chatSession.dispose(); this.cancelTimers(); this.listeners.clear(); }
}
export function createBrowserAdapter(): ScriptedAdapter {
    let storage: StoragePort | null = null;
    try {
        storage = window.localStorage;
    }
    catch { /* Storage is optional, not a startup dependency. */ }
    let recoveryStorage: StoragePort | null = null;
    try {
        recoveryStorage = window.sessionStorage;
    }
    catch { /* Tab recovery is optional; the UI warns when storage is unavailable. */ }
    return new ScriptedAdapter({ storage, recoveryStorage });
}
