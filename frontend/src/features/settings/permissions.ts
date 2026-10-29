import type { AppContext } from '../../app/context.js';
import { h, valueOf } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, textareaField, toggle } from '../../components/ui.js';
import type { IconName, WebsitePolicy } from '../../protocol/models.js';
import type { SettingsNavigation } from './settings-navigation.js';
import { providerMark } from './connections.js';
function navigationRow(label: string, count: number | null, action: () => void): HTMLElement {
    const node = h('button', { type: 'button', class: 'reference-setting-row permission-navigation' }, h('span', { class: 'setting-row-copy' }, label), count === null ? null : h('span', { class: 'permission-count' }, String(count)), icon('chevron', 16));
    node.addEventListener('click', action);
    return node;
}
export function permissionsTab(ctx: AppContext, nav: SettingsNavigation): HTMLElement {
    if (nav.detail === 'websites')
        return websites(ctx, nav);
    if (nav.detail === 'network')
        return network(ctx, nav);
    if (nav.detail === 'signed-in')
        return signedIn(ctx, nav);
    if (nav.detail === 'permission-connectors') {
        return h('div', { class: 'settings-stack' }, h('h3', { class: 'settings-section-title' }, 'Connected services'), h('div', { class: 'settings-group reference-list' }, ...ctx.state.connectors.filter(item => item.state === 'connected').map(item => h('div', { class: 'reference-setting-row' }, providerMark(item.id, item.icon), h('span', { class: 'setting-row-copy' }, item.name, h('span', { class: 'setting-description' }, item.scope)), icon('check', 16)))), h('p', { class: 'settings-note' }, 'Reading is limited to the displayed scope. Writes, purchases and credential fills require their own approval.'));
    }
    if (nav.detail === 'permission-artifacts')
        return h('div', { class: 'settings-stack' }, h('div', { class: 'settings-group' }, h('p', {}, ctx.state.publications.length ? `${ctx.state.publications.length} approved local snapshots.` : 'No published artifacts.'), h('p', { class: 'settings-note' }, 'Every publish and every update requires a fresh approval. Interactive apps cannot publish.')), ...ctx.state.publications.map(item => navigationRow(item.title, null, () => { ctx.dialogs.closeAll(); ctx.navigate('published', item.id); })));
    if (nav.detail === 'permission-scheduled')
        return h('div', { class: 'settings-stack' }, h('p', {}, 'Recurring work requires approval of the exact schedule. A one-time yes authorizes one run only.'), ...ctx.state.jobs.map(item => h('div', { class: 'settings-group' }, h('strong', {}, item.title), h('p', { class: 'setting-description' }, item.schedule, ' · ', item.health))), button('Open scheduled work', () => { ctx.dialogs.closeAll(); ctx.navigate('schedule'); }, { variant: 'link' }));
    if (nav.detail === 'permission-advanced')
        return advanced(ctx, nav);
    const root = h('div', { class: 'settings-stack permissions-stack' });
    const defaults = (id: string, heading: string, descriptions: readonly [
        string,
        string
    ]): HTMLElement => {
        const item = ctx.state.permissions.find(permission => permission.id === id);
        const group = h('div', { class: 'settings-group reference-list permission-choices', role: 'radiogroup', 'aria-label': heading });
        for (const [index, mode] of ['ask-for-some', 'always-ask'].entries()) {
            const value = mode === 'always-ask' ? 'always-ask' : 'ask-for-some';
            const selected = item?.mode === value;
            const row = h('button', { type: 'button', class: 'reference-setting-row permission-choice', role: 'radio', 'aria-checked': selected, tabindex: selected ? 0 : -1 }, h('span', { class: 'setting-row-copy' }, index === 0 ? 'Ask for some actions' : 'Always ask', h('span', { class: 'setting-description' }, descriptions[index] ?? '')), selected ? icon('check', 19) : null);
            const apply = (): void => { void ctx.run({ type: 'permission.set', id, mode: value }, true).then(nav.refresh); };
            row.addEventListener('click', apply);
            row.addEventListener('keydown', event => {
                if (!['ArrowDown', 'ArrowUp', 'ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key))
                    return;
                event.preventDefault();
                const target = event.key === 'Home' ? 'ask-for-some' : event.key === 'End' ? 'always-ask' : value === 'always-ask' ? 'ask-for-some' : 'always-ask';
                void ctx.run({ type: 'permission.set', id, mode: target }, true).then(() => { nav.refresh(); document.querySelector<HTMLElement>(`[aria-label="${heading}"] [aria-checked="true"]`)?.focus({ preventScroll: true }); });
            });
            group.append(row);
        }
        return h('section', {}, h('h3', { class: 'settings-section-title' }, heading), group);
    };
    root.append(defaults('connectors', 'Connector defaults', ['Before actions that may share your information or make important changes', 'Before any action']), defaults('websites', 'Web access defaults', ['When your information may be shared or the website is unfamiliar', 'Ask before accessing any website']));
    const manage = h('div', { class: 'settings-group reference-list' }, navigationRow('Connectors', ctx.state.connectors.filter(item => item.state === 'connected').length, () => nav.open('permission-connectors')), navigationRow('Websites', ctx.state.websitePermissions.filter(item => item.policy === 'allow').length, () => nav.open('websites')), ctx.state.mode === 'live' ? navigationRow('Signed-in sites', ctx.state.signedInSites.length, () => nav.open('signed-in')) : null, navigationRow('Artifacts', ctx.state.publications.length, () => nav.open('permission-artifacts')), navigationRow('Scheduled tasks', ctx.state.jobs.filter(item => item.health === 'ok').length, () => nav.open('permission-scheduled')), navigationRow('Direct network protocols', null, () => nav.open('network')));
    root.append(h('section', {}, h('h3', { class: 'settings-section-title' }, 'Manage permissions'), manage), button('Advanced approval settings', () => nav.open('permission-advanced'), { variant: 'link' }), button('Reset permission defaults', () => ctx.dialogs.confirm('Reset permission defaults?', 'Remove website grants, disable direct network protocols and standing challenge permission, and restore confirmation defaults? Purchase approvals remain mandatory.', () => { void ctx.run({ type: 'permission.reset' }).then(nav.refresh); }, { label: 'Reset defaults' }), { variant: 'ghost' }));
    return root;
}
function websites(ctx: AppContext, nav: SettingsNavigation): HTMLElement {
    if (ctx.state.mode === 'live')
        return liveWebsites(ctx, nav);
    const group = h('div', { class: 'settings-group reference-list website-permissions' });
    for (const item of ctx.state.websitePermissions) {
        const control = h('select', { 'aria-label': `Permission for ${item.host}`, class: 'website-policy' }, ...(['allow', 'ask', 'block'] as const).map(value => h('option', { value, selected: item.policy === value }, value === 'allow' ? 'Allow' : value === 'ask' ? 'Ask' : 'Block')));
        control.addEventListener('change', () => {
            const policy: WebsitePolicy = control.value === 'allow' ? 'allow' : control.value === 'block' ? 'block' : 'ask';
            control.value = item.policy;
            const apply = (): void => { void ctx.run({ type: 'permission.website', host: item.host, policy }, true).then(nav.refresh); };
            if (policy === 'allow')
                ctx.dialogs.confirm(`Allow ${item.host}?`, 'Allow this site in the local permission list. Sensitive actions still need approval. No real website will be contacted.', apply, { label: 'Allow website' });
            else
                apply();
        });
        group.append(h('div', { class: 'reference-setting-row website-permission' }, h('span', { class: 'website-icon' }, icon('globe', 18)), h('span', { class: 'setting-row-copy' }, item.host), control));
    }
    return h('div', { class: 'settings-stack' }, h('section', {}, h('h3', { class: 'settings-section-title' }, "Websites you've allowed"), group));
}
/** The sites Hatchling's broker keeps: added only by Allow always on a request, revoked here. */
function liveWebsites(ctx: AppContext, nav: SettingsNavigation): HTMLElement {
    const group = h('div', { class: 'settings-group reference-list website-permissions' });
    for (const item of ctx.state.websitePermissions) {
        const revoke = button('Revoke', () => ctx.dialogs.confirm(`Revoke ${item.host}?`, 'The agent asks you again the next time it wants a page on this site. A task you already allowed it for keeps it until the task ends.', () => { void ctx.run({ type: 'permission.website', host: item.host, policy: 'ask' }, true).then(nav.refresh); }, { label: 'Revoke site' }), { variant: 'ghost' });
        group.append(h('div', { class: 'reference-setting-row website-permission' }, h('span', { class: 'website-icon' }, icon('globe', 18)), h('span', { class: 'setting-row-copy' }, item.host), revoke));
    }
    const empty = h('p', { class: 'settings-note' }, 'No sites yet. When the agent asks to read a site, choose Allow always on its request to keep it here.');
    return h('div', { class: 'settings-stack' }, h('section', {}, h('h3', { class: 'settings-section-title' }, "Websites you've allowed"), ctx.state.websitePermissions.length ? group : empty));
}
/** The sites the user kept signed in for the agent: kept only from a browser they drove, signed out or deleted here. */
function signedIn(ctx: AppContext, nav: SettingsNavigation): HTMLElement {
    const group = h('div', { class: 'settings-group reference-list signed-in-sites' });
    for (const item of ctx.state.signedInSites) {
        const names = item.sites.join(', ');
        const signOut = item.signedIn ? button('Sign out', () => ctx.dialogs.confirm(`Sign out of ${names}?`, 'Hatchling drops the session it kept. The agent still opens these sites in a browser of their own, signed out, until you delete them.', () => { void ctx.run({ type: 'profile.sign-out', id: item.id }).then(nav.refresh); }, { label: 'Sign out' }), { variant: 'ghost' }) : null;
        const remove = button('Delete', () => ctx.dialogs.confirm(`Delete ${names}?`, 'Hatchling drops the session and the sites. The agent asks you again the next time it wants a page on them.', () => { void ctx.run({ type: 'profile.delete', id: item.id }).then(nav.refresh); }, { label: 'Delete', danger: true }), { variant: 'ghost' });
        group.append(h('div', { class: 'reference-setting-row website-permission' }, h('span', { class: 'website-icon' }, icon('lock', 18)), h('span', { class: 'setting-row-copy' }, names, h('span', { class: 'setting-description' }, item.signedIn ? `Signed in · kept ${item.lastUsed}` : 'Signed out')), signOut, remove));
    }
    const empty = h('p', { class: 'settings-note' }, 'No signed-in sites yet. Take control of the agent’s browser, sign in to a site, and choose Keep me signed in.');
    return h('div', { class: 'settings-stack' }, h('section', {}, h('h3', { class: 'settings-section-title' }, 'Signed-in sites'), ctx.state.signedInSites.length ? group : empty));
}
function network(ctx: AppContext, nav: SettingsNavigation): HTMLElement {
    const group = h('div', { class: 'settings-group reference-list network-permissions' });
    for (const item of ctx.state.networkPermissions) {
        const control = toggle(item.name, item.enabled, enabled => {
            const apply = (): void => { void ctx.run({ type: 'permission.network', id: item.id, enabled }, true).then(nav.refresh); };
            if (enabled) {
                const input = control.querySelector<HTMLInputElement>('input');
                if (input)
                    input.checked = false;
                ctx.dialogs.confirm(`Allow ${item.name.toLowerCase()}?`, `${item.description}. This updates a sample preference only, not a real firewall or connection.`, apply, { label: 'Allow protocol' });
            }
            else
                apply();
        }, item.description);
        control.classList.add('network-toggle');
        control.prepend(icon(item.icon, 17));
        group.append(control);
    }
    return h('div', { class: 'settings-stack' }, group);
}
function advanced(ctx: AppContext, nav: SettingsNavigation): HTMLElement {
    const root = h('div', { class: 'settings-stack' }, h('div', { class: 'settings-group' }, toggle('Standing challenge permission', ctx.state.settings.captchaStanding, value => {
        if (value)
            ctx.dialogs.confirm('Allow site challenges by default?', 'Permit one bounded attempt on allowed sites. Persistent challenges still require takeover; this never authorizes purchases or credential fills.', () => { void ctx.run({ type: 'settings.captcha', value }).then(nav.refresh); }, { label: 'Allow bounded challenges' });
        else
            void ctx.run({ type: 'settings.captcha', value }).then(nav.refresh);
    })));
    const form = h('form', { class: 'stack' }, textareaField('Allowed site names · one per line', 'allowlist', ctx.state.settings.allowlist.join('\n'), 4), h('p', { class: 'text-small muted' }, 'Demo browser tasks are additionally constrained to these sample sites. This is separate from the sample website grant list.'), h('button', { class: 'button secondary', type: 'submit' }, 'Save allowlist'));
    form.addEventListener('submit', event => { event.preventDefault(); void ctx.run({ type: 'settings.allowlist', value: valueOf(form, 'allowlist') }).then(nav.refresh); });
    root.append(form);
    for (const [id, name, glyph] of [['payments', 'Payments', 'wallet'], ['scheduled', 'Scheduled work', 'clock']] as const) {
        const permission = ctx.state.permissions.find(item => item.id === id);
        root.append(h('div', { class: 'settings-group' }, icon(glyph satisfies IconName, 18), h('strong', {}, name), h('p', {}, permission?.description ?? 'Fresh approval is required.')));
    }
    return root;
}
