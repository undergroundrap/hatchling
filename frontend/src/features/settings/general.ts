import type { AppContext } from '../../app/context.js';
import type { Theme, IconName, InterfaceSize } from '../../protocol/models.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, selectField } from '../../components/ui.js';
import { accents } from './appearance.js';
import { editIdentity } from '../context/identity.js';
import { liveUsage } from './usage.js';
function usage(label: string, used: number, note: string, suffix = ''): HTMLElement {
    return h('div', { class: 'usage-meter' }, h('div', { class: 'split-row' }, h('span', {}, label), h('span', { class: 'quota-used' }, `${used}% used${suffix}`)), h('p', {}, note), h('meter', { min: 0, max: 100, value: used, 'aria-label': label, title: 'Synthetic sample quota. Not an actual account balance.' }));
}
export function generalTab(ctx: AppContext, refresh: () => void): HTMLElement {
    const root = h('div', { class: 'settings-stack general-stack' });
    const account = h('button', { class: 'settings-group account-card', type: 'button' }, icon('people', 22), h('span', { class: 'row-copy' }, h('span', {}, 'Hatchling account'), h('span', { class: 'text-small muted-on-card' }, 'Local profile and personal details')), icon('arrow', 15));
    account.addEventListener('click', () => editIdentity(ctx));
    const sampleUsage = (): HTMLElement => h('section', { class: 'general-section' }, h('h3', {}, 'Usage'), h('div', { class: 'settings-group usage-group' }, usage('Free plan', 100, 'Weekly limit resets on Sep 28'), usage('Additional tokens', 29, 'Never expires', ' (709M tokens left)'), button('Upgrade', () => {
        const body = h('div', { class: 'stack' }, h('p', {}, 'These are synthetic sample usage meters. This frontend has no billing service and will not start a purchase or subscription.'), h('p', { class: 'muted' }, 'The connected adapter can supply real plan options later.'));
        ctx.dialogs.open('Plan details · scripted demo', body, 'confirm-dialog');
    }, { variant: 'link', className: 'upgrade-link' })));
    // Connected to Hatchling, what the model really used, as the broker counted
    // it: never a sample quota shown as though it were the owner's.
    root.append(account, ctx.state.mode === 'live' ? liveUsage(ctx, refresh) : sampleUsage());
    const language = h('button', { class: 'settings-group general-language', type: 'button' }, h('span', {}, 'Language'), icon('chevron', 15));
    language.addEventListener('click', () => {
        const choices = selectField('Language', 'language', [{ value: 'English', label: 'English' }], 'English');
        const body = h('div', { class: 'stack' }, choices, h('p', { class: 'muted text-small' }, 'English is the only bundled interface translation.'));
        const ref = ctx.dialogs.open('Language', body, 'confirm-dialog');
        body.append(button('Done', () => { void ctx.run({ type: 'settings.language', value: 'English' }, true).then(() => { ref.close(); refresh(); }); }, { variant: 'primary' }));
    });
    root.append(language);
    const modes: readonly {
        id: Theme;
        label: string;
        icon: IconName;
    }[] = [{ id: 'light', label: 'Light mode', icon: 'sun' }, { id: 'dark', label: 'Dark mode', icon: 'moon' }, { id: 'system', label: 'System mode', icon: 'device' }];
    const modeGroup = h('div', { class: 'appearance-modes', role: 'radiogroup', 'aria-label': 'Appearance mode' });
    for (const mode of modes) {
        const control = h('button', { id: `mode-${mode.id}`, class: `appearance-mode ${mode.id === ctx.state.settings.theme ? 'selected' : ''}`, type: 'button', role: 'radio', 'aria-checked': String(mode.id === ctx.state.settings.theme), 'aria-label': mode.label, 'data-tooltip': mode.label }, icon(mode.icon, 16));
        control.addEventListener('click', () => { void ctx.run({ type: 'settings.theme', value: mode.id }, true).then(refresh); });
        modeGroup.append(control);
    }
    const colors = h('div', { class: 'appearance-colors', role: 'radiogroup', 'aria-label': 'Theme color' });
    for (const choice of accents) {
        const selected = ctx.state.settings.accent === choice.id;
        const swatch = h('button', { id: `accent-${choice.id}`, type: 'button', class: `color-swatch ${choice.id === 'avatar' ? 'avatar-swatch' : ''} ${selected ? 'selected' : ''}`, style: `--swatch: ${choice.color}`, role: 'radio', 'aria-checked': String(selected), 'aria-label': choice.label, 'data-tooltip': choice.label, tabindex: selected ? 0 : -1 }, choice.id === 'avatar' ? h('img', { src: ctx.state.settings.avatar || './media/rail-avatar.png', alt: '' }) : null, selected ? h('span', { class: 'swatch-check', 'aria-hidden': 'true' }, icon('check', 9)) : null);
        swatch.addEventListener('click', () => { void ctx.run({ type: 'settings.accent', value: choice.id }, true).then(refresh); });
        colors.append(swatch);
    }
    for (const group of [modeGroup, colors])
        group.addEventListener('keydown', (event) => {
            if (!['ArrowRight', 'ArrowLeft', 'ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key))
                return;
            const choices = [...group.querySelectorAll<HTMLButtonElement>('[role=radio]')];
            const index = choices.findIndex((node) => node === document.activeElement);
            const next = event.key === 'Home' ? 0 : event.key === 'End' ? choices.length - 1 : (index + (['ArrowRight', 'ArrowDown'].includes(event.key) ? 1 : -1) + choices.length) % choices.length;
            const target = choices[next];
            if (target) {
                event.preventDefault();
                target.focus();
                target.click();
            }
        });
    root.append(h('section', { class: 'general-section' }, h('h3', {}, 'Appearance'), h('div', { class: 'settings-group appearance-card' }, h('div', { class: 'appearance-row' }, h('span', {}, 'Mode'), modeGroup), h('div', { class: 'appearance-row theme-color-row' }, h('span', {}, 'Theme color'), colors))));
    const sizeGroup = h('div', { class: 'interface-size-group', role: 'radiogroup', 'aria-label': 'Interface size' });
    const sizes: readonly {
        value: InterfaceSize;
        label: string;
        detail: string;
    }[] = [
        { value: 'compact', label: 'Compact', detail: 'Original spacing' },
        { value: 'comfortable', label: 'Comfortable', detail: 'Default · 112%' },
        { value: 'large', label: 'Large', detail: 'Larger text · 125%' },
    ];
    for (const size of sizes) {
        const selected = size.value === ctx.state.settings.interfaceSize;
        const control = h('button', { class: 'interface-size-option', id: `size-${size.value}`, type: 'button', role: 'radio', 'aria-checked': String(selected), 'aria-label': size.label, tabindex: selected ? 0 : -1 }, h('strong', {}, size.label), h('small', {}, size.detail));
        control.addEventListener('click', () => { void ctx.run({ type: 'settings.interface-size', value: size.value }, true).then(refresh); });
        control.addEventListener('keydown', event => {
            if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key))
                return;
            event.preventDefault();
            const index = sizes.findIndex(option => option.value === size.value);
            const next = event.key === 'Home' ? 0 : event.key === 'End' ? sizes.length - 1 : (index + (event.key === 'ArrowLeft' ? -1 : 1) + sizes.length) % sizes.length;
            sizeGroup.querySelector<HTMLButtonElement>(`#size-${sizes[next]?.value}`)?.click();
        });
        sizeGroup.append(control);
    }
    root.append(h('section', { class: 'general-section interface-size-section' }, h('h3', {}, 'Interface size'), sizeGroup, h('p', { class: 'interface-size-description' }, 'Bigger text and controls without changing browser zoom. Compact keeps the earlier layout. Phone layouts remain fitted to the screen.')));
    root.append(h('p', { class: 'settings-note' }, `${ctx.state.mode === 'live' ? '' : 'Local scripted account. Usage and reset dates are reference fixtures, not live entitlements. '}White is the default theme color; Match my avatar uses this agent’s supplied avatar palette.`));
    return root;
}
