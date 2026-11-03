import type { AppContext } from '../../app/context.js';
import type { IconName } from '../../protocol/models.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button } from '../../components/ui.js';
import { currency, maskCard } from '../../presentation/format.js';
import { secureHandoff, walletExamples } from './trust-tabs.js';
import type { SettingsNavigation } from './settings-navigation.js';
const marks: Record<string, string> = {
    'browser': './media/providers/browser.png',
    'github': './media/providers/github.png',
    'instagram': './media/providers/instagram.png',
    'calendly': './media/providers/calendly.png',
    'facebook': './media/providers/facebook.png',
    'plaid': './media/providers/plaid.png',
    'function': './media/providers/function.png',
    'gmail': './media/providers/gmail.png',
    'link': './media/providers/link.png',
    'shop-reference': './media/providers/shop-reference.png'
};
export function providerMark(id: string, fallback: IconName): HTMLElement {
    return h('span', { class: 'provider-mark', 'aria-hidden': 'true' }, marks[id] ? h('img', { src: marks[id], alt: '', width: 28, height: 28 }) : icon(fallback, 21));
}
export function connectorsTab(ctx: AppContext, nav: SettingsNavigation): HTMLElement {
    // Connected to Hatchling, no connector exists yet: say so, rather than
    // offer sample services to connect.
    if (ctx.state.mode === 'live' && !ctx.state.connectors.length)
        return h('div', { class: 'settings-stack' }, h('p', { class: 'settings-intro' }, `No apps are connected to ${ctx.state.settings.name}. Connectors for mail, calendar and GitHub are coming, each with only the permissions you grant it, and each shown here with what it may do.`));
    if (nav.detail === 'connector') {
        const item = ctx.state.connectors.find(entry => entry.id === nav.id);
        if (!item)
            return h('p', {}, 'This connector is no longer available.');
        const root = h('div', { class: 'settings-stack' }, h('div', { class: 'settings-group connector-detail' }, providerMark(item.id, item.icon), h('h3', {}, item.name), h('p', {}, item.scope), h('p', { class: 'muted text-small' }, `Status: ${item.state}. This is a scripted account, not a real connection.`)));
        root.append(item.state === 'connected' ? button('Disconnect', () => ctx.dialogs.confirm(`Disconnect ${item.name}?`, 'Remove the local connection and its grant? No external account is affected.', () => { void ctx.run({ type: 'connector.disconnect', id: item.id }).then(nav.back); }, { label: 'Disconnect', danger: true }), { variant: 'danger' }) : button('Continue linking', () => secureHandoff(ctx, item.name, { type: 'connector.finish', id: item.id }, nav.refresh), { variant: 'primary' }));
        return root;
    }
    const root = h('div', { class: 'settings-stack connectors-stack' });
    const input = h('input', { type: 'search', placeholder: 'Search connectors', 'aria-label': 'Search connectors', autocomplete: 'off' });
    const results = h('div', { class: 'connector-results' });
    root.append(h('label', { class: 'connector-search' }, icon('search', 17), input), results);
    const draw = (): void => {
        const nodes: HTMLElement[] = [];
        for (const connected of [true, false]) {
            const items = ctx.state.connectors.filter(item => (item.state === 'connected') === connected && item.name.toLowerCase().includes(input.value.trim().toLowerCase()));
            if (!items.length)
                continue;
            const group = h('div', { class: 'settings-group reference-list' });
            for (const item of items) {
                const copy = h('span', { class: 'setting-row-copy' }, h('span', {}, item.name), item.custom ? h('span', { class: 'setting-description' }, 'Custom') : null);
                if (connected) {
                    const row = h('button', { type: 'button', class: 'reference-setting-row connector-row', 'aria-label': `Manage ${item.name}`, 'data-connector': item.id }, providerMark(item.id, item.icon), copy, icon('chevron', 16));
                    row.addEventListener('click', () => nav.open('connector', item.id));
                    group.append(row);
                }
                else {
                    const action = button(item.state === 'pending' ? 'Continue' : 'Connect', () => {
                        void ctx.run({ type: 'connector.begin', id: item.id }, true).then(result => {
                            if (result.ok)
                                secureHandoff(ctx, item.name, { type: 'connector.finish', id: item.id }, nav.refresh);
                        });
                    }, { variant: 'link' });
                    action.setAttribute('aria-label', `${item.state === 'pending' ? 'Continue linking' : 'Connect'} ${item.name}`);
                    group.append(h('div', { class: 'reference-setting-row connector-row', 'data-connector': item.id }, providerMark(item.id, item.icon), copy, action));
                }
            }
            nodes.push(h('section', {}, h('h3', { class: 'settings-section-title' }, connected ? 'Connected' : 'Available'), group));
        }
        if (!nodes.length)
            nodes.push(h('p', { class: 'setting-empty', role: 'status' }, 'No connectors found.'));
        results.replaceChildren(...nodes);
    };
    input.addEventListener('input', draw);
    draw();
    return root;
}
export function walletTab(ctx: AppContext, nav: SettingsNavigation): HTMLElement {
    // Connected to Hatchling, nothing can be bought: say so plainly.
    if (ctx.state.mode === 'live' && !ctx.state.providers.length)
        return h('div', { class: 'settings-stack reference-wallet' }, h('p', { class: 'settings-intro' }, `${ctx.state.settings.name} can't make purchases: payments aren't part of this build. When they come, each purchase will need your yes on its exact quote, and nothing will be charged without it.`));
    if (nav.detail === 'wallet-examples')
        return walletExamples(ctx, nav.refresh);
    if (nav.detail === 'wallet-provider') {
        const provider = ctx.state.providers.find(item => item.id === nav.id);
        if (!provider)
            return h('p', {}, 'This payment provider is no longer available.');
        const root = h('div', { class: 'settings-stack' }, h('p', {}, 'Card details stay on the provider’s secure page. Every purchase requires review.'), h('div', { class: 'settings-group wallet-provider' }, ...provider.methods.map(method => h('div', { class: 'reference-setting-row' }, icon('wallet', 20), h('span', {}, maskCard(method))))));
        root.append(button('Add payment method', () => secureHandoff(ctx, provider.name, { type: 'wallet.add-method', id: provider.id }, nav.refresh), { variant: 'primary' }), h('p', { class: 'text-small muted' }, `${currency(ctx.state.spendingLimit - ctx.state.spentToday)} available in the sample allowance. Checked again before every submission.`));
        return root;
    }
    const group = h('div', { class: 'settings-group reference-list wallet-methods' });
    for (const provider of ctx.state.providers.filter(item => !item.sampleOnly)) {
        const action = provider.comingSoon ? h('span', { class: 'coming-soon', 'aria-disabled': 'true' }, 'Coming soon') : button(provider.state === 'connected' ? 'Manage' : provider.state === 'reauth_required' ? 'Reconnect' : 'Add', () => {
            if (provider.state === 'connected')
                nav.open('wallet-provider', provider.id);
            else
                secureHandoff(ctx, provider.name, { type: provider.state === 'reauth_required' ? 'wallet.reauth' : 'wallet.connect', id: provider.id }, nav.refresh);
        }, { variant: 'link' });
        group.append(h('div', { class: `reference-setting-row wallet-method ${provider.comingSoon ? 'unavailable' : ''}` }, providerMark(provider.id, 'wallet'), h('span', { class: 'setting-row-copy' }, provider.name), action));
    }
    const root = h('div', { class: 'settings-stack reference-wallet' }, h('p', { class: 'settings-intro' }, `Add payment methods to allow ${ctx.state.settings.name} to make secure purchases and transactions for you.`), h('section', {}, h('h3', { class: 'settings-section-title' }, 'Payment methods'), group));
    root.append(h('details', { class: 'settings-sample-options' }, h('summary', {}, 'Local payment demonstration'), h('p', { class: 'text-small muted' }, 'No real payment provider is connected. Use the sample wallet to exercise quote approval, fees, and uncertain outcomes.'), button('Open sample wallets', () => nav.open('wallet-examples'), { variant: 'link' })));
    return root;
}
