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
    return 'On this install, the agent\'s rooms are held by process limits alone: this machine gives Hatchling no cgroup to bound a room\'s memory and CPU as a whole. sudo hatchling-broker doctor says why, and what would change it.';
}
export function securityTab(ctx: AppContext): HTMLElement {
    const setup = ctx.setup?.current();
    const notices = [onThisInstall(setup), roomsOnThisInstall(setup)].filter((said): said is string => said !== null);
    return h('div', { class: 'settings-stack' }, ...notices.map(said => h('div', { class: 'notice' }, said)), markdown(YOUR_SECURITY));
}
