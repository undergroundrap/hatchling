import type { ContextUse } from '../../session/usage.js';
import { loadingImage } from '../../components/loading-image.js';
import { domainApprovalNotice } from '../approvals/domain.js';
import { renderTaskApprovalQueue } from '../approvals/surfaces.js';
import { connectionBanner, historyNotice, historyPlaceholder, outboxNotice, outboxRow } from './reliability-ui.js';
import { connectorBanner } from './connector-chats.js';
import { renderChatHeader } from './chat-header.js';
import { avatarAccentFromImage } from '../context/avatar-color.js';
import { browserMessageCard } from '../browser/session.js';
import { messageActions, nativeAttachment } from './message-actions.js';
import { floatingAgent } from '../context/presence.js';
import { bindIconMotion } from '../../components/motion.js';
import type { AppContext } from '../../app/context.js';
import type { ArtifactFormat, DeepReadonly, Message } from '../../protocol/models.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, emptyState, iconButton, markdown, menu, pill } from '../../components/ui.js';
import { purchaseCard } from './purchase-card.js';
export function renderChat(ctx: AppContext): HTMLElement {
    const chat = ctx.state.chats.find((item) => item.id === ctx.ui.chatId);
    if (!chat)
        return h('section', { class: 'chat-view' }, renderChatHeader(ctx, undefined), emptyState('chat', 'This conversation is not available.', 'Open Chats to choose an available conversation.'));
    const history = ctx.delivery.history(chat.id);
    const messages = history.hasCache || history.phase === 'ready' ? ctx.state.messages.filter(message => message.chatId === chat.id) : [];
    const pending = ctx.delivery.visibleEntries(chat.id);
    const column = h('div', { class: 'chat-column' }, messages.length ? null : historyPlaceholder(ctx), ...messages.map(message => renderMessage(ctx, message)), ...pending.map(item => outboxRow(ctx, item)), ctx.state.turn === 'working' && ctx.delivery.connection.kind === 'ready' && history.phase === 'ready' ? h('div', { class: 'typing-indicator', 'aria-label': 'Working on it' }, h('i'), h('i'), h('i')) : null);
    const stream = h('div', { class: `chat-scroll${messages.length === 0 ? ' has-history-state' : ''}`, 'data-scroll': `chat-${ctx.ui.chatId}`, role: 'log', 'aria-label': 'Conversation', 'aria-live': 'polite', 'aria-busy': history.phase === 'loading' }, column);
    const connection = connectionBanner(ctx);
    const stale = historyNotice(ctx);
    const channel = chat.channelId ? connectorBanner(ctx, chat) : null;
    const view = h('section', { class: `chat-view${connection || stale || channel ? ' has-chat-notice' : ''}` }, renderChatHeader(ctx, chat), h('div', { class: 'chat-notices' }, connection, stale, channel), stream, renderComposer(ctx, chat.channelId !== null));
    if ((!ctx.ui.contextOpen && !ctx.dock.isOpen) || document.body.classList.contains('has-browser-session'))
        view.append(floatingAgent(ctx));
    const jump = iconButton('arrow', 'Jump to latest', () => { stream.scrollTo({ top: stream.scrollHeight, behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'instant' : 'smooth' }); }, 'jump-to-latest');
    jump.replaceChildren(h('span', {}, 'See more'), icon('chevron', 13));
    jump.removeAttribute('data-tooltip');
    jump.hidden = true;
    // Anchor to the actual composer, including multiline drafts and attachment chips.
    view.querySelector('.composer-area')?.append(jump);
    return view;
}
function renderMessage(ctx: AppContext, message: DeepReadonly<Message>): HTMLElement {
    const user = message.role === 'user';
    const root = h('div', { class: `message-row ${user ? 'user' : 'agent'} kind-${message.kind}`, 'data-message-id': message.id });
    if (message.source)
        root.append(pill(message.source, 'source-pill'));
    const chatMessages = ctx.state.messages.filter(item => item.chatId === message.chatId);
    const nextMessage = chatMessages[chatMessages.findIndex(item => item.id === message.id) + 1];
    if (user && nextMessage?.kind === 'browser')
        root.append(h('time', { class: 'browser-message-time' }, message.time));
    if (message.kind === 'browser') {
        const task = ctx.state.browserTasks.find(item => item.id === message.taskId);
        if (task)
            root.append(browserMessageCard(ctx, task));
    }
    else if (message.kind === 'text') {
        const bubbles = [message.text];
        for (const text of bubbles)
            root.append(h('div', { class: `bubble ${user ? 'user-bubble' : 'agent-bubble'}` }, markdown(text, false)));
    }
    else if (message.kind === 'choice') {
        root.append(h('div', { class: 'bubble agent-bubble choice-bubble' }, h('p', {}, message.text), h('div', { class: 'choice-options', role: 'group', 'aria-label': message.text }, ...message.options.map((option) => {
            const choice = button(option, () => { void ctx.run({ type: 'message.choose', id: message.id, option }); }, { variant: 'ghost', className: `choice-button ${message.selected === option ? 'selected' : ''}`, disabled: message.selected !== null });
            choice.setAttribute('aria-label', option);
            choice.setAttribute('aria-pressed', String(message.selected === option));
            return choice;
        }))));
    }
    else if (message.kind === 'purchase') {
        const purchase = ctx.state.purchases.find((item) => item.id === message.purchaseId);
        if (purchase)
            root.append(purchaseCard(ctx, purchase));
    }
    else if (message.kind === 'attachment') {
        const artifact = ctx.state.artifacts.find((item) => item.id === message.artifactId);
        root.append(h('p', { class: 'attachment-intro' }, message.text));
        if (artifact) {
            if (artifact.format === 'image' && artifact.asset) {
                const open = h('button', { class: 'message-image', 'aria-label': `Open ${artifact.title}`, type: 'button' }, loadingImage({ src: artifact.asset, alt: artifact.title, lazy: true }));
                open.addEventListener('click', () => ctx.openArtifact(artifact.id));
                root.append(open);
            }
            else {
                root.append(nativeAttachment(ctx, artifact));
            }
        }
        else {
            root.append(h('div', { class: 'missing-attachment', role: 'note' }, icon('file', 18), h('span', {}, 'This file is no longer available. The rest of your conversation is kept.')));
        }
    }
    else if (message.kind === 'secure') {
        root.append(h('article', { class: 'secure-card' }, h('div', { class: 'secure-icon' }, icon('lock', 22)), h('strong', {}, `Secure entry · ${message.service}`), h('p', { class: 'muted' }, 'Credentials go straight to secure storage, unreadable by the agent.'), message.completed ? pill('Handoff complete', 'good') : button('Open secure entry', () => openSecure(ctx, message.id, message.service), { icon: 'lock', variant: 'primary' }), h('span', { class: 'tiny muted' }, 'Demo: no credential values are collected.')));
    }
    else if (message.kind === 'code-request') {
        root.append(h('article', { class: 'secure-card' }, icon('keyboard', 22), h('p', {}, message.text), button(message.completed ? 'Site step completed' : 'Open secure browser step', () => {
            const body = h('div', { class: 'stack' }, h('p', {}, 'A real one-time code belongs on the site that requested it, never in chat or memory.'), h('div', { class: 'notice' }, icon('lock', 20), 'This local preview does not collect a code.'));
            const ref = ctx.dialogs.open('One-time site verification', body);
            body.append(button('Complete sample site step', () => { void ctx.run({ type: 'message.code-complete', id: message.id }); ref.close(); }, { variant: 'primary' }));
        }, { variant: 'secondary', disabled: message.completed })));
    }
    else if (message.kind === 'scheduled') {
        const evidenceId = message.evidenceId;
        // Scheduled messages expose evidence, not a job id. Keep the existing evidence action.
        root.append(h('article', { class: 'stream-event scheduled scheduled-message-card' }, h('header', {}, h('span', { class: 'schedule-symbol calendar', 'aria-hidden': 'true' }, icon('calendar', 32), h('span', { class: 'schedule-badge' }, icon('clock', 12))), h('div', {}, h('strong', {}, 'Scheduled update'), h('span', { class: 'eyebrow' }, 'Scheduled delivery'))), h('p', {}, message.text), evidenceId ? button('View evidence', () => ctx.openThread(evidenceId), { className: 'scheduled-evidence-button' }) : null));
    }
    else {
        const evidenceId = message.evidenceId;
        const name = message.kind === 'device' ? 'device' : message.kind === 'error' ? 'warning' : 'run';
        root.append(h('div', { class: `stream-event ${message.kind}` }, icon(name, 16), h('div', {}, h('p', {}, message.text), evidenceId ? button('View evidence', () => ctx.openThread(evidenceId), { variant: 'link' }) : null)));
    }
    if (message.reaction) {
        const reaction = button(message.reaction, () => { void ctx.run({ type: 'message.react', id: message.id, reaction: '' }, true); }, { className: 'reaction-chip' });
        reaction.setAttribute('aria-label', `Remove ${message.reaction} reaction`);
        root.append(reaction);
    }
    if (user && ctx.delivery.receipt(message.id))
        root.append(h('div', { class: 'delivery-status delivery-confirmed', 'aria-label': 'Message sent' }, icon('check', 13), h('span', {}, 'Sent')));
    root.append(messageActions(ctx, message));
    return root;
}
function openSecure(ctx: AppContext, messageId: string, service: string): void {
    const body = h('div', { class: 'stack' }, h('div', { class: 'secure-handshake' }, icon('lock', 36), h('h3', {}, 'Private by design'), h('p', {}, `The live provider’s secure surface would open for ${service}. The agent receives only a protected reference.`)), h('p', { class: 'muted' }, 'This is a local handoff preview, not a credential vault. Do not enter passwords or security codes into the demo.'));
    const ref = ctx.dialogs.open('Secure entry', body);
    body.append(button('Complete sample handoff', () => { void ctx.run({ type: 'message.secure-complete', id: messageId }); ref.close(); }, { variant: 'primary', icon: 'shield' }));
}
function renderComposer(ctx: AppContext, readOnly: boolean): HTMLElement {
    if (readOnly)
        return h('div', { class: 'composer-area' }, renderTaskApprovalQueue(ctx) ?? domainApprovalNotice(ctx), h('div', { class: 'read-only-composer' }, icon('lock', 18), h('span', {}, 'Continue this conversation in its messaging app.')), h('p', { class: 'composer-caption' }, 'Messages, service updates and purchase reviews stay in this connector chat.'));
    const form = h('form', { class: 'composer', 'aria-label': 'Message composer' });
    const input = h('textarea', { id: 'message-input', rows: 1, placeholder: 'Message', 'aria-label': 'Message', autocomplete: 'off' });
    input.value = ctx.ui.draft;
    input.addEventListener('input', () => {
        ctx.ui.draft = input.value;
        input.style.height = 'auto';
        input.style.height = `${Math.min(input.scrollHeight, 150)}px`;
        ctx.saveComposer();
        updateComposer();
    });
    const send = async (): Promise<void> => {
        const chatId = ctx.ui.chatId;
        const text = ctx.ui.draft.trim();
        const attachments = [...ctx.ui.attachmentNames];
        if ((!text && attachments.length === 0) || ctx.delivery.blockedReason(chatId) || ctx.ui.preparingChats.has(chatId) || [...ctx.ui.fileImports.values()].some(item => item.chatId === chatId && item.phase === 'reading'))
            return;
        ctx.ui.preparingChats.add(chatId);
        updateComposer();
        let avatarAccent: string | undefined;
        try {
            if (/^change (?:my|your) avatar to/i.test(text)) {
                const asset = ctx.state.artifacts.find(item => attachments.includes(item.id) && item.format === 'image');
                if (asset?.asset)
                    avatarAccent = await avatarAccentFromImage(asset.asset);
            }
            // Queue immediately; clearing the original draft never waits on a transport receipt.
            if (!ctx.delivery.enqueue(chatId, text, attachments, avatarAccent)) {
                ctx.toast(ctx.delivery.blockedReason(chatId) || 'There is not enough room in the outbox. Your draft is kept.', true);
                return;
            }
            ctx.completeQueuedDraft(chatId, text, attachments);
            ctx.render();
            if (ctx.ui.chatId === chatId) {
                document.getElementById('message-input')?.focus({ preventScroll: true });
                const currentScroll = document.querySelector('.chat-scroll');
                if (currentScroll)
                    currentScroll.scrollTop = currentScroll.scrollHeight;
            }
        }
        catch {
            ctx.toast('The message could not be prepared. Your draft is kept.', true);
        }
        finally {
            ctx.ui.preparingChats.delete(chatId);
            ctx.render();
        }
    };
    form.addEventListener('submit', (event) => { event.preventDefault(); void send(); });
    input.addEventListener('keydown', (event) => {
        if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
            event.preventDefault();
            void send();
        }
    });
    const attach = menu('Add attachments or voice', [
        { label: 'Add a photo', icon: 'image', action: () => chooseFile(ctx, true) },
        { label: 'Attach a file', icon: 'paperclip', action: () => chooseFile(ctx, false) },
        { label: 'Voice note preview', icon: 'audio', action: () => openVoice(ctx, 'note') },
        { label: 'Live voice call preview', icon: 'phone', action: () => openVoice(ctx, 'call') },
    ]);
    const summary = attach.querySelector('summary');
    summary?.replaceChildren(icon('plus', 22));
    const sendButton = h('button', { class: 'send-button', type: 'submit', 'aria-label': 'Send message' }, icon('arrow', 20));
    bindIconMotion(sendButton);
    const microphone = iconButton('mic', 'Voice dictation preview', () => openVoice(ctx, 'dictation'));
    const updateComposer = (): void => {
        const hasContent = input.value.trim().length > 0 || ctx.ui.attachmentNames.length > 0;
        sendButton.hidden = !hasContent;
        sendButton.disabled = !hasContent || !!ctx.delivery.blockedReason(ctx.ui.chatId) || ctx.ui.preparingChats.has(ctx.ui.chatId) || [...ctx.ui.fileImports.values()].some(item => item.chatId === ctx.ui.chatId && item.phase === 'reading');
        sendButton.title = ctx.delivery.blockedReason(ctx.ui.chatId) || (ctx.delivery.connection.kind !== 'ready' ? 'Queue message until the connection returns' : 'Send message');
        microphone.hidden = false;
    };
    form.append(attach, input, microphone, sendButton);
    updateComposer();
    requestAnimationFrame(() => {
        if (input.isConnected && input.value) {
            input.style.height = 'auto';
            input.style.height = `${Math.min(input.scrollHeight, 150)}px`;
        }
    });
    const area = h('div', { class: 'composer-area' });
    const notice = outboxNotice(ctx);
    if (notice)
        area.append(notice);
    const blocked = ctx.delivery.blockedReason(ctx.ui.chatId);
    if (blocked) {
        const archived = ctx.state.chats.find(chat => chat.id === ctx.ui.chatId)?.archived;
        area.append(h('div', { class: 'composer-hint', id: 'composer-hint', role: 'status' }, icon(archived ? 'folder' : 'lock', 14), h('span', {}, blocked), archived ? button('Restore chat', () => { void ctx.run({ type: 'chat.archive', id: ctx.ui.chatId }, true); }, { variant: 'link', id: 'restore-current-chat' }) : null));
        input.setAttribute('aria-describedby', 'composer-hint');
    }
    for (const item of ctx.ui.fileImports.values()) {
        if (item.chatId !== ctx.ui.chatId)
            continue;
        const failed = item.phase === 'failed';
        area.append(h('div', { class: `file-import-status ${failed ? 'failed' : ''}`, role: 'status' }, h('span', { class: failed ? 'status-glyph' : 'status-glyph is-spinning' }, icon(failed ? 'warning' : 'refresh', 14)), h('span', {}, failed ? `${item.file.name}: ${item.error}` : `Preparing ${item.file.name}…`), failed ? button('Retry file', () => { void importLocalFile(ctx, item.id); }, { variant: 'link', id: `retry-import-${item.id}` }) : null, iconButton('close', `Remove ${item.file.name}`, () => { ctx.ui.fileImports.delete(item.id); ctx.render(); })));
    }
    if (ctx.state.turn !== 'idle') {
        const stateLabel = ctx.state.turn === 'working' ? 'Working on it' : ctx.state.turn === 'awaiting' ? 'Waiting for your confirmation' : ctx.state.turn === 'interrupted' ? 'Interrupted — context saved' : 'Stalled — waiting on a source';
        area.append(h('div', { class: 'turn-state', role: 'status' }, icon(ctx.state.turn === 'working' ? 'run' : 'pause', 14), stateLabel, ctx.state.turn === 'working' ? button('Stop', () => { void ctx.run({ type: 'turn.interrupt' }); }, { variant: 'link' }) : ctx.state.turn === 'interrupted' ? button('Resume', () => { void ctx.run({ type: 'turn.resume' }); }, { variant: 'link' }) : null));
    }
    if (ctx.ui.attachmentNames.length)
        area.append(h('div', { class: 'attached-files' }, ...ctx.ui.attachmentNames.map((id) => h('span', { class: 'attachment-chip' }, icon('file', 14), ctx.state.artifacts.find((item) => item.id === id)?.title ?? 'Local file', iconButton('close', 'Remove attachment', () => { ctx.ui.attachmentNames = ctx.ui.attachmentNames.filter((item) => item !== id); ctx.render(); })))));
    const approval = renderTaskApprovalQueue(ctx) ?? domainApprovalNotice(ctx);
    if (approval)
        area.append(approval);
    area.append(form);
    const context = ctx.ui.chatId === 'main' ? ctx.usage?.context() : null;
    if (context)
        area.append(contextMeter(context));
    return area;
}
const SHORT = new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 1 });
/** A model's window as it's named: 16K for 16,384, 128K for 131,072. */
export function windowSize(window: number): string {
    return window % 1024 === 0 ? `${window / 1024}K` : SHORT.format(window);
}
/** How full the model's context was on the last answer: against its window when that's known. */
export function contextMeter(context: ContextUse): HTMLElement {
    const tokens = SHORT.format(context.tokens);
    if (context.window === null)
        return h('p', { class: 'composer-caption context-meter', title: 'The largest prompt of the last answer, as the model server counted it. Ollama says the model’s window itself; for another server, set HATCHLING_SCHEDULER_MODEL_CONTEXT_WINDOW to see it against the model’s window.' }, `Context: ${tokens} tokens on the last answer`);
    const share = Math.min(100, Math.round((context.tokens / context.window) * 100));
    return h('p', { class: 'composer-caption context-meter', title: 'The largest prompt of the last answer, against the model’s context window, as the model server counted it.' }, `Context: ${tokens} of ${windowSize(context.window)} tokens (${share}%) on the last answer`, h('meter', { min: 0, max: context.window, value: Math.min(context.tokens, context.window), 'aria-label': `Context used on the last answer: ${share}%` }));
}
function dataUrl(file: File): Promise<string> {
    return new Promise((resolve, reject) => { const reader = new FileReader(); reader.onload = () => typeof reader.result === 'string' ? resolve(reader.result) : reject(new Error('The file could not be read.')); reader.onerror = () => reject(new Error('The file could not be read.')); reader.readAsDataURL(file); });
}
let importSerial = 0;
function chooseFile(ctx: AppContext, imageOnly: boolean): void {
    const chatId = ctx.ui.chatId;
    const input = h('input', { type: 'file', accept: imageOnly ? 'image/png,image/jpeg,image/webp' : '.md,.txt,.csv,.ts,.json,.pdf,image/png,image/jpeg,image/webp', multiple: true });
    input.addEventListener('change', () => {
        const files = [...(input.files ?? [])];
        if (files.length > 20)
            ctx.toast('Choose up to 20 files at a time. The first 20 are being prepared.', true);
        for (const file of files.slice(0, 20)) {
            const id = `local-import-${++importSerial}`;
            ctx.ui.fileImports.set(id, { id, chatId, file, phase: 'reading', error: '' });
            void importLocalFile(ctx, id);
        }
    });
    input.click();
}
async function importLocalFile(ctx: AppContext, id: string): Promise<void> {
    const item = ctx.ui.fileImports.get(id);
    if (!item)
        return;
    item.phase = 'reading';
    item.error = '';
    ctx.render();
    const file = item.file;
    try {
        if (file.size > 2000000)
            throw new Error('Use a file under 2 MB in this local demo.');
        const isImage = ['image/png', 'image/jpeg', 'image/webp'].includes(file.type);
        const isPdf = file.type === 'application/pdf' || file.name.toLowerCase().endsWith('.pdf');
        if (file.type.startsWith('image/') && !isImage)
            throw new Error('Use a PNG, JPEG or WebP image.');
        const format: ArtifactFormat = isImage ? 'image' : isPdf ? 'pdf' : file.name.endsWith('.md') ? 'markdown' : file.name.endsWith('.csv') ? 'csv' : 'text';
        const content = isImage || isPdf ? '' : await file.text();
        const asset = isImage || isPdf ? await dataUrl(file) : '';
        if (!ctx.ui.fileImports.has(id))
            return;
        const result = await ctx.run({ type: 'artifact.import', title: file.name, format, content, dataUrl: asset }, true);
        if (!ctx.ui.fileImports.has(id))
            return;
        if (!result.ok || !result.id)
            throw new Error(result.ok ? 'This file could not be prepared.' : result.message);
        if (!ctx.state.chats.some(chat => chat.id === item.chatId))
            throw new Error('The original chat is no longer available. The imported file remains in your library.');
        ctx.stageAttachment(item.chatId, result.id);
        ctx.ui.fileImports.delete(id);
        if (ctx.ui.chatId !== item.chatId)
            ctx.toast(`${file.name} was attached to the draft in its original chat.`);
    }
    catch (error: unknown) {
        if (ctx.ui.fileImports.has(id)) {
            item.phase = 'failed';
            item.error = error instanceof Error ? error.message : 'The file could not be read. Try again.';
        }
    }
    finally {
        ctx.render();
    }
}
function openVoice(ctx: AppContext, mode: 'note' | 'call' | 'dictation'): void {
    const body = h('div', { class: 'voice-preview' }, h('img', { src: ctx.state.settings.avatar || './media/hatchling-avatar.png', width: 112, height: 112, alt: 'hatchling' }), h('h3', {}, mode === 'call' ? 'A little more conversational' : mode === 'dictation' ? 'Say it in your own words' : 'Keep the sound of a thought'), h('p', { class: 'muted' }, 'Scripted voice preview · microphone is off'), h('div', { class: 'waveform', 'aria-hidden': 'true' }, ...Array.from({ length: 21 }, (_, i) => h('i', { style: `height:${12 + ((i * 17) % 35)}px` }))));
    const ref = ctx.dialogs.open(mode === 'call' ? 'Live voice call preview' : mode === 'dictation' ? 'Voice dictation preview' : 'Voice note preview', body);
    if (mode === 'dictation')
        body.append(h('p', {}, '“Keep Sunday open, and remind me to pack a book.”'), button('Use sample transcript', () => { ctx.ui.draft = 'Keep Sunday open, and remind me to pack a book.'; ctx.render(); ref.close(); document.getElementById('message-input')?.focus(); }, { variant: 'primary' }));
    if (mode === 'note')
        body.append(h('audio', { controls: true, src: './media/sample-audio.wav', 'aria-label': 'Sample audio note' }), button('Attach sample voice note', () => { ctx.ui.attachmentNames.push('audio'); ctx.render(); ref.close(); }, { variant: 'primary' }));
    if (mode === 'call') {
        const status = h('p', { class: 'voice-status', role: 'status' }, 'Ready for a short sample update.');
        body.append(status);
        body.append(button('Play sample progress', () => {
            const text = 'I’m keeping Sunday open and checking the plan. Nothing will be booked without your review.';
            status.textContent = text;
            if ('speechSynthesis' in window) {
                const localVoice = window.speechSynthesis.getVoices().find((voice) => voice.localService && voice.lang.startsWith('en'));
                if (localVoice) {
                    const utterance = new SpeechSynthesisUtterance(text);
                    utterance.voice = localVoice;
                    window.speechSynthesis.speak(utterance);
                }
                else
                    ctx.toast('No local voice is available. The sample progress is shown as text.');
            }
        }, { icon: 'volume' }), button('End call · keep result in chat', () => {
            if ('speechSynthesis' in window)
                window.speechSynthesis.cancel();
            void ctx.run({ type: 'chat.send', chatId: ctx.ui.chatId, text: 'Keep the packing list simple and Sunday open.' }, true);
            ref.close();
        }, { icon: 'phone', variant: 'danger' }));
        ref.element.addEventListener('close', () => {
            if ('speechSynthesis' in window)
                window.speechSynthesis.cancel();
        });
    }
}
