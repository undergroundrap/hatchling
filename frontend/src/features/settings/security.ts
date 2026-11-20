// The Your security section: docs/your-security.md, said of this install.
import type { AppContext } from '../../app/context.js';
import { h } from '../../components/dom.js';
import { markdown } from '../../components/ui.js';
import type { SetupState } from '../../session/setup.js';
import { YOUR_SECURITY } from './your-security-text.js';
/** How this install's vault opens, in the page's words. Nothing until it's been read: a guess here would be a claim. */
export function onThisInstall(setup: SetupState | undefined): string | null {
    if (setup?.kind !== 'read')
        return null;
    switch (setup.setup.tier) {
        case 'hardware_key':
            return 'On this install, the vault opens with a security key or a backup code.';
        case 'totp':
            return 'On this install, the vault opens with authenticator codes, so a stolen disk still opens it.';
        case 'file':
            return 'On this install, the vault\'s key is in a file on the same disk, so a stolen disk opens it. Protect your vault changes that.';
    }
}
/** What holds the agent's rooms on this install, when it's the weaker way. Nothing otherwise, and nothing until it's been read. */
export function roomsOnThisInstall(setup: SetupState | undefined): string | null {
    if (setup?.kind !== 'read' || !setup.setup.processLimitsOnly)
        return null;
    return 'On this install, the agent\'s rooms are held by process limits alone: this machine gives Hatchling no cgroup to bound a room\'s memory and CPU as a whole, and the agent\'s computer and its browser can\'t run. sudo hatchling-broker doctor says why, and what would change it.';
}
/** Said when this install couldn't be read: the page below is then said of no install in particular. Nothing in the demo, and nothing while it's being read. */
export function unreadOnThisInstall(setup: SetupState | undefined): string | null {
    if (setup?.kind !== 'failed')
        return null;
    return 'Hatchling couldn\'t read how this install is protected, so nothing below is said of it in particular. sudo hatchling-broker doctor says what this install has, and what\'s off.';
}
export function securityTab(ctx: AppContext): HTMLElement {
    const setup = ctx.setup?.current();
    const notices = [unreadOnThisInstall(setup), onThisInstall(setup), roomsOnThisInstall(setup)].filter((said): said is string => said !== null);
    return h('div', { class: 'settings-stack' }, ...notices.map(said => h('div', { class: 'notice' }, said)), markdown(YOUR_SECURITY));
}
