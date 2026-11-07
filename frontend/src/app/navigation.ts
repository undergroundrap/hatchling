import { chatGroups, connectorChats } from '../features/chat/connector-chats.js';
import { sideChatDisclosure, relativeChatTime, navigationResizer, setChatNavigationOpen } from '../features/chat/chat-navigation.js';
import { openShortcuts, openDownloads, openIssueReport } from './support.js';
import type { AppContext, LibraryFilter, Route } from './context.js';
import type { IconName } from '../protocol/models.js';
import { h, valueOf } from '../components/dom.js';
import { bindIconMotion } from '../components/motion.js';
import { icon } from '../components/icons.js';
import { button, field, iconButton, menu } from '../components/ui.js';
export const primaryAreas: readonly {
    route: Route;
    name: string;
    icon: IconName;
}[] = [{ route: 'chat', name: 'Chat', icon: 'chat' }, { route: 'search', name: 'Search', icon: 'search' }, { route: 'feed', name: 'Feed', icon: 'feed' }, { route: 'ideas', name: 'Ideas', icon: 'idea' }, { route: 'goals', name: 'Goals', icon: 'goal' }, { route: 'library', name: 'Library', icon: 'library' }];
export function renderRail(ctx: AppContext): HTMLElement {
    const nav = h('nav', { class: 'icon-rail', 'aria-label': 'Product areas' }, h('div', { class: 'rail-primary' }, ...primaryAreas.map((area) => {
        const node = iconButton(area.icon, area.name, () => area.route === 'search' ? ctx.openSearch() : ctx.navigate(area.route, area.route === 'chat' ? ctx.ui.chatId : undefined), ctx.ui.route === area.route ? 'active' : '');
        node.id = `rail-${area.route}`;
        if (area.route === 'chat')
            node.addEventListener('pointerenter', () => {
                if (ctx.ui.route === 'chat' && !ctx.state.settings.keepChatPanelVisible && !ctx.ui.navOpen && window.innerWidth > 800) {
                    ctx.ui.navOpen = true;
                    ctx.render();
                }
            });
        bindIconMotion(node, ctx.ui.route === area.route && ctx.ui.lastMotionRoute !== ctx.ui.route);
        if (area.route === 'chat' && ctx.state.chats.some((chat) => chat.unread && !chat.archived))
            node.append(h('span', { class: 'rail-unread', 'aria-label': 'Unread conversations' }));
        if (ctx.ui.route === area.route)
            node.setAttribute('aria-current', 'page');
        return node;
    })));
    if (ctx.ui.route !== 'chat' || ctx.dock.keepsRail) {
        const brand = h('button', { class: 'rail-avatar', 'aria-label': ctx.state.mode === 'scripted' ? 'hatchling · scripted workspace' : 'hatchling workspace', 'data-tooltip': ctx.state.mode === 'scripted' ? 'hatchling · scripted workspace' : 'hatchling workspace', type: 'button' }, h('img', { src: ctx.state.settings.avatar || './media/rail-avatar.png', alt: '' }));
        brand.addEventListener('click', () => ctx.openSettings('general'));
        nav.prepend(brand);
    }
    const more = menu('More', [
        { label: 'Keyboard shortcuts', icon: 'keyboard', action: () => openShortcuts(ctx) },
        { label: ctx.state.mode === 'live' ? 'New versions' : 'Download apps', icon: 'download', action: () => openDownloads(ctx) },
        { label: 'Report an issue', icon: 'bug', action: () => openIssueReport(ctx) },
        { label: 'Settings', icon: 'settings', separatorBefore: true, action: () => ctx.openSettings() },
    ]);
    more.classList.add('rail-more');
    const summary = more.querySelector('summary');
    summary?.replaceChildren(icon('menu', 24));
    summary?.setAttribute('id', 'rail-more');
    summary?.setAttribute('data-tooltip', 'More');
    const actions = more.querySelectorAll('.menu-popover > button');
    actions[0]?.append(h('kbd', {}, 'Ctrl+/'));
    actions[0]?.setAttribute('aria-label', 'Keyboard shortcuts');
    actions[3]?.append(h('kbd', {}, 'Ctrl+,'));
    actions[3]?.setAttribute('aria-label', 'Settings');
    nav.append(h('div', { class: 'rail-bottom' }, ctx.state.mode === 'live' ? null : iconButton('download', 'Downloads', () => ctx.navigate('library', 'documents')), more));
    ctx.ui.lastMotionRoute = ctx.ui.route;
    return nav;
}
export function renderSidebar(ctx: AppContext): HTMLElement {
    const sidebar = h('aside', { id: 'chat-navigation', class: `navigation-panel ${ctx.ui.navOpen ? 'open' : ''}`, 'aria-label': ctx.ui.route === 'library' ? 'Library navigation' : 'Chat navigation' });
    if (ctx.ui.route === 'library') {
        const search = h('input', { type: 'search', id: 'library-search', class: 'library-search-input', placeholder: 'Search', value: ctx.ui.navSearch, 'aria-label': 'Search library' });
        search.addEventListener('input', () => { ctx.ui.navSearch = search.value; ctx.render(); });
        sidebar.append(h('label', { class: 'library-search' }, icon('search', 16), search));
        const groups: readonly {
            title: string;
            items: readonly {
                id: LibraryFilter;
                name: string;
                icon: IconName;
            }[];
        }[] = [
            { title: 'Artifacts', items: [{ id: 'all', name: 'All artifacts', icon: 'library' }, { id: 'documents', name: 'Documents', icon: 'file' }, { id: 'web', name: 'Web artifacts', icon: 'globe' }] },
            { title: 'Media', items: [{ id: 'images', name: 'Images', icon: 'image' }, { id: 'videos', name: 'Videos', icon: 'video' }, { id: 'audio', name: 'Podcasts', icon: 'audio' }] },
        ];
        for (const group of groups)
            sidebar.append(h('div', { class: 'sidebar-group' }, h('h2', {}, group.title), ...group.items.map((item) => button(item.name, () => ctx.navigate('library', item.id), { icon: item.icon, variant: 'ghost', className: `nav-item ${ctx.ui.libraryFilter === item.id ? 'active' : ''}` }))));
        const files = button('System files', () => ctx.navigate('library', 'system'), { icon: 'folder', variant: 'ghost', className: `nav-item system-nav ${ctx.ui.libraryFilter === 'system' ? 'active' : ''}` });
        sidebar.append(h('div', { class: 'library-nav-bottom' }, ctx.state.mode === 'live' ? null : files, menu('Library options', [{ label: 'Trash', icon: 'trash', action: () => ctx.navigate('library', 'trash') }, { label: 'Hide navigation', icon: 'panel', action: () => { ctx.ui.navOpen = false; ctx.render(); } }])));
    }
    else {
        const search = h('button', { id: 'chat-search', class: 'nav-search-trigger', type: 'button', 'aria-label': 'Search chats', 'aria-keyshortcuts': 'Control+k Meta+k' }, icon('search', 16), h('span', {}, 'Search'));
        search.addEventListener('click', ctx.openSearch);
        const options = sideChatOptions(ctx);
        sidebar.append(h('div', { class: 'nav-search-row' }, search, options), button('Main chat', () => ctx.navigate('chat', 'main'), { variant: 'ghost', className: `nav-item main-chat-link ${ctx.ui.chatId === 'main' ? 'active' : ''}` }), h('div', { class: 'side-chat-heading' }, sideChatDisclosure(ctx), iconButton('plus', 'New side chat', () => { ctx.ui.sideChatsOpen = true; chatForm(ctx); })));
        const chats = chatGroups(ctx.state.chats, ctx.ui.showArchived).side.filter(chat => chat.title.toLowerCase().includes(ctx.ui.navSearch.toLowerCase()));
        const list = h('div', { class: 'side-chat-list', 'data-scroll': 'side-chats' });
        for (const chat of chats) {
            const entry = button(chat.title, () => ctx.navigate('chat', chat.id), { variant: 'ghost', className: 'chat-nav-title' });
            if (chat.id === ctx.ui.chatId)
                entry.setAttribute('aria-current', 'page');
            const unsent = ctx.delivery.count(chat.id);
            if (unsent)
                entry.append(h('span', { class: `chat-outbox-count${ctx.delivery.attention(chat.id) ? ' needs-attention' : ''}`, 'aria-label': `${unsent} unsent messages` }, icon(ctx.delivery.attention(chat.id) ? 'warning' : 'clock', 12), String(unsent)));
            entry.title = chat.title;
            entry.id = `chat-link-${encodeURIComponent(chat.id)}`;
            const updated = relativeChatTime(chat.updatedAt, ctx.state.clock);
            const recency = h('time', { class: 'chat-recency', datetime: chat.updatedAt ?? '', 'aria-label': updated ? `Last active ${updated} ago` : 'No recorded time' }, updated);
            list.append(h('div', { class: `chat-nav-row ${chat.id === ctx.ui.chatId ? 'active' : ''}` }, chat.pinned ? icon('pin', 12) : null, entry, recency, chat.unread ? h('span', { class: 'unread-dot', 'aria-label': 'Unread' }) : null, menu(`Manage chat ${chat.title}`, [{ label: 'Rename chat', icon: 'edit', action: () => chatForm(ctx, chat.id) }, { label: chat.pinned ? 'Unpin' : 'Pin', icon: 'pin', action: () => { void ctx.run({ type: 'chat.toggle-pin', id: chat.id }, true); } }, { label: chat.archived ? 'Restore chat' : 'Archive chat', icon: 'folder', action: () => { void ctx.run({ type: 'chat.archive', id: chat.id }); } }, { label: 'Delete chat', icon: 'trash', danger: true, separatorBefore: true, disabled: chat.channelId !== null || unsent > 0 || ctx.approvals.pending(chat.id).length > 0 || ctx.ui.preparingChats.has(chat.id) || [...ctx.ui.fileImports.values()].some(item => item.chatId === chat.id), action: () => ctx.dialogs.confirm('Delete this side chat?', 'The transcript and any draft in this chat are removed from this demo. Connector conversations are retained separately.', () => {
                        void ctx.run({ type: 'chat.delete', id: chat.id }).then(result => {
                            if (result.ok && ctx.ui.chatId === chat.id)
                                ctx.navigate('chat', 'main');
                        });
                    }, { label: 'Delete chat', danger: true }) }])));
        }
        const accordion = h('div', { id: 'side-chat-accordion', class: `side-chat-accordion ${ctx.ui.sideChatsOpen ? '' : 'is-collapsed'}` }, h('div', { class: 'side-chat-accordion-inner' }, list));
        accordion.inert = !ctx.ui.sideChatsOpen;
        if (!chats.length)
            list.append(h('p', { class: 'sidebar-empty' }, ctx.ui.showArchived ? 'No archived side chats.' : 'Start a side chat to keep a topic separate.'));
        // A real install has no side chats or connected apps to offer yet.
        if (ctx.state.mode === 'live' && !chats.length)
            sidebar.querySelector('.side-chat-heading')?.remove();
        else
            sidebar.append(accordion, ...(ctx.state.mode === 'live' ? [] : [connectorChats(ctx)]));
    }
    if (ctx.ui.route === 'chat') {
        const main = sidebar.querySelector('.main-chat-link');
        if (main) {
            main.id = 'chat-link-main';
            main.setAttribute('aria-current', ctx.ui.chatId === 'main' ? 'page' : 'false');
        }
        const mainPending = ctx.delivery.count('main');
        if (mainPending)
            main?.append(h('span', { class: `chat-outbox-count${ctx.delivery.attention('main') ? ' needs-attention' : ''}`, 'aria-label': `${mainPending} unsent messages` }, icon(ctx.delivery.attention('main') ? 'warning' : 'clock', 12), String(mainPending)));
        if (ctx.state.chats.find((chat) => chat.id === 'main')?.unread)
            main?.append(h('span', { class: 'unread-dot', 'aria-label': 'Unread' }));
        sidebar.addEventListener('pointerleave', () => {
            if (window.innerWidth > 800 && !ctx.state.settings.keepChatPanelVisible && !sidebar.contains(document.activeElement) && !sidebar.querySelector('details[open]')) {
                setChatNavigationOpen(ctx, false);
            }
        });
    }
    if (ctx.ui.route === 'chat')
        sidebar.append(navigationResizer(ctx));
    return sidebar;
}
function chatForm(ctx: AppContext, id?: string): void {
    const chat = ctx.state.chats.find((item) => item.id === id);
    const form = h('form', { class: 'stack' }, field('Chat name', 'title', chat?.title ?? '', { placeholder: 'A thought worth keeping', required: true }), h('button', { class: 'button primary', type: 'submit' }, chat ? 'Save name' : 'Create side chat'));
    const ref = ctx.dialogs.open(chat ? 'Rename side chat' : 'New side chat', form);
    form.addEventListener('submit', (event) => {
        event.preventDefault();
        void ctx.run(chat ? { type: 'chat.rename', id: chat.id, title: valueOf(form, 'title') } : { type: 'chat.create', title: valueOf(form, 'title') }).then((result) => {
            if (result.ok) {
                ref.close();
                ctx.navigate('chat', result.id ?? chat?.id ?? 'main');
            }
        });
    });
}
function sideChatOptions(ctx: AppContext): HTMLElement {
    const options = menu('Side chat options', [
        { label: 'Keep chat panel visible', icon: 'check', action: () => { void ctx.run({ type: 'settings.chat-panel', value: !ctx.state.settings.keepChatPanelVisible }, true); } },
        ...(ctx.state.mode === 'live' ? [] : [{ label: 'Show archived chats', icon: 'folder' as const, separatorBefore: true, action: () => { ctx.ui.showArchived = !ctx.ui.showArchived; ctx.render(); } }]),
    ]);
    options.classList.add('side-chat-options');
    options.querySelector('summary')?.setAttribute('data-tooltip', 'Side chat options');
    options.querySelector('.menu-popover')?.setAttribute('role', 'menu');
    const controls = options.querySelectorAll<HTMLButtonElement>('.menu-popover > button');
    for (const [index, control] of controls.entries()) {
        const checked = index === 0 ? ctx.state.settings.keepChatPanelVisible : ctx.ui.showArchived;
        control.setAttribute('role', 'menuitemcheckbox');
        control.setAttribute('aria-checked', String(checked));
        if (index === 0 && !checked)
            control.querySelector('svg')?.classList.add('unchecked-mark');
        if (index === 1 && checked)
            control.append(icon('check', 13));
    }
    return options;
}
