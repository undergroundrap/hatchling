// A file the agent gave the owner, as the chat shows it beside the broker's
// notice (WO-041): that it came from the agent's computer, what opening it
// can do, and a download of the Library's copy. Never a preview, a link to
// it, or anything of it shown in the page.
import type { AppContext } from '../../app/context.js';
import type { DeepReadonly, Message } from '../../protocol/models.js';
import type { AgentFilesPort, OfferedFile } from '../../session/agent-files.js';
import { h } from '../../components/dom.js';
import { button } from '../../components/ui.js';
import { caution } from '../../session/agent-files.js';
/** Downloads `file`, and says how it went. */
export function downloadFile(ctx: AppContext, port: AgentFilesPort, file: {
    readonly id: string;
}): void {
    void port.download(file.id).then(result => ctx.toast(result.message, !result.ok));
}
/** The download, or that one is under way. */
function downloadButton(ctx: AppContext, port: AgentFilesPort, file: OfferedFile): HTMLElement {
    if (port.downloading(file.id))
        return h('span', { class: 'given-file-count', role: 'status' }, 'Downloading…');
    return button('Download', () => downloadFile(ctx, port, file), { variant: 'link' });
}
/**
 * What the chat shows beside the broker's word that the agent gave a file:
 * where it came from, what opening it can do, and Download. Nothing for any
 * other message, or with no Library to ask.
 */
export function offeredFileDetail(ctx: AppContext, message: DeepReadonly<Message>): HTMLElement | null {
    const port = ctx.agentFiles;
    if (port === undefined || message.kind !== 'notice')
        return null;
    const file = port.offered(message.id);
    if (file === undefined)
        return null;
    if (file.removed)
        return h('p', { class: 'given-file-detail' }, 'Removed from Hatchling’s Library since.');
    return h('p', { class: 'given-file-detail' }, `From the agent’s computer. ${caution(file.name)} `, downloadButton(ctx, port, file));
}
