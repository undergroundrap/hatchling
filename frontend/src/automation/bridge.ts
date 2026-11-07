import type { AppContext } from '../app/context.js';
import type { AgentPort, AgentResult } from './contracts.js';
import { actionCatalog, validateArguments } from './catalog.js';
import { isRecord, parseCommand } from '../protocol/codec.js';
import { containsSensitiveValue } from '../protocol/rules.js';
import { executeView } from './views.js';
import { h } from '../components/dom.js';
import { button } from '../components/ui.js';
import type { DialogRef } from '../components/dialog.js';
declare global {
    interface Window {
        readonly hatchlingFrontend?: AgentPort;
    }
}
interface Controller {
    show(): void;
    dispose(): void;
}
const controllers = new WeakMap<AppContext, Controller>();
export function openAgentControls(ctx: AppContext): void { controllers.get(ctx)?.show(); }
/** Opt-in same-page automation, not authentication or an OS security boundary. No transport listener. */
export function installAgentControl(ctx: AppContext): () => void {
    let enabled = false;
    let requestSequence = 0;
    let pending: {
        id: string;
        ref: DialogRef;
    } | null = null;
    const results = new Map<string, AgentResult>();
    const seen = new Map<string, {
        signature: string;
        id: string;
    }>();
    const record = (id: string, status: AgentResult['status'], message: string): AgentResult => {
        const value: AgentResult = { id, status, message, revision: ctx.state.revision };
        results.set(id, value);
        return value;
    };
    const blocked = (message: string): AgentResult => ({ id: '', status: 'blocked', message, revision: ctx.state.revision });
    const disable = (): void => {
        enabled = false;
        if (pending) {
            record(pending.id, 'cancelled', 'Agent control was disabled.');
            pending.ref.close();
            pending = null;
        }
    };
    const inspect = (): unknown => {
        if (!enabled)
            return { error: 'Agent control is disabled for this tab.' };
        const state = ctx.state;
        return {
            product: 'hatchling-frontend', mode: ctx.adapter.mode, revision: state.revision,
            view: { route: ctx.ui.route, chatId: ctx.ui.chatId, contextTab: ctx.ui.contextTab, contextOpen: ctx.ui.contextOpen, sideChatsOpen: ctx.ui.sideChatsOpen, workspaceOpen: ctx.dock.isOpen, interfaceSize: state.settings.interfaceSize },
            chats: state.chats.map(({ id, title, archived, unread, channelId }) => ({ id, title, archived, unread, readOnly: channelId !== null })),
            documents: state.memory.map(({ id, title, kind }) => ({ id, title, kind })),
            artifacts: state.artifacts.map(({ id, title, format, version, trashUntil }) => ({ id, title, format, version, trashed: trashUntil !== null })),
            activities: state.threads.map(({ id, name, state: status, subtitle }) => ({ id, name, status, subtitle })),
            browserTasks: state.browserTasks.map(({ id, state: status, controller }) => ({ id, status, controller })),
            goals: state.goals.map(({ id, title, status }) => ({ id, title, status })),
            recentActions: [...results.values()].slice(-20),
        };
    };
    const read = (resource: string, id: string): unknown => {
        if (!enabled)
            return { error: 'Agent control is disabled for this tab.' };
        const state = ctx.state;
        // Explicit content reads; never export the entire snapshot, payment methods or secure references.
        switch (resource) {
            case 'document': {
                const note = state.memory.find(item => item.id === id);
                return note ? { ...note, revision: state.revision } : { error: 'Document not found.' };
            }
            case 'artifact': {
                const file = state.artifacts.find(item => item.id === id && !item.trashUntil);
                return file ? { id, title: file.title, format: file.format, content: file.content, version: file.version, revision: state.revision } : { error: 'Artifact not found.' };
            }
            case 'activity': return state.threads.find(item => item.id === id) ?? { error: 'Activity not found.' };
            case 'chat': return { messages: state.messages.filter(item => item.chatId === id && ['text', 'attachment', 'browser', 'progress', 'notice'].includes(item.kind)), revision: state.revision };
            default: return { error: 'Use document, artifact, activity, or chat with an explicit id.' };
        }
    };
    const execute = async (input: unknown): Promise<AgentResult> => {
        if (!enabled)
            return blocked('Enable Agent control for this tab in Settings → Help & support.');
        if (!isRecord(input) || Object.keys(input).some(key => !['key', 'action', 'expectedRevision'].includes(key)) || typeof input['key'] !== 'string' || !/^[a-zA-Z0-9_-]{1,80}$/u.test(input['key']) || !isRecord(input['action']))
            return blocked('Use {key, action, expectedRevision?}; key must be a unique short identifier.');
        const action = input['action'];
        const name = action['type'];
        const description = actionCatalog().find(item => item.name === name);
        if (!description || !validateArguments(description.inputSchema, action))
            return blocked('Unknown action or invalid arguments. Inspect list() for exact schemas.');
        if (description.policy === 'internal')
            return blocked('This event belongs to the adapter, not an agent-facing control.');
        if (description.policy === 'human-only')
            return blocked('This action must be performed by the user in its native review, secure-entry or permission screen. Agent control cannot approve it.');
        const signature = JSON.stringify({ action, expectedRevision: input['expectedRevision'] });
        const previous = seen.get(input['key']);
        if (previous)
            return previous.signature === signature ? results.get(previous.id) ?? blocked('Request unavailable.') : blocked('This key already identifies a different request.');
        if (seen.size >= 500)
            return blocked('Session action limit reached. Reload and explicitly re-enable to start a new session.');
        if (pending || document.querySelector('dialog:modal'))
            return blocked('A user dialog is open. Wait for the user; agent commands cannot dismiss it.');
        if (Object.values(action).some(value => typeof value === 'string' && containsSensitiveValue(value)))
            return blocked('Sensitive values must not enter agent-control requests. Use protected user entry.');
        const revision = input['expectedRevision'];
        if (description.policy !== 'view' && (typeof revision !== 'number' || !Number.isInteger(revision)))
            return blocked('Mutations require expectedRevision from a fresh inspect() response.');
        if (revision !== undefined && revision !== ctx.state.revision)
            return blocked('The state has changed. Inspect it again before requesting this action.');
        // Session-local receipt, not an auth token. Also works in offline/insecure documents.
        const id = `frontend-action-${++requestSequence}`;
        seen.set(input['key'], { signature, id });
        if (description.policy === 'view') {
            const error = executeView(ctx, action);
            return record(id, error ? 'rejected' : 'done', error ?? 'View updated.');
        }
        if (action['type'] === 'chat.send' && typeof action['text'] === 'string' && /^(yes|yes please|approve|looks good)[.!]?$/iu.test(action['text'].trim()) && ctx.state.purchases.some(item => item.state === 'proposed'))
            return record(id, 'blocked', 'A purchase awaits review. The user must approve the exact quote in its native review screen.');
        const command = parseCommand(action);
        if (!command)
            return record(id, 'rejected', 'Invalid protocol command.');
        const body = h('div', { class: 'stack agent-review-content' }, h('p', {}, 'An agent requested this change. Nothing happens until you approve this exact request.'), h('p', { class: 'muted text-small' }, 'Changes made since this request invalidate it. Approval is one-time, not a standing permission.'), h('pre', { class: 'agent-request-payload', tabindex: 0 }, JSON.stringify(action, null, 2)));
        const ref = ctx.dialogs.open('Review agent action', body, 'agent-review-dialog');
        const finish = (status: AgentResult['status'], message: string): void => { record(id, status, message); pending = null; ref.close(); };
        const cancel = button('Decline', () => finish('cancelled', 'The user declined this request.'));
        const approve = h('button', { class: 'button primary', type: 'button' }, 'Approve this change');
        approve.addEventListener('click', event => {
            if (!event.isTrusted || !enabled || pending?.id !== id)
                return;
            if (ctx.state.revision !== revision) {
                finish('stale', 'State changed while this request was open. Inspect and request again.');
                ctx.toast('The request changed or is out of date. Nothing was applied.', true);
                return;
            }
            approve.disabled = true;
            // Consume approval before dispatch. A double click cannot replay it.
            pending = null;
            void ctx.run(command).then(result => finish(result.ok ? 'done' : 'rejected', result.message || 'Action completed.'));
        });
        ref.element.addEventListener('close', () => {
            if (pending?.id === id) {
                pending = null;
                record(id, 'cancelled', 'The review was dismissed.');
            }
        }, { once: true });
        body.append(h('div', { class: 'dialog-actions' }, cancel, approve));
        pending = { id, ref };
        cancel.focus();
        return record(id, 'pending', 'Waiting for a user review in the frontend. Poll result(id); do not repeat the action.');
    };
    const port: AgentPort = Object.freeze({ product: 'hatchling-frontend', version: '1.0', status: () => ({ product: 'hatchling-frontend', version: '1.0', enabled, pending: pending?.id ?? null }), list: () => structuredClone(actionCatalog()), inspect: () => structuredClone(inspect()), read: (resource: string, id: string) => structuredClone(read(resource, id)), execute: async (input: unknown) => structuredClone(await execute(input)), result: (id: string) => enabled ? structuredClone(results.get(id) ?? null) : null });
    Object.defineProperty(window, 'hatchlingFrontend', { value: port, configurable: true, writable: false });
    const show = (): void => {
        const body = h('div', { class: 'stack' }, h('p', {}, 'Allow a local agent driver to inspect this workspace and use named UI actions. Off by default and forgotten on reload.'), h('p', { class: 'muted text-small' }, 'Changes require a fresh review. Payments, permissions, credentials and browser takeover remain user-only. This is a same-page development interface, not a security boundary against DevTools or other trusted page code.'));
        const toggle = h('button', { class: 'button primary', type: 'button', 'aria-pressed': String(enabled) }, enabled ? 'Disable agent control' : 'Enable for this tab');
        const status = h('p', { role: 'status', class: 'agent-control-status' }, enabled ? 'Enabled for this tab' : 'Disabled');
        toggle.addEventListener('click', event => {
            if (!event.isTrusted)
                return;
            if (enabled)
                disable();
            else
                enabled = true;
            toggle.textContent = enabled ? 'Disable agent control' : 'Enable for this tab';
            toggle.setAttribute('aria-pressed', String(enabled));
            status.textContent = enabled ? 'Enabled for this tab' : 'Disabled';
        });
        body.append(toggle, status, h('p', { class: 'muted text-small' }, 'Never enable remote debugging on your everyday browser profile.'));
        ctx.dialogs.open('Agent control', body, 'agent-control-dialog');
    };
    const dispose = (): void => { disable(); controllers.delete(ctx); Reflect.deleteProperty(window, 'hatchlingFrontend'); };
    controllers.set(ctx, { show, dispose });
    return dispose;
}
