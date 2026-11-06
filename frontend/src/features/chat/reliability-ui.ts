import { skeletonContent } from '../../components/skeleton.js';
import type { AppContext } from '../../app/context.js';
import type { IconName } from '../../protocol/models.js';
import type { ConnectionKind, ReliabilityScenario } from '../../session/chat-session.js';
import type { OutboxItem } from './delivery.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, markdown } from '../../components/ui.js';
const connectionLabels: Record<ConnectionKind, string> = { ready: 'Connected', offline: 'You’re offline', reconnecting: 'Reconnecting', restarting: 'Workspace is restarting', unavailable: 'Service is unavailable', 'sign-in-required': 'Your session needs attention', locked: 'Hatchling is locked' };
export function connectionBanner(ctx: AppContext): HTMLElement | null {
    const connection = ctx.delivery.connection;
    if (connection.kind === 'ready')
        return null;
    const busy = connection.kind === 'reconnecting' || connection.kind === 'restarting';
    const glyph: IconName = busy ? 'refresh' : connection.kind === 'sign-in-required' || connection.kind === 'locked' ? 'lock' : 'warning';
    const action = ctx.delivery.port ? button(connection.kind === 'sign-in-required' ? 'Reconnect session' : connection.kind === 'locked' ? 'Check again' : busy ? 'Check connection' : 'Reconnect', () => { void ctx.delivery.reconnect(); }, { variant: 'ghost', className: 'reliability-action', disabled: connection.kind === 'reconnecting' || !navigator.onLine, id: 'reconnect-chat' }) : null;
    return h('section', { class: `connection-banner connection-${connection.kind}`, role: 'status', 'aria-live': 'polite' }, h('span', { class: busy ? 'status-glyph is-spinning' : 'status-glyph' }, icon(glyph, 18)), h('div', { class: 'connection-copy' }, h('strong', {}, connectionLabels[connection.kind]), h('p', {}, connection.detail || 'Your draft and visible messages stay here.')), action);
}
export function historyNotice(ctx: AppContext): HTMLElement | null {
    const history = ctx.delivery.history(ctx.ui.chatId);
    if (history.phase === 'ready' || !history.hasCache)
        return null;
    const loading = history.phase === 'loading';
    return h('section', { class: 'history-inline', role: 'status' }, h('span', { class: loading ? 'status-glyph is-spinning' : 'status-glyph' }, icon(loading ? 'refresh' : 'warning', 17)), h('div', {}, h('strong', {}, loading ? 'Updating the conversation' : 'Couldn’t load the latest messages'), h('p', {}, loading ? 'Your last loaded messages stay visible.' : 'You’re viewing the last loaded chat. New messages may be missing.')), button(loading ? 'Retrying…' : 'Retry', () => { void ctx.delivery.retryHistory(ctx.ui.chatId); }, { variant: 'ghost', disabled: loading || ctx.delivery.connection.kind !== 'ready', id: 'retry-chat-history' }));
}
export function historyPlaceholder(ctx: AppContext): HTMLElement {
    const history = ctx.delivery.history(ctx.ui.chatId);
    const loading = history.phase === 'loading';
    const empty = history.phase === 'ready';
    const body = h('div', { class: `history-placeholder ${loading ? 'is-loading' : ''}`, role: 'status', 'aria-live': 'polite' }, h('span', { class: 'history-state-icon' }, icon(empty ? 'chat' : loading ? 'chat' : 'warning', 25)), h('h2', {}, empty ? 'A little space for a new thought' : loading ? 'Loading your conversation' : 'This conversation couldn’t load'), h('p', {}, empty ? 'Send a message to get started. This chat has its own context.' : loading ? 'Getting your messages. You can start writing below.' : 'We couldn’t retrieve the messages. Try again; your draft will stay right here.'));
    if (loading)
        body.prepend(h('div', { class: 'history-skeleton', 'aria-busy': true, 'aria-hidden': true }, skeletonContent('chat')));
    if (!loading && !empty)
        body.append(h('div', { class: 'history-state-actions' }, button('Retry loading chat', () => { void ctx.delivery.retryHistory(ctx.ui.chatId); }, { icon: 'refresh', variant: 'primary', disabled: ctx.delivery.connection.kind !== 'ready', id: 'retry-chat-history' }), button('Open another chat', () => { ctx.ui.navOpen = true; ctx.render(); document.getElementById('chat-search')?.focus(); }, { variant: 'ghost', id: 'open-another-chat' })));
    return body;
}
function copyUnsent(ctx: AppContext, item: Readonly<OutboxItem>): void {
    const text = item.command.type === 'chat.send' ? item.command.text : ctx.state.artifacts.find(artifact => item.command.type === 'chat.attach' && artifact.id === item.command.artifactId)?.title ?? 'Attachment';
    void (async () => {
        try {
            await navigator.clipboard.writeText(text);
            ctx.toast('Copied');
        }
        catch {
            const input = h('textarea', { rows: 6, readonly: true, 'aria-label': 'Unsent message to copy' });
            input.value = text;
            const ref = ctx.dialogs.open('Copy unsent message', h('div', { class: 'stack' }, input, h('p', { class: 'muted' }, 'Select and copy this text. Nothing is being resent.')));
            ref.element.querySelector('textarea')?.select();
        }
    })();
}
export function outboxRow(ctx: AppContext, item: Readonly<OutboxItem>): HTMLElement {
    const failed = item.phase === 'failed';
    const unknown = item.phase === 'unconfirmed';
    const queued = item.phase === 'queued';
    const sending = item.phase === 'sending';
    const title = sending ? 'Sending…' : failed ? 'Not sent' : unknown ? 'Delivery not confirmed' : item.phase === 'accepted' ? 'Sent · syncing conversation' : ctx.state.chats.find(chat => chat.id === item.command.chatId)?.archived ? 'Queued · chat is archived' : ctx.delivery.restoredQueue ? 'Queued · waiting for your review' : ctx.delivery.connection.kind !== 'ready' ? 'Queued · waiting for connection' : 'Queued · waiting its turn';
    const glyph: IconName = sending ? 'refresh' : failed || unknown ? 'warning' : item.phase === 'accepted' ? 'check' : 'clock';
    const bubble = item.command.type === 'chat.send' ? h('div', { class: 'bubble user-bubble' }, markdown(item.command.text, false)) : h('div', { class: 'bubble user-bubble outbox-file' }, icon('file', 21), h('span', {}, ctx.state.artifacts.find(artifact => item.command.type === 'chat.attach' && artifact.id === item.command.artifactId)?.title ?? 'Attached file'));
    const row = h('article', { class: `message-row user outbox-row delivery-${item.phase}`, 'data-outbox-id': item.id, id: `outbox-${item.id}`, 'aria-label': `${item.command.type === 'chat.attach' ? 'Attachment' : 'Message'}: ${title}`, tabindex: -1 }, bubble, h('div', { class: 'delivery-status', role: 'status' }, h('span', { class: sending || item.checking ? 'status-glyph is-spinning' : 'status-glyph' }, icon(glyph, 13)), h('span', {}, title)));
    if (item.detail && (failed || unknown))
        row.append(h('p', { class: 'delivery-detail' }, item.detail));
    const actions = h('div', { class: 'outbox-actions', 'aria-label': 'Unsent message actions' });
    if (failed)
        actions.append(button('Retry', () => ctx.delivery.retry(item.id), { icon: 'refresh', variant: 'ghost', id: `retry-${item.id}`, disabled: ctx.delivery.connection.kind !== 'ready' || ctx.delivery.history(item.command.chatId).phase !== 'ready' || !!ctx.state.chats.find(chat => chat.id === item.command.chatId)?.archived }));
    if (unknown && ctx.delivery.port)
        actions.append(button(item.checking ? 'Checking…' : 'Check status', () => { void ctx.delivery.check(item.id); }, { icon: 'refresh', variant: 'ghost', id: `check-${item.id}`, disabled: item.checking || ctx.delivery.connection.kind !== 'ready' }));
    if (queued || failed)
        actions.append(button('Edit', () => {
            const held = ctx.delivery.edit(item.id);
            if (!held.length)
                return;
            const text = held.flatMap(entry => entry.command.type === 'chat.send' ? [entry.command.text] : []).join('\n\n');
            const files = held.flatMap(entry => entry.command.type === 'chat.attach' ? [entry.command.artifactId] : []);
            ctx.recoverComposer(item.command.chatId, text, files);
            document.getElementById('message-input')?.focus({ preventScroll: true });
        }, { variant: 'ghost', id: `edit-${item.id}` }));
    if (!sending && item.phase !== 'accepted')
        actions.append(button('Copy', () => copyUnsent(ctx, item), { variant: 'ghost', id: `copy-${item.id}` }));
    if (queued || failed)
        actions.append(button('Remove', () => ctx.dialogs.confirm('Remove this unsent message?', 'It will be removed from your outbox and will not be sent. Other messages are kept.', () => { ctx.delivery.remove(item.id); document.getElementById('message-input')?.focus({ preventScroll: true }); }, { label: 'Remove message', danger: true }), { variant: 'ghost', id: `remove-${item.id}` }));
    if (unknown)
        actions.append(button('Dismiss', () => ctx.dialogs.confirm('Dismiss this delivery warning?', 'This does not cancel the original message. It may still arrive. Nothing will be resent, and later queued messages will continue. Check the conversation before repeating the request.', () => { ctx.delivery.dismissUnconfirmed(item.id); document.getElementById('message-input')?.focus({ preventScroll: true }); }, { label: 'Dismiss warning' }), { variant: 'ghost', id: `dismiss-${item.id}`, disabled: item.checking }));
    if (item.phase === 'accepted' && ctx.delivery.port)
        actions.append(button('Refresh chat', () => { void ctx.delivery.retryHistory(item.command.chatId); }, { variant: 'ghost', id: `sync-${item.id}`, disabled: ctx.delivery.history(item.command.chatId).phase === 'loading' }));
    if (actions.childElementCount)
        row.append(actions);
    return row;
}
export function outboxNotice(ctx: AppContext): HTMLElement | null {
    const count = ctx.delivery.count(ctx.ui.chatId);
    if (!count)
        return null;
    return h('div', { class: 'outbox-notice', role: 'status' }, icon('clock', 14), h('span', {}, ctx.delivery.restoredQueue ? 'Unsent messages recovered. Review them before continuing.' : ctx.delivery.recoveryAvailable ? `${count} ${count === 1 ? 'message' : 'messages'} in your outbox · saved for this tab.` : `${count} ${count === 1 ? 'message' : 'messages'} in your outbox · keep this tab open.`), ctx.delivery.restoredQueue ? button('Resume queue', () => ctx.delivery.resumeRecovered(), { variant: 'link', id: 'resume-outbox' }) : null);
}
export const reliabilityScenarios: readonly {
    id: ReliabilityScenario;
    title: string;
    description: string;
    icon: IconName;
    draft?: string;
}[] = [
    { id: 'history-loading', title: 'Chat is loading', description: 'A calm loading state, with the composer available for a draft.', icon: 'chat' },
    { id: 'history-error', title: 'Chat couldn’t load', description: 'An inline retry, not an empty transcript or a disappearing toast.', icon: 'warning' },
    { id: 'history-stale', title: 'Latest messages unavailable', description: 'Keep cached messages readable while retrying the latest history.', icon: 'refresh' },
    { id: 'vm-restarting', title: 'Workspace restarting', description: 'Send a sample message. It queues, then sends once the demo recovers.', icon: 'device', draft: 'Keep the service receipts together and let me know when you’re back.' },
    { id: 'slow-send', title: 'Message is sending', description: 'A translucent bubble stays visible while the receipt is pending.', icon: 'clock', draft: 'Please keep this note with the rest of our plan.' },
    { id: 'send-failed', title: 'Message was not sent', description: 'A definite rejection with Retry, Edit, Copy and Remove.', icon: 'warning', draft: 'Remind me to review the receipts before Friday.' },
    { id: 'send-unknown', title: 'Delivery is uncertain', description: 'Check the receipt without resending a possible duplicate.', icon: 'shield', draft: 'Keep this request once, even if the connection drops.' },
    { id: 'offline', title: 'Offline with queued messages', description: 'Draft freely. New messages wait for an explicit reconnect.', icon: 'globe', draft: 'Save this thought for when the connection returns.' },
    { id: 'service-unavailable', title: 'Service unavailable', description: 'Retain the chat, explain the outage and offer reconnect.', icon: 'warning' },
    { id: 'session-expired', title: 'Session needs attention', description: 'Pause sending without deleting the draft or the transcript.', icon: 'lock' },
    { id: 'attachment-failed', title: 'One attachment fails', description: 'Attach a file and send. Retry only the failed file, not the text.', icon: 'paperclip', draft: 'Here is the file for this conversation.' },
    { id: 'ready', title: 'Back to normal', description: 'Clear simulated connection problems. Your unsent messages stay in the outbox.', icon: 'check' },
];
