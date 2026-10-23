import { writeRouteHash } from './route-location.js';
import { ContentLoading } from './content-loading.js';
import { ApprovalReview } from '../features/approvals/review.js';
import type { TaskApprovalPort } from '../session/task-approvals.js';
import type { SignInPort } from '../session/sign-ins.js';
import { ChatDelivery } from '../features/chat/delivery.js';
import type { ChatSessionPort } from '../session/chat-session.js';
import { ComposerDrafts } from '../features/chat/drafts.js';
import { resolveChatId } from '../features/chat/chat-header.js';
import { setChatNavigationOpen } from '../features/chat/chat-navigation.js';
import { animateChatNavigation } from '../features/chat/navigation-motion.js';
import { openBrowserSession } from '../features/browser/session.js';
import { icon } from '../components/icons.js';
import { setContextOpen } from '../features/context/presence.js';
import { openShortcuts } from './support.js';
import { ArtifactDock } from '../components/dock.js';
import { installTooltips, installFloatingMenus } from '../components/floating.js';
import { openMemoryViewer } from '../features/library/memory-viewer.js';
import { disposeIconMotion } from '../components/motion.js';
import { applyAppearance } from '../features/settings/appearance.js';
import type { AppContext, LibraryFilter, Route, UiState } from './context.js';
import type { ArtifactFormat, Command, CommandResult, DownloadPayload, ProtocolAdapter } from '../protocol/models.js';
import { h, replacePreservingFocus } from '../components/dom.js';
import { DialogManager } from '../components/dialog.js';
import { button, emptyState, iconButton, markdown } from '../components/ui.js';
import { renderRail, renderSidebar, primaryAreas } from './navigation.js';
import { openScenarios } from './scenarios.js';
import { renderChat } from '../features/chat/chat.js';
import { openActivity } from '../features/activity/activity.js';
import { renderContextPanel } from '../features/context/panel.js';
import { renderLibrary, createArtifactDialog } from '../features/library/library.js';
import { openViewer } from '../features/library/viewer.js';
import { renderGoals } from '../features/goals/goals.js';
import { renderIdeas } from '../features/ideas/ideas.js';
import { renderFeed } from '../features/feed/feed.js';
import { renderMemory } from '../features/memory/memory.js';
import { renderSchedule, renderNotifications } from '../features/schedule/schedule.js';
import { renderBrowser } from '../features/browser/browser.js';
import { openSearch } from '../features/search/search.js';
import { openSettings } from '../features/settings/settings.js';
function isRoute(value: string): value is Route { return ['chat', 'search', 'feed', 'ideas', 'goals', 'library', 'browser', 'memory', 'schedule', 'notifications', 'published'].includes(value); }
const routeTitles: Readonly<Record<Route, string>> = { chat: 'Chat', search: 'Search', feed: 'Feed', ideas: 'Ideas', goals: 'Goals', library: 'Library', browser: 'Browser', memory: 'Memory', schedule: 'Schedule', notifications: 'Notifications', published: 'Published' };
function isLibraryFilter(value: string): value is LibraryFilter { return ['all', 'documents', 'web', 'images', 'videos', 'audio', 'system', 'trash'].includes(value); }
/** Composition/controller: UI view state is local; product mutations cross the adapter only. */
export class App implements AppContext {
    readonly ui: UiState = { sideChatsOpen: true, connectorChatsOpen: true, fileImports: new Map(), preparingChats: new Set(), navigationWidth: 240, route: 'chat', chatId: 'reference', libraryFilter: 'all', search: '', navSearch: '', draft: '', navOpen: window.innerWidth > 800, pageNavOpen: false, lastMotionRoute: null, contextOpen: window.innerWidth > 1100, contextTab: 'identity', contextScroll: new Map(), recentChatIds: ['reference', 'chat-0', 'main', 'chat-1', 'chat-2', 'chat-3'], libraryLayout: 'grid', librarySort: 'opened', selectionMode: false, selectedArtifacts: new Set<string>(), goalFilter: 'active', goalExamples: false, ideaExamples: false, feedPromptOpen: false, systemSort: 'asc', ideaFilter: 'suggested', memoryFilter: 'profile', scheduleFilter: 'all', showArchived: false, attachmentNames: [], systemFolder: '', systemExpanded: new Set<string>(), systemSelected: '', publicationId: '' };
    readonly delivery: ChatDelivery;
    readonly contentLoading: ContentLoading;
    readonly approvals: ApprovalReview;
    readonly dialogs: DialogManager;
    get signIns(): SignInPort | undefined {
        return this.adapter.signIns;
    }
    readonly dock: ArtifactDock;
    private readonly disposeTooltips: () => void;
    private readonly disposeMenus: () => void;
    private artifactOrigin: string | null = null;
    private dockNavigation: boolean | null = null;
    private readonly unsubscribe: () => void;
    private readonly chatScrolls = new Map<string, {
        top: number;
        bottom: boolean;
    }>();
    private readonly drafts = new ComposerDrafts();
    private readQueued = false;
    private readonly colorScheme = window.matchMedia('(prefers-color-scheme: light)');
    private lastViewportWidth = window.innerWidth;
    private lastViewportHeight = window.innerHeight;
    constructor(readonly adapter: ProtocolAdapter & {
        readonly chatSession?: ChatSessionPort;
        readonly taskApprovals?: TaskApprovalPort;
        readonly signIns?: SignInPort;
    }, private readonly root: HTMLElement, dialogRoot: HTMLElement, private readonly toastRoot: HTMLElement) {
        this.dialogs = new DialogManager(dialogRoot, this.acknowledgeVisible);
        this.dock = new ArtifactDock(dialogRoot, this.onDockChange);
        this.contentLoading = new ContentLoading(this.render);
        this.delivery = new ChatDelivery(adapter, this.render);
        this.approvals = new ApprovalReview(adapter.taskApprovals, this.render);
        for (const draft of this.delivery.savedDrafts())
            this.drafts.save(draft.chatId, draft.text, draft.attachments);
        const recovered = this.drafts.read(this.ui.chatId);
        this.ui.draft = recovered.text;
        this.ui.attachmentNames = [...recovered.attachments];
        this.disposeTooltips = installTooltips();
        this.disposeMenus = installFloatingMenus();
        this.unsubscribe = adapter.subscribe(this.render);
        window.addEventListener('beforeunload', this.beforeUnload);
        window.addEventListener('online', this.networkChanged);
        window.addEventListener('offline', this.networkChanged);
        window.addEventListener('hashchange', this.readLocation);
        window.addEventListener('popstate', this.readLocation);
        window.addEventListener('resize', this.onResize);
        window.addEventListener('keydown', this.onKeyboard);
        document.addEventListener('click', this.closeMenus);
        this.ui.navOpen = window.innerWidth > 800 && this.state.settings.keepChatPanelVisible;
        this.colorScheme.addEventListener('change', this.render);
        document.addEventListener('visibilitychange', this.acknowledgeVisible);
        this.root.addEventListener('transitionend', this.onDockTransition);
        window.addEventListener('focus', this.acknowledgeVisible);
        this.readLocation();
        if (!navigator.onLine)
            this.networkChanged();
    }
    get state(): ReturnType<ProtocolAdapter['getSnapshot']> { return this.adapter.getSnapshot(); }
    render = (): void => {
        this.saveComposer();
        this.delivery.sync();
        if (this.ui.route === 'chat' && !this.state.chats.some(chat => chat.id === this.ui.chatId)) {
            const available = resolveChatId(this.state.chats, this.ui.chatId);
            if (available) {
                this.selectChat(available);
                writeRouteHash('#chat/main', true);
            }
        }
        const previousChat = this.root.querySelector<HTMLElement>('.conversation-toolbar')?.dataset['chatId'];
        const previousSidebar = this.root.querySelector('.route-chat .navigation-panel') !== null;
        applyAppearance(this.state.settings);
        const characterName = this.state.settings.name.trim() || 'hatchling';
        document.title = `${routeTitles[this.ui.route]} — ${characterName}`;
        const contextualSidebar = this.ui.route === 'chat' || this.ui.route === 'library';
        const needsSidebar = contextualSidebar || this.ui.pageNavOpen;
        const sidebarOpen = contextualSidebar ? this.ui.navOpen : this.ui.pageNavOpen;
        const showContext = !this.dock.isOpen && this.ui.contextOpen && ['chat', 'browser'].includes(this.ui.route);
        const shell = h('div', { class: `app-shell route-${this.ui.route} ${needsSidebar && sidebarOpen ? 'with-navigation' : ''} ${showContext ? 'with-context' : ''} ${this.ui.route === 'chat' && !this.state.settings.keepChatPanelVisible ? 'transient-navigation' : ''}` }, renderRail(this));
        if (needsSidebar && sidebarOpen)
            shell.append(renderSidebar(this));
        const main = h('main', { id: 'main', class: 'main-region', 'data-scroll': this.ui.route, tabindex: -1 });
        if (this.ui.route !== 'chat')
            main.append(h('div', { class: 'mobile-page-toolbar' }, iconButton('panel', 'Toggle navigation', () => {
                if (contextualSidebar)
                    this.ui.navOpen = !this.ui.navOpen;
                else
                    this.ui.pageNavOpen = !this.ui.pageNavOpen;
                this.render();
            }), h('span', {}, 'hatchling'), iconButton('settings', 'Open settings', () => this.openSettings())));
        switch (this.ui.route) {
            case 'chat':
                main.append(renderChat(this));
                break;
            case 'search':
                main.append(renderChat(this));
                break;
            case 'library':
                main.append(renderLibrary(this));
                break;
            case 'goals':
                main.append(renderGoals(this));
                break;
            case 'ideas':
                main.append(renderIdeas(this));
                break;
            case 'feed':
                main.append(renderFeed(this));
                break;
            case 'browser':
                main.append(renderBrowser(this));
                break;
            case 'memory':
                main.append(renderMemory(this));
                break;
            case 'schedule':
                main.append(renderSchedule(this));
                break;
            case 'notifications':
                main.append(renderNotifications(this));
                break;
            case 'published': {
                const publication = this.state.publications.find((item) => item.id === this.ui.publicationId);
                main.append(publication ? h('div', { class: 'page published-page reading-wide' }, h('div', { class: 'notice' }, 'Approved static snapshot · local preview, not a public hosted link'), h('header', { class: 'page-header' }, h('h1', {}, publication.title)), h('p', { class: 'muted text-small' }, `Approved version ${publication.version}. Later edits do not change this snapshot.`), markdown(publication.content), button('Back to library', () => this.navigate('library'))) : emptyState('file', 'This snapshot is not in this local workspace.', 'Shared demo links only work in the browser that approved them.'));
                break;
            }
        }
        shell.append(main);
        if (showContext) {
            const backdrop = h('button', { class: 'context-backdrop', type: 'button', 'aria-label': 'Dismiss agent panel' });
            backdrop.addEventListener('click', () => setContextOpen(this, false));
            shell.append(backdrop, renderContextPanel(this));
        }
        if (needsSidebar && sidebarOpen) {
            const backdrop = h('button', { class: 'navigation-backdrop', type: 'button', 'aria-label': 'Dismiss navigation' });
            backdrop.addEventListener('click', () => { this.ui.pageNavOpen = false; setChatNavigationOpen(this, false, true); });
            shell.append(backdrop);
        }
        const previous = this.root.querySelector<HTMLElement>('.chat-scroll');
        const previousKey = previous?.dataset['scroll'];
        if (previous && previousKey)
            this.chatScrolls.set(previousKey, { top: previous.scrollTop, bottom: previous.scrollHeight - previous.scrollTop - previous.clientHeight < 40 });
        disposeIconMotion(this.root);
        replacePreservingFocus(this.root, shell);
        animateChatNavigation(this.root, previousChat, previousSidebar);
        const stream = this.root.querySelector<HTMLElement>('.chat-scroll');
        const streamKey = stream?.dataset['scroll'];
        if (stream && streamKey) {
            const saved = this.chatScrolls.get(streamKey);
            stream.scrollTop = !saved || saved.bottom ? stream.scrollHeight : saved.top;
            const jump = this.root.querySelector<HTMLElement>('.jump-to-latest');
            const update = (): void => {
                // A reflow scroll is not the user leaving the bottom of the conversation.
                if (window.innerWidth !== this.lastViewportWidth || window.innerHeight !== this.lastViewportHeight)
                    return;
                const bottom = stream.scrollHeight - stream.scrollTop - stream.clientHeight < 60;
                this.chatScrolls.set(streamKey, { top: stream.scrollTop, bottom });
                if (jump)
                    jump.hidden = bottom;
            };
            stream.addEventListener('scroll', update, { passive: true });
            stream.addEventListener('scroll', this.acknowledgeVisible, { passive: true });
            update();
        }
        const contextBody = this.root.querySelector<HTMLElement>('.context-body');
        if (contextBody)
            contextBody.scrollTop = this.ui.contextScroll.get(this.ui.contextTab) ?? 0;
        this.dock.syncAccessibility();
        this.acknowledgeVisible();
    };
    navigate = (route: Route, detail?: string): void => {
        const previousRoute = this.ui.route;
        const previousChat = this.ui.chatId;
        const previousSidebar = this.ui.navOpen;
        if (this.dock.keepsRail && this.dock.coversConversation)
            this.dock.close();
        if (route === 'search') {
            this.openSearch();
            return;
        }
        if (route !== 'chat' || (detail && detail !== this.ui.chatId))
            this.artifactOrigin = null;
        if (route === 'chat')
            this.selectChat(detail ?? 'main');
        this.ui.route = route;
        this.ui.pageNavOpen = false;
        if (route === 'library')
            this.ui.libraryFilter = detail?.startsWith('system') ? 'system' : detail && isLibraryFilter(detail) ? detail : 'all';
        if (route === 'library' && detail?.startsWith('system')) {
            this.ui.systemFolder = detail.slice(7);
            this.ui.navSearch = '';
        }
        if (route === 'published')
            this.ui.publicationId = detail ?? '';
        if (window.innerWidth <= 800)
            this.ui.navOpen = false;
        else if (route === 'library' || route === 'chat')
            this.ui.navOpen = route === 'library' || (this.state.settings.keepChatPanelVisible && (previousRoute !== 'chat' || previousSidebar));
        const resolvedDetail = route === 'chat' ? this.ui.chatId : detail;
        const hash = `#${route}${resolvedDetail ? `/${encodeURIComponent(resolvedDetail)}` : ''}`;
        writeRouteHash(hash);
        this.render();
        if (route === 'chat' && (previousRoute !== 'chat' || previousChat !== this.ui.chatId))
            document.getElementById('chat-navigation-toggle')?.focus({ preventScroll: true });
    };
    run = async (command: Command, silent = false): Promise<CommandResult> => {
        const loadingToken = command.type === 'feed.generate' ? this.contentLoading.begin('feed', this.state.posts.length > 0) : null;
        if (command.type === 'feed.generate' && loadingToken === null)
            return { ok: false, message: 'Feed generation is already in progress.' };
        try {
            const result = await this.adapter.execute(command);
            if (loadingToken !== null)
                this.contentLoading.finish('feed', loadingToken, result.ok, result.message);
            if (!result.ok || (!silent && result.message))
                this.toast(result.message, !result.ok);
            if (result.ok && result.download)
                this.download(result.download);
            return result;
        }
        catch {
            const message = 'That action could not finish. Your last saved state is still available.';
            if (loadingToken !== null)
                this.contentLoading.finish('feed', loadingToken, false, message);
            this.toast(message, true);
            return { ok: false, message };
        }
    };
    toast = (message: string, error = false): void => {
        if (!message)
            return;
        const toast = h('div', { class: `toast ${error ? 'error' : ''}`, role: error ? 'alert' : 'status' }, icon(error ? 'warning' : 'check', 20), h('span', {}, message), iconButton('close', 'Dismiss notification', () => toast.remove()));
        this.toastRoot.append(toast);
        window.setTimeout(() => toast.remove(), error ? 9000 : 4800);
    };
    openSettings = (tab?: string): void => { openSettings(this, tab); };
    openArtifact = (id: string): void => {
        const activity = document.querySelector<HTMLDialogElement>('.activity-dialog');
        if (activity) {
            this.artifactOrigin = activity.dataset['threadId'] ?? null;
            // Suspend rather than stack; activity's view cache retains selection and both scrolls.
            this.dialogs.closeAll();
        }
        openViewer(this, id, () => {
            const origin = this.artifactOrigin;
            this.artifactOrigin = null;
            if (origin)
                this.openThread(origin);
        });
    };
    openMemoryDocument = (id: string): void => { this.artifactOrigin = null; openMemoryViewer(this, id); };
    prefillComposer = (text: string): void => {
        if (this.state.chats.find(item => item.id === this.ui.chatId)?.channelId)
            this.navigate('chat', 'main');
        if (this.ui.route !== 'chat')
            this.navigate('chat', this.ui.chatId);
        this.ui.draft = this.ui.draft.trim() ? `${this.ui.draft.trimEnd()}\n${text}` : text;
        this.render();
        const input = document.querySelector<HTMLTextAreaElement>('#message-input');
        input?.focus();
        input?.setSelectionRange(input.value.length, input.value.length);
    };
    saveComposer = (): void => {
        const exists = this.state.chats.some(chat => chat.id === this.ui.chatId);
        this.drafts.save(this.ui.chatId, exists ? this.ui.draft : '', exists ? this.ui.attachmentNames : []);
        this.delivery.saveDraft(this.ui.chatId, exists ? this.ui.draft : '', exists ? this.ui.attachmentNames : []);
    };
    completeQueuedDraft = (chatId: string, text: string, attachments: readonly string[]): void => {
        this.saveComposer();
        const draft = this.drafts.read(chatId);
        const remainingText = draft.text.trim() === text ? '' : draft.text;
        const remainingFiles = draft.attachments.filter(id => !attachments.includes(id));
        this.drafts.save(chatId, remainingText, remainingFiles);
        this.delivery.saveDraft(chatId, remainingText, remainingFiles);
        if (this.ui.chatId === chatId) {
            this.ui.draft = remainingText;
            this.ui.attachmentNames = remainingFiles;
        }
    };
    stageAttachment = (chatId: string, id: string): void => {
        this.saveComposer();
        const draft = this.drafts.read(chatId);
        const attachments = [...new Set([...draft.attachments, id])];
        this.drafts.save(chatId, draft.text, attachments);
        this.delivery.saveDraft(chatId, draft.text, attachments);
        if (this.ui.chatId === chatId)
            this.ui.attachmentNames = attachments;
    };
    recoverComposer = (chatId: string, text: string, attachments: readonly string[]): void => {
        if (!this.state.chats.some(chat => chat.id === chatId))
            return;
        this.drafts.save(this.ui.chatId, this.ui.draft, this.ui.attachmentNames);
        const recovered = this.drafts.recover(chatId, text, attachments);
        this.delivery.saveDraft(chatId, recovered.text, recovered.attachments);
        if (chatId === this.ui.chatId) {
            this.ui.draft = recovered.text;
            this.ui.attachmentNames = [...recovered.attachments];
            this.render();
        }
        else {
            this.toast('Your unsent message is back in its original chat.');
        }
    };
    openBrowser = (id: string): void => { openBrowserSession(this, id); };
    openThread = (id: string): void => { openActivity(this, id); };
    openScenarios = (): void => { openScenarios(this); };
    openSearch = (): void => { openSearch(this); };
    private acknowledgeVisible = (): void => {
        if (this.readQueued)
            return;
        this.readQueued = true;
        queueMicrotask(() => {
            this.readQueued = false;
            if (this.delivery.history(this.ui.chatId).phase !== 'ready' || this.delivery.connection.kind !== 'ready' || document.visibilityState !== 'visible' || this.root.inert || this.dock.coversConversation || this.ui.route !== 'chat' || document.querySelector('dialog:modal'))
                return;
            const stream = this.root.querySelector<HTMLElement>('.chat-scroll');
            if (!stream || stream.scrollHeight - stream.scrollTop - stream.clientHeight > 60)
                return;
            if (this.state.chats.find((chat) => chat.id === this.ui.chatId)?.unread)
                void this.run({ type: 'chat.read', id: this.ui.chatId }, true);
        });
    };
    private selectChat(requestedId: string): void {
        const id = resolveChatId(this.state.chats, requestedId) ?? requestedId;
        if (id !== requestedId)
            this.toast('That conversation is no longer available. Opened Main chat.');
        this.ui.recentChatIds = [id, ...this.ui.recentChatIds.filter((other) => other !== id)].slice(0, 6);
        if (id === this.ui.chatId)
            return;
        this.saveComposer();
        this.ui.chatId = id;
        const saved = this.drafts.read(id);
        this.ui.draft = saved.text;
        this.ui.attachmentNames = [...saved.attachments];
    }
    createArtifact = (format?: ArtifactFormat): void => { createArtifactDialog(this, format); };
    private readLocation = (): void => {
        const previousRoute = this.ui.route;
        const previousChat = this.ui.chatId;
        const [rawSegment, ...parts] = location.hash.slice(1).split('/');
        const segment = rawSegment || 'chat';
        const raw = parts.join('/');
        let detail = '';
        try {
            detail = decodeURIComponent(raw);
        }
        catch { /* Malformed links fall back to the requested section. */ }
        if (segment === 'search') {
            this.render();
            this.openSearch();
            return;
        }
        if (isRoute(segment)) {
            this.ui.route = segment;
            if (segment === 'chat') {
                this.selectChat(detail || (location.hash ? 'main' : 'reference'));
                this.ui.navOpen = window.innerWidth > 800 && this.state.settings.keepChatPanelVisible && (previousRoute !== 'chat' || this.ui.navOpen);
                if (detail && detail !== this.ui.chatId)
                    writeRouteHash(`#chat/${encodeURIComponent(this.ui.chatId)}`, true);
            }
            if (segment === 'library') {
                this.ui.libraryFilter = detail.startsWith('system') ? 'system' : isLibraryFilter(detail) ? detail : 'all';
                if (detail.startsWith('system'))
                    this.ui.systemFolder = detail.slice(7);
                if (window.innerWidth > 800)
                    this.ui.navOpen = true;
            }
            if (segment === 'published')
                this.ui.publicationId = detail;
        }
        this.render();
        if (this.ui.route === 'chat' && previousChat !== this.ui.chatId)
            document.getElementById('chat-navigation-toggle')?.focus({ preventScroll: true });
    };
    private onDockChange = (): void => {
        if (this.dock.isOpen && this.dockNavigation === null) {
            this.dockNavigation = this.ui.navOpen;
            if (innerWidth <= 1650)
                this.ui.navOpen = false;
        }
        else if (!this.dock.isOpen && this.dockNavigation !== null) {
            this.ui.navOpen = innerWidth > 800 && this.dockNavigation;
            this.dockNavigation = null;
        }
        this.render();
    };
    private onDockTransition = (event: TransitionEvent): void => {
        if (event.propertyName !== 'width' || event.target !== this.root)
            return;
        const stream = this.root.querySelector<HTMLElement>('.chat-scroll');
        const saved = stream?.dataset['scroll'] ? this.chatScrolls.get(stream.dataset['scroll']) : undefined;
        if (stream && saved?.bottom)
            stream.scrollTop = stream.scrollHeight;
    };
    private onResize = (): void => {
        const width = window.innerWidth;
        const resized = width !== this.lastViewportWidth || window.innerHeight !== this.lastViewportHeight;
        const stream = this.root.querySelector<HTMLElement>('.chat-scroll');
        const saved = stream?.dataset['scroll'] ? this.chatScrolls.get(stream.dataset['scroll']) : undefined;
        if (stream && saved?.bottom)
            stream.scrollTop = stream.scrollHeight;
        let changed = false;
        if (width <= 800 && this.lastViewportWidth > 800) {
            this.ui.navOpen = false;
            changed = true;
        }
        if (width <= 1100 && this.lastViewportWidth > 1100) {
            this.ui.contextOpen = false;
            changed = true;
        }
        this.lastViewportWidth = width;
        this.lastViewportHeight = window.innerHeight;
        if (changed || resized)
            this.render();
    };
    private onKeyboard = (event: KeyboardEvent): void => {
        if (event.isComposing || event.repeat || event.defaultPrevented)
            return;
        if (event.key === 'Escape' && event.composedPath().some(node => node instanceof HTMLDialogElement))
            return;
        if (event.key === 'Escape' && this.ui.route === 'chat' && this.ui.navOpen && !this.dock.isOpen && !document.querySelector('dialog:modal, details.menu[open]')) {
            const active = document.activeElement;
            if (!this.state.settings.keepChatPanelVisible || innerWidth <= 800 || active?.closest('.navigation-panel, .conversation-location')) {
                event.preventDefault();
                setChatNavigationOpen(this, false, true);
                return;
            }
        }
        if ((event.ctrlKey || event.metaKey) && [',', '/', 'd'].includes(event.key.toLowerCase())) {
            if (event.key.toLowerCase() === 'd' && !event.shiftKey)
                return;
            event.preventDefault();
            if (event.key === ',') {
                if (!document.querySelector('.settings-dialog'))
                    this.openSettings();
            }
            else if (event.key === '/') {
                if (!document.querySelector('.shortcuts-dialog'))
                    openShortcuts(this);
            }
            else {
                this.dialogs.closeAll();
                this.openScenarios();
            }
            return;
        }
        if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') {
            event.preventDefault();
            this.openSearch();
        }
        if (event.altKey && /^[1-6]$/.test(event.key)) {
            const area = primaryAreas[Number(event.key) - 1];
            if (area) {
                event.preventDefault();
                this.dialogs.closeAll();
                this.artifactOrigin = null;
                this.dock.close();
                this.navigate(area.route);
            }
        }
    };
    private closeMenus = (event: MouseEvent): void => {
        for (const menu of document.querySelectorAll<HTMLDetailsElement>('details.menu[open]'))
            if (event.target instanceof Node && !menu.contains(event.target))
                menu.open = false;
    };
    private download(payload: DownloadPayload): void {
        const generated = payload.text !== null;
        const url = generated ? URL.createObjectURL(new Blob([payload.text ?? ''], { type: payload.mime })) : payload.asset;
        if (!url) {
            this.toast('There is no downloadable file for this artifact.', true);
            return;
        }
        const link = h('a', { href: url, download: payload.filename });
        document.body.append(link);
        link.click();
        link.remove();
        if (generated)
            window.setTimeout(() => URL.revokeObjectURL(url), 5000);
    }
    private networkChanged = (): void => { this.delivery.setBrowserOffline(!navigator.onLine); };
    private beforeUnload = (event: BeforeUnloadEvent): void => {
        if (this.approvals.pending().length > 0 || this.delivery.hasUnsent || this.delivery.hasDrafts || this.ui.fileImports.size > 0 || this.ui.preparingChats.size > 0) {
            event.preventDefault();
            event.returnValue = '';
        }
    };
    dispose(): void {
        this.contentLoading.dispose();
        this.delivery.dispose();
        this.approvals.dispose();
        window.removeEventListener('beforeunload', this.beforeUnload);
        window.removeEventListener('online', this.networkChanged);
        window.removeEventListener('offline', this.networkChanged);
        this.root.removeEventListener('transitionend', this.onDockTransition);
        this.disposeTooltips();
        this.disposeMenus();
        this.dock.dispose();
        disposeIconMotion(this.root);
        this.colorScheme.removeEventListener('change', this.render);
        document.removeEventListener('visibilitychange', this.acknowledgeVisible);
        window.removeEventListener('focus', this.acknowledgeVisible);
        this.unsubscribe();
        this.adapter.dispose();
        window.removeEventListener('hashchange', this.readLocation);
        window.removeEventListener('popstate', this.readLocation);
        window.removeEventListener('resize', this.onResize);
        window.removeEventListener('keydown', this.onKeyboard);
        document.removeEventListener('click', this.closeMenus);
        this.dialogs.closeAll();
    }
}
