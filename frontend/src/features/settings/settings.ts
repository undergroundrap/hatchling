import { openAgentControls } from '../../automation/bridge.js';
import { clearDocumentDrafts } from '../library/markdown-editor.js';
import { icon } from '../../components/icons.js';
import { generalTab } from './general.js';
import type { AppContext } from '../../app/context.js';
import { h, replacePreservingFocus, valueOf } from '../../components/dom.js';
import { button, field, markdown, selectField, textareaField, toggle } from '../../components/ui.js';
import { channelsTab, devicesTab, settingIcons } from './trust-tabs.js';
import { connectorsTab, walletTab } from './connections.js';
import { secureStoreTab } from './credentials.js';
import { permissionsTab } from './permissions.js';
import type { SettingsNavigation } from './settings-navigation.js';
import { iconButton } from '../../components/ui.js';
import { openShortcuts, openDownloads, openIssueReport } from '../../app/support.js';
const sections = [{ id: 'general', title: 'General' }, { id: 'connectors', title: 'Connectors' }, { id: 'wallet', title: 'Wallet' }, { id: 'secure', title: 'Secure store' }, { id: 'permissions', title: 'Permissions' }, { id: 'channels', title: 'Messaging channels' }, { id: 'devices', title: 'Devices' }, { id: 'data', title: 'Data controls' }, { id: 'help', title: 'Help & support' }, { id: 'legal', title: 'Legal info' }];
export function openSettings(ctx: AppContext, initial = 'general'): void {
    let active = sections.some((section) => section.id === initial) ? initial : 'general';
    let detail = '';
    let detailId = '';
    const history: {
        detail: string;
        id: string;
        top: number;
    }[] = [];
    const rootScroll = new Map<string, number>();
    const titles: Record<string, string> = { 'credential-add': 'Secure credentials store', 'websites': "Websites you've allowed", 'network': 'Direct network protocols', 'signed-in': 'Signed-in sites', 'permission-connectors': 'Connector permissions', 'permission-artifacts': 'Artifact permissions', 'permission-scheduled': 'Scheduled task permissions', 'permission-advanced': 'Advanced approval settings', 'wallet-examples': 'Sample wallets' };
    const selectSection = (id: string): void => {
        if (!detail)
            rootScroll.set(active, layout.querySelector('.settings-pane')?.scrollTop ?? 0);
        active = id;
        detail = '';
        detailId = '';
        history.length = 0;
        refresh(rootScroll.get(id) ?? 0);
    };
    const layout = h('div', { class: 'settings-layout' });
    const ref = ctx.dialogs.open('Settings', layout, 'settings-dialog');
    const refresh = (top?: number): void => {
        const scroll = top ?? layout.querySelector('.settings-pane')?.scrollTop ?? 0;
        const nested: SettingsNavigation = { detail, id: detailId,
            refresh: () => refresh(),
            open: (next, id = '') => { history.push({ detail, id: detailId, top: layout.querySelector('.settings-pane')?.scrollTop ?? 0 }); detail = next; detailId = id; refresh(0); },
            back: () => { const previous = history.pop(); detail = previous?.detail ?? ''; detailId = previous?.id ?? ''; refresh(previous?.top ?? 0); requestAnimationFrame(() => layout.querySelector<HTMLElement>('.settings-pane h2')?.focus({ preventScroll: true })); },
        };
        const nav = h('nav', { class: 'settings-nav', 'aria-label': 'Settings sections' }, h('h2', { class: 'settings-nav-title' }, 'Settings'), ...sections.map((section) => {
            const node = button(section.title, () => selectSection(section.id), { icon: settingIcons[section.id] ?? 'settings', variant: 'ghost', className: active === section.id ? 'active' : '' });
            if (active === section.id)
                node.setAttribute('aria-current', 'page');
            return node;
        }));
        const mobileSections = selectField('Settings section', 'settings-section', [...sections.map((section) => ({ value: section.id, label: section.title })), { value: 'reset', label: 'Reset local demo…' }], active);
        mobileSections.classList.add('settings-nav-mobile');
        mobileSections.querySelector('select')?.addEventListener('change', (event) => {
            const target = event.currentTarget;
            if (!(target instanceof HTMLSelectElement))
                return;
            if (target.value === 'reset') {
                target.value = active;
                resetDemo(ctx);
                return;
            }
            if (sections.some((section) => section.id === target.value)) {
                selectSection(target.value);
            }
        });
        nav.append(mobileSections);
        nav.append(h('div', { class: 'settings-nav-bottom' }, button('Log out', () => {
            ctx.dialogs.confirm('Leave this local workspace?', 'There is no signed-in account in this frontend. Close the workspace without deleting your local edits? Reset is available in Data controls.', () => {
                ctx.dialogs.closeAll();
                const welcome = h('div', { class: 'stack' }, h('p', {}, 'The local workspace is paused. Your saved edits are still on this device.'));
                const paused = ctx.dialogs.open('Workspace paused', welcome);
                welcome.append(button('Return to workspace', paused.close, { variant: 'primary' }));
            }, { label: 'Leave workspace' });
        }, { icon: 'logout', variant: 'ghost' })));
        const title = detail === 'connector' ? ctx.state.connectors.find(item => item.id === detailId)?.name ?? 'Connector' : detail === 'wallet-provider' ? ctx.state.providers.find(item => item.id === detailId)?.name ?? 'Payment methods' : titles[detail] ?? (active === 'secure' ? 'Secure credentials store' : sections.find(section => section.id === active)?.title ?? 'General');
        const pane = h('section', { class: 'settings-pane', 'data-scroll': 'settings-pane', 'data-section': active, 'data-detail': detail }, h('div', { class: `settings-pane-heading ${detail ? 'is-nested' : ''}` }, detail ? iconButton('chevron', 'Back in settings', nested.back, 'settings-back') : null, h('h2', { tabindex: -1 }, title)));
        switch (active) {
            case 'connectors':
                pane.append(connectorsTab(ctx, nested));
                break;
            case 'wallet':
                pane.append(walletTab(ctx, nested));
                break;
            case 'secure':
                pane.append(secureStoreTab(ctx, nested));
                break;
            case 'permissions':
                pane.append(permissionsTab(ctx, nested));
                break;
            case 'channels':
                pane.append(channelsTab(ctx, () => refresh()));
                break;
            case 'devices':
                pane.append(devicesTab(ctx, () => refresh()));
                break;
            case 'data':
                pane.append(dataTab(ctx, () => refresh()));
                break;
            case 'help':
                pane.append(helpTab(ctx));
                break;
            case 'legal':
                pane.append(legalTab());
                break;
            default: pane.append(generalTab(ctx, () => refresh()));
        }
        replacePreservingFocus(layout, nav, pane);
        pane.scrollTop = scroll;
    };
    refresh();
    ref.element.addEventListener('keydown', (event) => {
        if (event.key === 'Escape' && !detail) {
            event.preventDefault();
            event.stopPropagation();
            ref.close();
        }
    });
}
function resetDemo(ctx: AppContext): void { ctx.dialogs.confirm('Reset this local workspace?', 'This removes all demo edits, imported attachments, and local approval history from this browser, then restores the original fixtures. Export anything you need first.', () => { void ctx.run({ type: 'demo.reset' }).then(() => { ctx.dialogs.closeAll(); ctx.navigate('chat'); ctx.dock.close(); ctx.dialogs.closeAll(); clearDocumentDrafts(ctx); }); }, { label: 'Reset demo', danger: true }); }
function dataTab(ctx: AppContext, refresh: () => void): HTMLElement {
    const privacy = h('div', { class: 'settings-group privacy-card' }, icon('shield', 21), h('div', {}, h('strong', {}, 'Your privacy is important to us'), h('p', {}, 'Learn about the steps we take to keep your information private and secure.'), button('Learn more', () => {
        ctx.dialogs.open('Privacy in this local demo', h('div', { class: 'stack' }, h('p', {}, 'This frontend stores its scripted event log in your browser. No account, telemetry service, or model training endpoint is connected.'), h('p', {}, 'Import only non-sensitive memory notes. Export downloads local workspace data. Reset removes local edits and restores the examples; it does not contact or delete data from any external account.')));
    }, { variant: 'link' })));
    const importMemory = (): void => {
        const file = h('input', { type: 'file', accept: '.md,.txt,text/plain,text/markdown', 'aria-label': 'Choose memory file' });
        const form = h('form', { class: 'stack' }, h('p', { class: 'muted text-small' }, 'Review a plain-text or Markdown note before adding it to local memory. Do not include passwords, codes, tokens, or payment details.'), file, field('Title', 'title', '', { required: true }), textareaField('Memory to import', 'body', '', 8), h('button', { type: 'submit', class: 'button primary' }, 'Import note'));
        const ref = ctx.dialogs.open('Import memory', form, 'memory-import-dialog');
        file.addEventListener('change', () => {
            const selected = file.files?.[0];
            if (!selected)
                return;
            if (selected.size > 100000) {
                ctx.toast('Choose a text note smaller than 100 KB.', true);
                file.value = '';
                return;
            }
            void selected.text().then((text) => {
                const title = form.querySelector<HTMLInputElement>('[name="title"]');
                const body = form.querySelector<HTMLTextAreaElement>('[name="body"]');
                if (title)
                    title.value = selected.name.replace(/\.(md|txt)$/iu, '');
                if (body)
                    body.value = text;
            }).catch(() => ctx.toast('That file could not be read. Paste the note instead.', true));
        });
        form.addEventListener('submit', (event) => {
            event.preventDefault();
            void ctx.run({ type: 'memory.save', id: '', title: valueOf(form, 'title'), body: valueOf(form, 'body') }).then((result) => {
                if (result.ok)
                    ref.close();
            });
        });
    };
    const row = (title: string, action: () => void): HTMLElement => {
        const control = button(title, action, { variant: 'ghost', className: 'data-navigation-row' });
        control.append(icon('chevron', 16));
        return control;
    };
    return h('div', { class: 'settings-stack data-controls-stack' }, privacy, h('section', {}, h('div', { class: 'settings-group data-toggle-card' }, toggle('Help improve our AI models', ctx.state.settings.telemetry, (value) => { void ctx.run({ type: 'settings.telemetry', value }).then(refresh); })), h('p', { class: 'data-description' }, 'Preference preview only. This frontend does not send interactions or train models.')), h('div', { class: 'settings-group data-links-card' }, row('Import memory', importMemory), row('Download your agent data', () => { void ctx.run({ type: 'data.export' }); })), h('section', {}, h('div', { class: 'settings-group data-reset-card' }, button('Reset hatchling', () => resetDemo(ctx), { variant: 'danger' })), h('p', { class: 'data-description' }, 'Removes local demo edits, chat history and saved tasks, then restores the original examples.')), h('details', { class: 'data-advanced' }, h('summary', {}, 'More data options'), row('Memory & preferences', () => { ctx.dialogs.closeAll(); ctx.navigate('memory'); }), row('Recover deleted artifacts', () => { ctx.dialogs.closeAll(); ctx.navigate('library', 'trash'); })));
}
function helpTab(ctx: AppContext): HTMLElement {
    return h('div', { class: 'settings-stack' }, h('div', { class: 'settings-group support-links' }, button('Keyboard shortcuts', () => openShortcuts(ctx), { icon: 'keyboard', variant: 'ghost' }), button('Download apps', () => openDownloads(ctx), { icon: 'download', variant: 'ghost' }), button('Report an issue', () => openIssueReport(ctx), { icon: 'bug', variant: 'ghost' })), h('div', { class: 'settings-group support-links' }, ...(['browser', 'memory', 'schedule', 'notifications'] as const).map(route => button(route === 'schedule' ? 'Scheduled work' : route[0]?.toUpperCase() + route.slice(1), () => { ctx.dialogs.closeAll(); ctx.navigate(route); }, { variant: 'ghost' }))), markdown('## A small guide\n\nUse the left rail to move between Chat, Feed, Ideas, Goals, and Library. Help & support links to Browser, Memory, Scheduled work, and Notifications.\n\nUse Control/Command + Shift + D, or Open demo scenarios below, to try every acceptance scenario. Nothing leaves your browser.\n\n## Keyboard\n\n- Control or Command + K opens Search.\n- Alt + 1 through 6 switches the main areas.\n- Enter sends a chat message; Shift + Enter adds a line.\n- Escape closes a modal or open menu.\n- Tab reaches every control, including card menus.\n\n## Iterating the code\n\nThe source includes AGENTS.md, an adapter contract, deterministic fixtures, an AST linter, strict compiler checks, unit tests, and browser tests. Feature modules do not import the scripted adapter.'), button('Open demo scenarios', () => { ctx.dialogs.closeAll(); ctx.openScenarios(); }, { icon: 'play', variant: 'primary' }), button('Agent control', () => openAgentControls(ctx), { icon: 'code' }), button('Export a reproducible state', () => { void ctx.run({ type: 'data.export' }); }, { icon: 'download' }));
}
function legalTab(): HTMLElement { return h('div', { class: 'settings-stack' }, h('div', { class: 'notice' }, icon('file', 21), 'Frontend demonstration, not a hosted service'), markdown('## About this build\n\nThis implementation recreates the supplied layout direction with original sample content and the user-supplied chick avatar. Hatchling is the product name. Reference images inform interaction patterns only; sample content is synthetic.\n\n## Local data\n\nChanges are retained in this browser’s storage. Import only non-sensitive files. Shared snapshots are local previews, not externally hosted publications. Clearing browser data removes this workspace.\n\n## No real external actions\n\nPurchases, account connections, browser automation, device pairing, voice calls, and scheduled workers are simulations. The UI does not charge a card, send a message, or connect a real account.\n\n## Source and assets\n\nSource is supplied for iteration. Reference artwork remains subject to its original owner’s rights. The small demo audio, video, PDF and hand-authored interface icons are included as sample assets. No bundled system fonts are distributed.')); }
