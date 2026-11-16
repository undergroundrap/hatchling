// Giving the agent a file from the chat (WO-041): the menu item that picks
// one, the row that says how its sending goes, and what the chat shows beside
// the broker's word that it was put. The owner's name for a file is untrusted
// text: shown only inert, in an isolated direction, its hidden characters
// shown as code points, and never put in a message's own text.
import type { AppContext } from '../../app/context.js';
import type { DeepReadonly, Message } from '../../protocol/models.js';
import type { FileSend, GivenFile, InboxPort } from '../../session/inbox.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, iconButton } from '../../components/ui.js';
import { revealedValue } from '../approvals/fields.js';
import { SECRET_SUFFIX, isInboxName, sizeWords } from '../../session/inbox.js';
/** Whether the composer offers a file: the preview's own attach, or, live, giving one to the agent where a file is taken. */
export function offersFiles(ctx: AppContext): boolean {
    return ctx.state.mode !== 'live' || takesFiles(ctx, ctx.ui.chatId);
}
/** Whether the chat `chatId` may be given a file. */
export function takesFiles(ctx: AppContext, chatId: string): boolean {
    return ctx.state.mode === 'live' && ctx.inbox?.takes(chatId) === true;
}
/** Picks one file, any kind, and gives it to the agent in the chat open now. */
export function giveFile(ctx: AppContext): void {
    const port = ctx.inbox;
    const chatId = ctx.ui.chatId;
    if (port === undefined)
        return;
    const input = h('input', { type: 'file', 'aria-label': 'Choose a file to give Hatchling' });
    input.addEventListener('change', () => {
        const file = input.files?.[0];
        if (file !== undefined)
            port.give(chatId, file);
    });
    input.click();
}
/** The owner's name for a file, inert. */
function named(name: string): HTMLElement {
    return h('span', { class: 'given-file-name' }, revealedValue(name));
}
/** What a send says, beside its name. */
function sendWords(send: FileSend): string {
    switch (send.state) {
        case 'waiting':
            return 'Waiting its turn to go to Hatchling.';
        case 'sending':
            // Every part is in: the agent's computer may wait its turn at its disk.
            return send.sent === send.parts ? 'Waiting for the agent’s computer to take it…' : 'Giving it to Hatchling…';
        case 'refused':
            return `Not given. ${send.detail}`;
        case 'unknown':
            return send.detail;
    }
}
/** One send's row: how it goes, and what the owner may do. */
function sendRow(ctx: AppContext, port: InboxPort, send: FileSend): HTMLElement {
    const working = send.state === 'waiting' || send.state === 'sending';
    const glyph = h('span', { class: working ? 'status-glyph is-spinning' : 'status-glyph', 'aria-hidden': 'true' }, icon(working ? 'refresh' : 'warning', 14));
    // The count changes with each part: it is not read out each time.
    const count = send.state === 'sending' ? h('span', { class: 'given-file-count', 'aria-hidden': 'true' }, `${send.sent} of ${send.parts} parts`) : null;
    const actions: HTMLElement[] = [];
    if (send.state === 'refused' && send.retry)
        actions.push(button('Try again', () => port.retry(send.id), { variant: 'link' }));
    if (send.state === 'unknown')
        actions.push(button('Check status', () => { void port.refresh().then(() => ctx.openSettings('data')); }, { variant: 'link' }));
    if (!working)
        actions.push(iconButton('close', 'Dismiss', () => port.dismiss(send.id)));
    return h('div', { class: `file-import-status given-file-send ${working ? '' : 'failed'}`, role: 'status' }, glyph, h('span', { class: 'given-file-words' }, named(send.name), ` · ${sizeWords(send.bytes)}. ${sendWords(send)}`), count, ...actions);
}
/** The rows of the files being given in the chat open now, or that ended without being put. */
export function fileSendRows(ctx: AppContext): HTMLElement[] {
    const port = ctx.inbox;
    if (port === undefined)
        return [];
    return port.sends(ctx.ui.chatId).map(send => sendRow(ctx, port, send));
}
/** The file a message of the chat showed as given, and whether it is the warning after it. */
function givenBy(port: InboxPort, message: DeepReadonly<Message>): readonly [
    GivenFile,
    boolean
] | undefined {
    if (message.kind === 'notice') {
        const file = port.given(message.id);
        return file === undefined ? undefined : [file, false];
    }
    if (message.kind === 'error' && message.id.endsWith(SECRET_SUFFIX)) {
        const file = port.given(message.id.slice(0, -SECRET_SUFFIX.length));
        return file === undefined ? undefined : [file, true];
    }
    return undefined;
}
/** Asks before taking `file` out of the agent's inbox, then does. */
function removeFile(ctx: AppContext, port: InboxPort, file: GivenFile): void {
    ctx.dialogs.confirm(`Remove ${file.placedAs} from the agent’s inbox?`, 'Hatchling can’t read it after this. Anything it already read or copied from it stays where it is.', () => {
        void port.remove(file.placedAs).then(result => ctx.toast(result.message, !result.ok));
    }, { label: 'Remove file', danger: true });
}
/**
 * What the chat shows beside the broker's word that a file was given: the
 * owner's name for it, inert, and taking it out again. Nothing for any other
 * message, or with no inbox to ask.
 */
export function givenFileDetail(ctx: AppContext, message: DeepReadonly<Message>): HTMLElement | null {
    const port = ctx.inbox;
    if (port === undefined)
        return null;
    const given = givenBy(port, message);
    if (given === undefined)
        return null;
    const [file, warning] = given;
    if (file.removed)
        return h('p', { class: 'given-file-detail' }, 'Removed from the agent’s inbox since.');
    // A name the inbox never gives is no file to remove.
    if (!isInboxName(file.placedAs))
        return warning ? null : h('p', { class: 'given-file-detail' }, 'Your name for it: ', named(file.name));
    const remove = button(warning ? 'Remove it now' : 'Remove', () => removeFile(ctx, port, file), { variant: 'link' });
    if (warning)
        return h('p', { class: 'given-file-detail' }, remove);
    return h('p', { class: 'given-file-detail' }, 'Your name for it: ', named(file.name), ' ', remove);
}
