import type { AppContext } from '../../app/context.js';
import { h } from '../../components/dom.js';
import { button, iconButton } from '../../components/ui.js';
import { icon } from '../../components/icons.js';
import type { SettingsNavigation } from './settings-navigation.js';
/** A faithful form shell, deliberately incapable of capturing a real password. */
export function secureStoreTab(ctx: AppContext, nav: SettingsNavigation): HTMLElement {
    if (nav.detail === 'credential-add') {
        const url = h('input', { id: 'credential-url', type: 'url', name: 'url', autocomplete: 'off', required: true, 'aria-label': 'URL' });
        const username = h('input', { id: 'credential-username', type: 'text', name: 'username', autocomplete: 'off', 'aria-label': 'Username' });
        const password = h('input', { id: 'credential-password', type: 'password', readonly: true, autocomplete: 'off', 'aria-label': 'Password · sample only' });
        const reveal = iconButton('eye', 'Show sample password', () => { password.type = password.type === 'password' ? 'text' : 'password'; reveal.setAttribute('aria-pressed', String(password.type === 'text')); });
        reveal.setAttribute('aria-pressed', 'false');
        const row = (label: string, id: string, control: HTMLElement): HTMLElement => h('div', { class: 'credential-row' }, h('label', { for: id }, label), control);
        const submit = h('button', { type: 'submit', class: 'button primary', disabled: true }, 'Add');
        const form = h('form', { class: 'credential-form', autocomplete: 'off' }, h('div', { class: 'settings-group credential-fields' }, row('URL', url.id, url), row('Username', username.id, username), row('Password', password.id, h('div', { class: 'credential-password' }, password, reveal))), h('div', { class: 'dialog-actions' }, button('Cancel', nav.back), submit), h('p', { class: 'credential-safety' }, 'Local preview only. Password entry is disabled; use the sample below. Only the site label is saved.'), button('Use sample credential', () => { url.value = 'https://example.com'; username.value = 'demo-user'; password.value = 'sample-password'; submit.disabled = false; }, { variant: 'link' }));
        url.addEventListener('input', () => { submit.disabled = !url.validity.valid || !password.value; });
        form.addEventListener('submit', event => {
            event.preventDefault();
            if (!url.validity.valid || !password.value)
                return;
            // Neither username nor password crosses the adapter/event-log boundary.
            let host = '';
            try {
                const parsed = new URL(url.value);
                if (parsed.protocol !== 'https:' || parsed.username || parsed.password)
                    throw new Error('Invalid URL');
                host = parsed.hostname;
            }
            catch {
                ctx.toast('Use an HTTPS site URL without credentials.', true);
                return;
            }
            void ctx.run({ type: 'secure.add', service: host }).then(result => {
                if (result.ok) {
                    password.value = '';
                    username.value = '';
                    nav.back();
                }
            });
        });
        // Unless a field already has the focus: a late frame mustn't take it
        // mid-typing.
        requestAnimationFrame(() => {
            if (!form.contains(document.activeElement))
                url.focus({ preventScroll: true });
        });
        return form;
    }
    const root = h('div', { class: 'settings-stack secure-store-stack' }, h('div', { class: 'secure-store-intro' }, h('p', {}, 'Securely store passwords for your agent to use.'), button('Add', () => nav.open('credential-add'), { variant: 'primary' })));
    if (ctx.state.secureItems.length) {
        root.append(h('div', { class: 'settings-group reference-list' }, ...ctx.state.secureItems.map(service => h('div', { class: 'reference-setting-row' }, icon('lock', 18), h('span', { class: 'setting-row-copy' }, service, h('span', { class: 'setting-description' }, 'Sample reference · no password stored')), button('Remove', () => ctx.dialogs.confirm('Remove protected reference?', 'Remove this local site label? No real credential is affected.', () => { void ctx.run({ type: 'secure.remove', service }).then(nav.refresh); }, { label: 'Remove', danger: true }), { variant: 'ghost' })))));
    }
    return root;
}
