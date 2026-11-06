import type { ProtocolAdapter } from '../../protocol/models.js';
import type { ChatSendCommand, ChatSessionPort, ConnectionState, DeliveryOutcome, HistoryState, PendingPhase, ReliabilityScenario, SavedDraft, SavedSend } from '../../session/chat-session.js';
export interface OutboxItem {
    readonly id: string;
    readonly batchId: string;
    readonly command: ChatSendCommand;
    phase: PendingPhase | 'accepted';
    detail: string;
    messageId: string | null;
    checking: boolean;
    generation: number;
}
const readyHistory: HistoryState = { phase: 'ready', hasCache: true, detail: '' };
const unknownDetail = 'Delivery is not confirmed. It may have arrived, so it will not be sent again automatically.';
/** View-owned outbox. Only adapter receipts can establish acceptance or safe rejection.
 * Serial per chat, independent across chats; never retry a dispatched unknown automatically. */
export class ChatDelivery {
    readonly port: ChatSessionPort | undefined;
    private readonly items: OutboxItem[] = [];
    private readonly drafts = new Map<string, SavedDraft>();
    private readonly active = new Set<string>();
    private readonly watchdogs = new Map<string, ReturnType<typeof setTimeout>>();
    private readonly unlisten: (() => void) | undefined;
    private disposed = false;
    private browserOffline = false;
    private serial = 0;
    private readonly prefix = `outbox-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
    private pausedAfterRestore = false;
    private reconnecting = false;
    private connectionError = '';
    private readonly historyRequests = new Set<string>();
    private readonly historyDeadlines = new Map<string, ReturnType<typeof setTimeout>>();
    private readonly historyErrors = new Map<string, string>();
    private persisted = false;
    constructor(private readonly adapter: ProtocolAdapter & {
        readonly chatSession?: ChatSessionPort;
    }, private readonly changed: () => void, private readonly timeoutMs = 12000) {
        this.port = adapter.chatSession;
        let recovery;
        try {
            recovery = this.port?.loadRecovery?.();
        }
        catch { /* Optional storage must not block startup. */ }
        if (recovery) {
            for (const draft of recovery.drafts)
                this.drafts.set(draft.chatId, draft);
            for (const saved of recovery.outbox)
                this.items.push({ ...saved, phase: saved.phase === 'sending' ? 'unconfirmed' : saved.phase, detail: saved.phase === 'sending' ? unknownDetail : saved.detail, messageId: null, checking: false, generation: 0 });
            this.pausedAfterRestore = this.items.some(item => item.phase === 'queued');
            this.persisted = true;
        }
        this.unlisten = this.port?.subscribe(() => {
            if (this.port?.getSnapshot().connection.kind === 'ready')
                this.connectionError = '';
            for (const [chatId, history] of Object.entries(this.port?.getSnapshot().histories ?? {}))
                if (history.phase === 'ready')
                    this.historyErrors.delete(chatId);
            this.observeHistory();
            this.publish();
            this.pump();
        });
        this.observeHistory();
    }
    get connection(): ConnectionState {
        if (this.browserOffline)
            return { kind: 'offline', detail: 'Your browser is offline. Queued messages will wait here until the connection returns.' };
        if (this.reconnecting)
            return { kind: 'reconnecting', detail: 'Checking the connection. Your draft stays here.' };
        if (this.connectionError)
            return { kind: 'unavailable', detail: this.connectionError };
        return this.port?.getSnapshot().connection ?? { kind: 'ready', detail: '' };
    }
    history(chatId: string): HistoryState {
        const history = this.port?.getSnapshot().histories[chatId] ?? readyHistory;
        if (this.historyRequests.has(chatId))
            return { ...history, phase: 'loading' };
        const error = this.historyErrors.get(chatId);
        return error ? { ...history, phase: 'error', detail: error } : history;
    }
    get restoredQueue(): boolean { return this.pausedAfterRestore; }
    get recoveryAvailable(): boolean { return this.persisted; }
    get hasUnsent(): boolean { return this.items.some(item => item.phase !== 'accepted'); }
    get hasDrafts(): boolean { return [...this.drafts.values()].some(draft => draft.text.trim() || draft.attachments.length); }
    savedDrafts(): readonly SavedDraft[] { return [...this.drafts.values()]; }
    saveDraft(chatId: string, text: string, attachments: readonly string[]): void {
        const previous = this.drafts.get(chatId);
        if (previous?.text === text && previous.attachments.length === attachments.length && previous.attachments.every((id, index) => id === attachments[index]))
            return;
        if (!text && attachments.length === 0)
            this.drafts.delete(chatId);
        else
            this.drafts.set(chatId, { chatId, text, attachments: [...attachments] });
        this.persist();
    }
    entries(chatId: string): readonly Readonly<OutboxItem>[] { return this.items.filter(item => item.command.chatId === chatId); }
    visibleEntries(chatId: string): readonly Readonly<OutboxItem>[] {
        const messages = this.adapter.getSnapshot().messages;
        return this.entries(chatId).filter(item => item.phase !== 'accepted' || !item.messageId || !messages.some(message => message.id === item.messageId));
    }
    receipt(messageId: string): boolean { return this.items.some(item => item.phase === 'accepted' && item.messageId === messageId); }
    count(chatId: string): number { return this.entries(chatId).filter(item => item.phase !== 'accepted').length; }
    attention(chatId: string): boolean { return this.entries(chatId).some(item => ['failed', 'unconfirmed'].includes(item.phase)); }
    blockedReason(chatId: string): string {
        const chat = this.adapter.getSnapshot().chats.find(item => item.id === chatId);
        if (!chat)
            return 'This conversation is no longer available.';
        if (chat.channelId)
            return 'Reply in the connected messaging app.';
        if (chat.archived)
            return 'Restore this chat before sending a message.';
        if (this.connection.kind === 'locked')
            return 'Unlock Hatchling to send. You can keep writing your draft.';
        if (this.connection.kind === 'sign-in-required')
            return 'Reconnect your session to send. You can keep writing your draft.';
        if (this.history(chatId).phase !== 'ready')
            return 'Your draft is kept. Load the conversation before sending.';
        if (this.items.filter(item => item.phase !== 'accepted').length >= 30)
            return 'Your outbox is full. Resolve or remove an unsent message first.';
        return '';
    }
    enqueue(chatId: string, text: string, attachments: readonly string[], avatarAccent?: string): boolean {
        const clean = text.trim();
        if ((!clean && attachments.length === 0) || this.blockedReason(chatId))
            return false;
        const commands: ChatSendCommand[] = [];
        if (clean)
            commands.push({ type: 'chat.send', chatId, text: clean });
        for (const artifactId of [...new Set(attachments)])
            commands.push({ type: 'chat.attach', chatId, artifactId, ...(avatarAccent ? { avatarAccent } : {}) });
        if (this.items.filter(item => item.phase !== 'accepted').length + commands.length > 30)
            return false;
        const batchId = `${this.prefix}-${++this.serial}`;
        for (const [index, command] of commands.entries())
            this.items.push({ id: `${batchId}-${index}`, batchId, command, phase: 'queued', detail: '', messageId: null, checking: false, generation: 0 });
        this.publish();
        queueMicrotask(() => this.pump());
        return true;
    }
    resumeRecovered(): void { this.pausedAfterRestore = false; this.publish(); this.pump(); }
    setBrowserOffline(value: boolean): void {
        this.browserOffline = value;
        this.publish();
        if (!value)
            this.pump();
    }
    sync(): void { queueMicrotask(() => this.pump()); }
    retry(id: string): void {
        const item = this.items.find(entry => entry.id === id);
        if (!item || item.phase !== 'failed')
            return;
        item.phase = 'queued';
        item.detail = '';
        item.generation++;
        this.publish();
        this.pump();
    }
    /** Removal/editing is safe only before dispatch or after an explicit rejection. */
    remove(id: string): Readonly<OutboxItem> | null {
        const index = this.items.findIndex(item => item.id === id);
        const item = this.items[index];
        if (!item || !['queued', 'failed'].includes(item.phase))
            return null;
        this.items.splice(index, 1);
        this.publish();
        this.pump();
        return item;
    }
    /** Editing a failed/queued bubble also holds its remaining files, never accepted parts. */
    edit(id: string): readonly Readonly<OutboxItem>[] {
        const item = this.items.find(entry => entry.id === id);
        if (!item || !['queued', 'failed'].includes(item.phase))
            return [];
        const editable = this.items.filter(entry => entry.batchId === item.batchId && ['queued', 'failed'].includes(entry.phase));
        for (const entry of editable)
            this.items.splice(this.items.indexOf(entry), 1);
        this.publish();
        this.pump();
        return editable;
    }
    /** An explicit local dismissal is not cancellation, rejection, or evidence of delivery. */
    dismissUnconfirmed(id: string): boolean {
        const index = this.items.findIndex(entry => entry.id === id);
        const item = this.items[index];
        if (!item || item.phase !== 'unconfirmed' || item.checking)
            return false;
        this.items.splice(index, 1);
        this.publish();
        this.pump();
        return true;
    }
    async check(id: string): Promise<void> {
        const item = this.items.find(entry => entry.id === id);
        if (!item || item.phase !== 'unconfirmed' || item.checking || !this.port)
            return;
        item.checking = true;
        this.publish();
        const generation = item.generation;
        let timer: ReturnType<typeof setTimeout> | undefined;
        try {
            const outcome = await Promise.race([
                this.port.checkDelivery(id),
                new Promise<DeliveryOutcome>(resolve => { timer = setTimeout(() => resolve({ state: 'unknown', detail: 'Status is still unavailable. Nothing has been resent.' }), this.timeoutMs); }),
            ]);
            if (!this.disposed && item.generation === generation)
                this.settle(item, outcome);
        }
        catch {
            if (!this.disposed && !['accepted'].includes(item.phase))
                item.detail = 'Status could not be checked. Nothing has been resent.';
        }
        finally {
            clearTimeout(timer);
            item.checking = false;
            if (!this.disposed) {
                this.publish();
                this.pump();
            }
        }
    }
    async retryHistory(chatId: string): Promise<void> {
        if (!this.port || this.history(chatId).phase === 'loading' || this.historyRequests.has(chatId))
            return;
        clearTimeout(this.historyDeadlines.get(chatId));
        this.historyDeadlines.delete(chatId);
        this.historyRequests.add(chatId);
        this.historyErrors.delete(chatId);
        this.publish();
        let timer: ReturnType<typeof setTimeout> | undefined;
        try {
            await Promise.race([
                this.port.retryHistory(chatId),
                new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error('History request timed out.')), this.timeoutMs); }),
            ]);
        }
        catch {
            this.historyErrors.set(chatId, 'The conversation could not be loaded. Your draft and visible messages are kept.');
        }
        finally {
            clearTimeout(timer);
            this.historyRequests.delete(chatId);
            if (!this.disposed) {
                this.publish();
                this.pump();
            }
        }
    }
    async reconnect(): Promise<void> {
        if (!this.port || this.reconnecting || this.browserOffline)
            return;
        this.reconnecting = true;
        this.connectionError = '';
        this.publish();
        let timer: ReturnType<typeof setTimeout> | undefined;
        try {
            await Promise.race([
                this.port.reconnect(),
                new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error('Connection check timed out.')), this.timeoutMs); }),
            ]);
        }
        catch {
            this.connectionError = 'The service is still unavailable. Your messages are kept; try reconnecting again.';
        }
        finally {
            clearTimeout(timer);
            this.reconnecting = false;
            if (!this.disposed) {
                this.publish();
                this.pump();
            }
        }
    }
    preview(scenario: ReliabilityScenario, chatId: string): void {
        if (this.adapter.mode !== 'scripted')
            return;
        this.connectionError = '';
        this.historyErrors.clear();
        this.port?.preview?.(scenario, chatId);
    }
    private pump(): void {
        if (this.disposed || this.pausedAfterRestore || this.connection.kind !== 'ready')
            return;
        const chats = new Set(this.items.filter(item => item.phase !== 'accepted').map(item => item.command.chatId));
        for (const chatId of chats) {
            if (this.active.has(chatId) || this.blockedReasonForDispatch(chatId))
                continue;
            const next = this.items.find(item => item.command.chatId === chatId && item.phase !== 'accepted');
            if (next?.phase === 'queued')
                void this.dispatch(next);
        }
    }
    private blockedReasonForDispatch(chatId: string): boolean {
        const chat = this.adapter.getSnapshot().chats.find(item => item.id === chatId);
        return !chat || chat.archived || chat.channelId !== null || this.history(chatId).phase !== 'ready';
    }
    private async dispatch(item: OutboxItem): Promise<void> {
        const chatId = item.command.chatId;
        const generation = ++item.generation;
        this.active.add(chatId);
        item.phase = 'sending';
        item.detail = '';
        this.publish();
        const timer = setTimeout(() => {
            if (!this.disposed && item.phase === 'sending' && item.generation === generation) {
                item.phase = 'unconfirmed';
                item.detail = unknownDetail;
                this.active.delete(chatId);
                this.publish();
            }
        }, this.timeoutMs);
        this.watchdogs.set(item.id, timer);
        try {
            const outcome = this.port ? await this.port.send({ id: item.id, command: item.command }) : await this.legacySend(item.command);
            if (!this.disposed && item.generation === generation)
                this.settle(item, outcome);
        }
        catch {
            if (!this.disposed && item.generation === generation && !['accepted'].includes(item.phase)) {
                item.phase = 'unconfirmed';
                item.detail = unknownDetail;
            }
        }
        finally {
            clearTimeout(timer);
            this.watchdogs.delete(item.id);
            if (item.generation === generation)
                this.active.delete(chatId);
            if (!this.disposed) {
                this.publish();
                this.pump();
            }
        }
    }
    private async legacySend(command: ChatSendCommand): Promise<DeliveryOutcome> {
        const before = new Set(this.adapter.getSnapshot().messages.map(message => message.id));
        const result = await this.adapter.execute(command);
        if (!result.ok)
            return this.adapter.mode === 'scripted' ? { state: 'rejected', detail: result.message } : { state: 'unknown', detail: result.message || unknownDetail };
        // The frozen result has no required receipt ID. Only the local fixture fallback may identify its synchronous row.
        const local = this.adapter.mode === 'scripted' ? this.adapter.getSnapshot().messages.find(message => message.chatId === command.chatId && message.role === 'user' && !before.has(message.id)) : undefined;
        return { state: 'accepted', messageId: result.id ?? local?.id ?? null };
    }
    private settle(item: OutboxItem, outcome: DeliveryOutcome): void {
        if (item.phase === 'accepted')
            return; // A late failure must not overwrite an authoritative acceptance.
        if (outcome.state === 'accepted') {
            item.phase = 'accepted';
            item.messageId = outcome.messageId;
            item.detail = '';
        }
        else if (outcome.state === 'rejected') {
            item.phase = 'failed';
            item.detail = outcome.detail;
        }
        else {
            item.phase = 'unconfirmed';
            item.detail = outcome.detail || unknownDetail;
        }
        if (outcome.state !== 'unknown') {
            this.active.delete(item.command.chatId);
            clearTimeout(this.watchdogs.get(item.id));
        }
    }
    private observeHistory(): void {
        const histories = this.port?.getSnapshot().histories ?? {};
        for (const [chatId, timer] of this.historyDeadlines) {
            if (histories[chatId]?.phase !== 'loading') {
                clearTimeout(timer);
                this.historyDeadlines.delete(chatId);
            }
        }
        for (const [chatId, history] of Object.entries(histories)) {
            if (history.phase !== 'loading' || this.historyDeadlines.has(chatId))
                continue;
            this.historyDeadlines.set(chatId, setTimeout(() => {
                if (!this.disposed && this.port?.getSnapshot().histories[chatId]?.phase === 'loading') {
                    this.historyErrors.set(chatId, 'Loading is taking too long. Try again; your draft is kept.');
                    this.publish();
                }
            }, this.timeoutMs));
        }
    }
    private persist(): void {
        const outbox: SavedSend[] = [];
        for (const item of this.items)
            if (item.phase !== 'accepted')
                outbox.push({ id: item.id, batchId: item.batchId, command: item.command, phase: item.phase, detail: item.detail });
        try {
            this.persisted = this.port?.saveRecovery?.({ version: 1, drafts: [...this.drafts.values()], outbox }) ?? false;
        }
        catch {
            this.persisted = false;
        }
    }
    private publish(): void {
        if (!this.disposed) {
            this.persist();
            this.changed();
        }
    }
    dispose(): void {
        this.disposed = true;
        this.unlisten?.();
        for (const timer of this.watchdogs.values())
            clearTimeout(timer);
        this.watchdogs.clear();
        for (const timer of this.historyDeadlines.values())
            clearTimeout(timer);
        this.historyDeadlines.clear();
    }
}
