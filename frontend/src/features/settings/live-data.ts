// The Data section of Settings on a real install: how to back up, restore and
// start over, as commands to copy into a terminal on this machine.
import type { AppContext } from '../../app/context.js';
import { h } from '../../components/dom.js';
import { button } from '../../components/ui.js';
import { KEEP_COMMANDS, KEY_COMMANDS, MODEL_COMMANDS, RESET_COMMANDS } from '../../session/data-commands.js';
import { onWindows } from '../../session/data-commands.js';
import type { DataCommand } from '../../session/data-commands.js';
function copyButton(ctx: AppContext, command: string): HTMLElement {
    const copy = (): void => {
        void navigator.clipboard.writeText(command).then(() => ctx.toast(onWindowsSetup(ctx) ? 'Copied. Open PowerShell from the Start menu, and paste it there.' : 'Copied. Paste it in a terminal on this machine.'), () => ctx.toast('That couldn’t be copied. Select the command instead.', true));
    };
    return button('Copy', copy, { variant: 'ghost' });
}
/** Whether the Windows setup made this install. */
function onWindowsSetup(ctx: AppContext): boolean {
    const setup = ctx.setup?.current();
    return setup?.kind === 'read' && setup.setup.windowsSetup;
}
/** `item` as it's typed on this install. */
function forHere(ctx: AppContext, item: DataCommand): DataCommand {
    return onWindowsSetup(ctx) ? onWindows(item) : item;
}
function commandWell(ctx: AppContext, item: DataCommand): HTMLElement {
    return h('div', { class: 'command-well' }, h('code', {}, item.command), copyButton(ctx, item.command));
}
/** One command to copy, and what it does. */
export function commandRow(ctx: AppContext, given: DataCommand): HTMLElement {
    const item = forHere(ctx, given);
    return h('div', { class: 'data-command' }, h('strong', {}, item.title), h('p', { class: 'data-description' }, item.note), commandWell(ctx, item));
}
/** One command to copy, where what's around it says what it does. */
export function commandLine(ctx: AppContext, given: DataCommand): HTMLElement {
    return commandWell(ctx, forHere(ctx, given));
}
function commandGroup(ctx: AppContext, title: string, items: readonly DataCommand[]): HTMLElement {
    return h('section', {}, h('h3', { class: 'settings-section-title' }, title), h('div', { class: 'settings-group' }, ...items.map((item) => commandRow(ctx, item))));
}
/** Setting Hatchling up: where the model runs, and who can open the vault. */
export function liveSetup(ctx: AppContext): HTMLElement[] {
    const checklist = (): void => { ctx.dialogs.closeAll(); ctx.navigate('setup'); };
    return [
        h('div', { class: 'settings-group' }, h('div', { class: 'data-row' }, h('div', {}, h('strong', {}, 'Setup checklist'), h('p', { class: 'data-description' }, 'What’s done, and what’s left.')), button('Open', checklist, { variant: 'ghost' }))),
        h('p', { class: 'settings-note' }, `${onWindowsSetup(ctx) ? 'You don’t need these for everyday use. To protect your vault or make a backup, open the Start menu and type its name. If you want one of these, open PowerShell from the Start menu yourself, paste it there and press Enter. You don’t need Linux or sudo.' : 'Run these in a terminal you opened yourself.'} Anything secret you type there goes to Hatchling, and to nothing else. Set up one model, or both: with both, you switch between them above.`),
        commandGroup(ctx, 'Where your model runs', MODEL_COMMANDS),
        commandGroup(ctx, 'Who can open your vault', KEY_COMMANDS),
    ];
}
/** Back up, restore and start over. */
export function liveData(ctx: AppContext): HTMLElement[] {
    return [
        h('p', { class: 'settings-note' }, `${onWindowsSetup(ctx) ? 'To run one, open PowerShell from the Start menu yourself, paste it there and press Enter. They work from outside Hatchling' : 'These run in a terminal on this machine, with sudo'}, so no agent can reach them. Clearing the agent doesn’t change who can open your vault.`),
        commandGroup(ctx, 'Keep a copy', KEEP_COMMANDS),
        commandGroup(ctx, 'Start over', RESET_COMMANDS),
    ];
}
