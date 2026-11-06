// The Data section of Settings on a real install: how to back up, restore and
// start over, as commands to copy into a terminal on this machine.
import type { AppContext } from '../../app/context.js';
import { h } from '../../components/dom.js';
import { button } from '../../components/ui.js';
import { KEEP_COMMANDS, RESET_COMMANDS } from '../../session/data-commands.js';
import type { DataCommand } from '../../session/data-commands.js';
function commandRow(ctx: AppContext, item: DataCommand): HTMLElement {
    const copy = (): void => {
        void navigator.clipboard.writeText(item.command).then(() => ctx.toast('Copied. Paste it in a terminal on this machine.'), () => ctx.toast('That couldn’t be copied. Select the command instead.', true));
    };
    return h('div', { class: 'data-command' }, h('div', {}, h('strong', {}, item.title), h('p', { class: 'data-description' }, item.note), h('code', {}, item.command)), button('Copy', copy, { variant: 'ghost' }));
}
function commandGroup(ctx: AppContext, title: string, items: readonly DataCommand[]): HTMLElement {
    return h('section', {}, h('h3', { class: 'settings-section-title' }, title), h('div', { class: 'settings-group' }, ...items.map((item) => commandRow(ctx, item))));
}
/** Back up, restore and start over. */
export function liveData(ctx: AppContext): HTMLElement[] {
    return [
        h('p', { class: 'settings-note' }, 'These run in a terminal on this machine, with sudo, so no agent can reach them. Clearing the agent doesn’t change who can open your vault.'),
        commandGroup(ctx, 'Keep a copy', KEEP_COMMANDS),
        commandGroup(ctx, 'Start over', RESET_COMMANDS),
    ];
}
