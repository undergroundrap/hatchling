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
export function securityTab(ctx: AppContext): HTMLElement {
    const here = onThisInstall(ctx.setup?.current());
    return h('div', { class: 'settings-stack' }, ...(here ? [h('div', { class: 'notice' }, here)] : []), markdown(YOUR_SECURITY));
}
