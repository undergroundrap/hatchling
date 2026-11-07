import type { AppContext } from '../../app/context.js';
import type { ArtifactFormat, Artifact, DeepReadonly, Message } from '../../protocol/models.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, iconButton, menu } from '../../components/ui.js';
import { setFloatingMenuOpen } from '../../components/floating.js';
export function attachmentLabel(artifact: DeepReadonly<Artifact>): string {
    if (artifact.format === 'archive')
        return 'GZ';
    const suffix = artifact.title.match(/\.([a-z0-9]{1,8})$/i)?.[1];
    const labels: Partial<Record<ArtifactFormat, string>> = { markdown: 'MD', code: 'CODE', text: 'TXT', word: 'DOCX', spreadsheet: 'XLSX', slides: 'SLIDES' };
    return suffix?.toUpperCase() ?? labels[artifact.format] ?? artifact.format.toUpperCase();
}
export function nativeAttachment(ctx: AppContext, artifact: DeepReadonly<Artifact>): HTMLElement {
    const open = h('button', { class: 'native-file-open', type: 'button', 'aria-label': `Open ${artifact.title}` }, h('span', { class: 'native-file-glyph' }, icon('file', 32), h('small', {}, artifact.format === 'archive' ? '?' : '')), h('span', { class: 'row-copy' }, h('strong', {}, artifact.title), h('span', { class: 'native-file-type' }, attachmentLabel(artifact))));
    open.addEventListener('click', () => ctx.openArtifact(artifact.id));
    const more = menu(`File options for ${artifact.title}`, [{ label: 'Download', icon: 'download', action: () => { void ctx.run({ type: 'artifact.download', id: artifact.id }, true); } }]);
    return h('article', { class: 'native-file-card attachment-card', 'aria-label': artifact.title }, open, more);
}
function content(message: DeepReadonly<Message>, ctx: AppContext): string {
    if ('text' in message)
        return message.text;
    if (message.kind === 'browser')
        return ctx.state.browserTasks.find(item => item.id === message.taskId)?.title ?? 'Browser session';
    return message.kind === 'purchase' ? 'Purchase review' : message.service;
}
function formatMessageTimestamp(time: string, live: boolean): string {
    const normalized = time.replace(/\b(am|pm)\b/i, (match) => match.toUpperCase());
    return live ? normalized : `Today at ${normalized}`;
}
function messageMetaMenu(label: string): HTMLElement {
    const details = h('details', { class: 'menu message-meta-menu' });
    const summary = h('summary', { class: 'icon-button', 'aria-label': 'Message details', 'aria-expanded': 'false', 'data-tooltip': 'More', title: 'More' }, icon('more'));
    const content = h('div', { class: 'menu-popover message-meta-popover', role: 'note', 'aria-label': 'Message details' }, h('span', { class: 'message-meta-label' }, label));
    details.append(summary, content);
    summary.addEventListener('click', (event) => {
        event.preventDefault();
        setFloatingMenuOpen(details, !details.open);
    });
    details.addEventListener('keydown', (event) => {
        if (event.key === 'Escape' && details.open) {
            event.preventDefault();
            event.stopPropagation();
            setFloatingMenuOpen(details, false);
            summary.focus({ preventScroll: true });
        }
        else if (event.key === 'Tab' && details.open) {
            setFloatingMenuOpen(details, false);
        }
    });
    details.addEventListener('focusout', (event) => {
        if (event.relatedTarget instanceof Node && details.contains(event.relatedTarget))
            return;
        queueMicrotask(() => {
            if (details.open && !details.contains(document.activeElement))
                setFloatingMenuOpen(details, false);
        });
    });
    return details;
}
export function messageActions(ctx: AppContext, message: DeepReadonly<Message>): HTMLElement {
    const root = h('div', { class: 'message-actions message-reactions', 'aria-label': 'Message actions' });
    const reactions = ['👍', '❤️', '😂', '😮', '😢', '🙏'];
    const react = (reaction: string): void => { void ctx.run({ type: 'message.react', id: message.id, reaction }, true); };
    const picker = menu(`React to message ${message.id}`, reactions.map(reaction => ({ label: reaction, icon: 'heart' as const, action: () => react(reaction) })));
    picker.classList.add('reaction-menu');
    picker.querySelector('summary')?.replaceChildren(icon('smile', 17));
    for (const [index, node] of picker.querySelectorAll('.menu-popover > button').entries()) {
        node.replaceChildren(h('span', { 'aria-hidden': 'true' }, reactions[index] ?? ''));
        node.setAttribute('aria-label', reactions[index] ?? 'Reaction');
    }
    const extras = button('', () => {
        picker.removeAttribute('open');
        const body = h('div', { class: 'emoji-grid' });
        const ref = ctx.dialogs.open('More reactions', body);
        for (const reaction of [...reactions, '😊', '🎉', '🔥', '👏', '💡', '✅'])
            body.append(button(reaction, () => { ref.close(); react(reaction); }, { variant: 'ghost' }));
    }, { className: 'more-reactions' });
    extras.replaceChildren(icon('plus', 16));
    extras.setAttribute('aria-label', 'More reactions');
    picker.querySelector('.menu-popover')?.append(extras);
    const text = content(message, ctx);
    const reply = iconButton('reply', `Reply to message ${message.id}`, () => ctx.prefillComposer(`> ${text.split('\n')[0] ?? ''}\n\n`));
    const copy = iconButton('copy', `Copy message ${message.id}`, () => {
        void (async () => {
            try {
                await navigator.clipboard.writeText(text);
                ctx.toast('Copied');
            }
            catch {
                const input = h('textarea', { rows: 7, readonly: true, 'aria-label': 'Message text to copy' });
                input.value = text;
                const ref = ctx.dialogs.open('Copy message', h('div', { class: 'stack' }, input, h('p', { class: 'muted' }, 'Clipboard access is unavailable here. Select and copy the text.')));
                ref.element.querySelector('textarea')?.select();
            }
        })();
    });
    const meta = messageMetaMenu(formatMessageTimestamp(message.time, ctx.state.mode === 'live'));
    root.append(picker, reply, copy, meta);
    return root;
}
