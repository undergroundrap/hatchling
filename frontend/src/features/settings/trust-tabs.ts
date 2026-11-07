import type { AppContext } from '../../app/context.js';
import type { Channel, Command, DeepReadonly, Device, IconName } from '../../protocol/models.js';
import { channelAction, currency, maskCard } from '../../presentation/format.js';
import { h, valueOf } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, field, infoRow, pill } from '../../components/ui.js';
export function secureHandoff(ctx: AppContext, name: string, command: Command, refresh: () => void): void {
    const body = h('div', { class: 'stack' }, h('div', { class: 'secure-handshake' }, icon('shield', 40), h('h3', {}, `Continue securely with ${name}`), h('p', {}, 'The provider’s secure page is the only place to sign in. hatchling receives a protected reference, not your password.')), h('div', { class: 'notice' }, icon('lock', 20), 'Local handoff preview. No external deep link was invented and no account or credential is collected.'));
    const ref = ctx.dialogs.open('Secure connection handoff', body);
    body.append(button('Complete sample handoff', () => {
        void ctx.run(command).then((result) => {
            if (result.ok) {
                ref.close();
                refresh();
            }
        });
    }, { variant: 'primary', icon: 'check' }));
}
export function walletExamples(ctx: AppContext, refresh: () => void): HTMLElement {
    const root = h('div', { class: 'settings-stack' }, h('p', { class: 'settings-intro' }, 'Payment details stay on the provider’s secure page. Every purchase needs your review.'), h('div', { class: 'spending-limit settings-group' }, h('div', { class: 'split-row' }, h('span', {}, 'Today’s sample spending limit'), h('strong', {}, currency(ctx.state.spendingLimit))), h('meter', { min: 0, max: ctx.state.spendingLimit, value: ctx.state.spentToday, 'aria-label': 'Daily spending used' }), h('p', { class: 'muted text-small' }, `${currency(ctx.state.spentToday)} used · ${currency(ctx.state.spendingLimit - ctx.state.spentToday)} available. Checked again before submission.`)));
    for (const provider of ctx.state.providers.filter(item => item.sampleOnly)) {
        const group = h('section', { class: 'settings-group wallet-provider' }, h('header', { class: 'split-row' }, h('h3', {}, provider.name), pill(provider.state.replaceAll('_', ' '))));
        if (provider.state === 'connected') {
            if (provider.methods.length)
                for (const method of provider.methods)
                    group.append(infoRow(maskCard(method), method.fee ? `${currency(method.fee)} additional routing fee` : 'No additional routing fee', 'wallet'));
            else
                group.append(h('p', { class: 'muted text-small' }, 'Connected, but there are no payment methods yet.'), button('Open secure add-card preview', () => secureHandoff(ctx, provider.name, { type: 'wallet.add-method', id: provider.id }, refresh), { icon: 'plus', variant: 'primary' }));
        }
        else
            group.append(button(provider.state === 'reauth_required' ? 'Reconnect securely' : 'Connect wallet', () => secureHandoff(ctx, provider.name, { type: provider.state === 'reauth_required' ? 'wallet.reauth' : 'wallet.connect', id: provider.id }, refresh), { icon: 'lock', variant: 'primary' }));
        root.append(group);
    }
    root.append(button('Try a purchase review', () => { ctx.dialogs.closeAll(); void ctx.run({ type: 'demo.scenario', scenario: 'purchase' }); ctx.navigate('chat'); }, { icon: 'cart' }), h('p', { class: 'settings-note' }, 'Never enter a card number or security code into chat. An uncertain payment stops automation; it is never retried.'));
    return root;
}
export function channelsTab(ctx: AppContext, refresh: () => void): HTMLElement {
    const root = h('div', { class: 'settings-stack' }, h('p', { class: 'settings-intro' }, 'A dedicated conversation in each app. Replies and scheduled deliveries stay in that conversation.'));
    for (const channel of ctx.state.channels)
        root.append(channelPanel(ctx, channel, refresh));
    root.append(h('p', { class: 'settings-note' }, 'Only the linked account’s own messages are eligible. Identity failures are rejected. Messenger Companion is separate: reading and searching an account never authorizes sending a message.'));
    return root;
}
function channelPanel(ctx: AppContext, channel: DeepReadonly<Channel>, refresh: () => void): HTMLElement {
    const action = channelAction(channel);
    const group = h('section', { class: 'settings-group channel-provider', 'data-channel-state': channel.state }, h('header', { class: 'split-row' }, h('h3', {}, channel.name), pill(channel.state.replaceAll('_', ' '), channel.state === 'linked' && channel.polling ? 'success' : '')), h('p', { class: 'muted text-small' }, channel.lastEvent));
    if (channel.state === 'linked') {
        group.append(h('p', { class: `polling-health ${channel.polling ? '' : 'needs-attention'}` }, icon(channel.polling ? 'check' : 'warning', 15), channel.polling ? `Listening · approximately every ${channel.interval}` : 'Listening stopped · nothing will be delivered'));
        group.append(h('div', { class: 'action-row' }, button('View side chat', () => { ctx.dialogs.closeAll(); ctx.navigate('chat', channel.chatId); }, { icon: 'chat' }), channel.name === 'WhatsApp' ? button('Open chat handoff preview', () => {
            ctx.dialogs.open('WhatsApp chat handoff', h('div', { class: 'stack' }, h('p', {}, 'A linked WhatsApp channel can provide an open-chat deep link. The live adapter supplies that link; this demo does not invent one.'), button('View local conversation', () => { ctx.dialogs.closeAll(); ctx.navigate('chat', channel.chatId); }, { variant: 'primary' })));
        }, { icon: 'link' }) : null, !channel.polling ? button('Restart sample listener', () => { void ctx.run({ type: 'channel.restart', id: channel.id }).then(refresh); }, { icon: 'refresh' }) : null, button('Disconnect', () => ctx.dialogs.confirm(`Disconnect ${channel.name}?`, 'Stop polling immediately and deliver one goodbye to this channel. Its durable transcript remains available.', () => { void ctx.run({ type: 'channel.disconnect', id: channel.id }).then(refresh); }, { label: 'Disconnect', danger: true }), { variant: 'danger' })));
    }
    else if (channel.state === 'unlinked' || channel.state === 'link_pending')
        group.append(button(channel.state === 'link_pending' ? 'Continue existing handoff' : 'Connect securely', () => {
            const handoff = (): void => secureHandoff(ctx, channel.name, { type: 'channel.finish', id: channel.id }, refresh);
            if (channel.state === 'link_pending')
                handoff();
            else
                void ctx.run({ type: 'channel.begin', id: channel.id }, true).then((result) => {
                    if (result.ok) {
                        refresh();
                        handoff();
                    }
                });
        }, { variant: 'primary', icon: 'link' }));
    else
        group.append(button(action.label, () => { void ctx.run({ type: 'channel.check', id: channel.id }).then(refresh); }, { icon: 'refresh' }));
    return group;
}
export function devicesTab(ctx: AppContext, refresh: () => void): HTMLElement {
    const root = h('div', { class: 'settings-stack' }, h('p', { class: 'settings-intro' }, 'Your devices, their capabilities, and exactly where an action will run.'));
    for (const device of ctx.state.devices)
        root.append(h('div', { class: 'settings-group' }, infoRow(device.name, `${device.reachable ? 'Reachable' : 'Offline'} · ${device.lastSeen}${device.origin ? ' · this device' : ''}`, 'device', button('Details', () => deviceDetail(ctx, device, refresh), { variant: 'ghost' }))));
    root.append(button('Pair a device', () => {
        const form = h('form', { class: 'stack' }, h('div', { class: 'pairing-code' }, h('span', { class: 'eyebrow' }, 'SAMPLE PAIRING CODE'), h('strong', {}, 'HATCH–2048')), h('p', { class: 'muted' }, 'Code-flow preview. A live device must verify a fresh, expiring code. This fixed demo code cannot pair real hardware.'), field('Device name', 'name', '', { placeholder: 'My phone', required: true }), h('button', { class: 'button primary', type: 'submit' }, 'Pair sample device'));
        const ref = ctx.dialogs.open('Pair a device', form);
        form.addEventListener('submit', (event) => {
            event.preventDefault();
            void ctx.run({ type: 'device.pair', name: valueOf(form, 'name') }).then((result) => {
                if (result.ok) {
                    ref.close();
                    refresh();
                }
            });
        });
    }, { variant: 'primary', icon: 'plus' }), h('p', { class: 'settings-note' }, 'Synced content is untrusted data, not an instruction. Photos and notifications never automatically post into chat; only genuinely useful opportunities surface.'));
    return root;
}
function deviceDetail(ctx: AppContext, device: DeepReadonly<Device>, refresh: () => void): void {
    const body = h('div', { class: 'stack' }, infoRow(device.name, device.reachable ? 'Reachable · explicitly targeted' : 'Offline · no silent fallback', 'device'), h('h3', {}, 'Advertised capabilities'));
    const ref = ctx.dialogs.open('Device details', body);
    for (const capability of device.capabilities)
        body.append(button(capability, () => ctx.dialogs.confirm(`Run on ${device.name}?`, `${capability} will run only on ${device.name}. The action will fail rather than silently switch to another device.`, () => { void ctx.run({ type: 'device.command', id: device.id, capability }); }, { label: 'Run on this device' }), { icon: 'play' }));
    if (!device.origin)
        body.append(button('Remove device', () => ctx.dialogs.confirm(`Remove ${device.name}?`, 'Its advertised capabilities will no longer be available for routing.', () => { void ctx.run({ type: 'device.remove', id: device.id }).then(() => { ref.close(); refresh(); }); }, { danger: true, label: 'Remove device' }), { icon: 'trash', variant: 'danger' }));
}
export const settingIcons: Record<string, IconName> = { general: 'settings', security: 'shieldLock', connectors: 'grid', wallet: 'wallet', secure: 'key', permissions: 'hand', channels: 'chat', devices: 'device', data: 'lock', help: 'help', legal: 'shield' };
