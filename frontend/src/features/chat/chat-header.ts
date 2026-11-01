import type { AppContext } from '../../app/context.js';
import type { Chat, DeepReadonly } from '../../protocol/models.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { iconButton } from '../../components/ui.js';
import { setChatNavigationOpen } from './chat-navigation.js';
/** Main is a destination, not the browser's previous entry or the agent's display name. */
export function chatBreadcrumb(chat: Pick<Chat, 'id' | 'title'> | undefined): {
    label: string;
    isMain: boolean;
    description: string;
} {
    const isMain = !chat || chat.id === 'main';
    return { label: isMain ? 'Chats' : chat.title, isMain, description: isMain ? 'Main chat' : `Side chat: ${chat.title}` };
}
/** Explicitly resolve stale links, never send into an id whose transcript is not being shown. */
export function resolveChatId(chats: readonly Pick<Chat, 'id'>[], requested: string): string | undefined {
    return chats.find(chat => chat.id === requested)?.id ?? chats.find(chat => chat.id === 'main')?.id;
}
export function renderChatHeader(ctx: AppContext, chat: DeepReadonly<Chat> | undefined): HTMLElement {
    const breadcrumb = chatBreadcrumb(chat);
    const browserOpen = document.body.classList.contains('has-browser-session');
    const hasUnread = ctx.state.chats.some(item => item.unread && !item.archived);
    const header = h('header', { class: 'chat-toolbar conversation-toolbar', 'aria-label': 'Conversation navigation', 'data-chat-id': chat?.id ?? '' });
    const group = h('nav', { class: 'conversation-location', 'aria-label': chat?.channelId ? `Connector chat: ${chat.title}` : breadcrumb.description });
    if (browserOpen || !breadcrumb.isMain) {
        const back = iconButton('arrow', browserOpen ? 'Back to conversation' : 'Back to Main chat', () => browserOpen ? ctx.dock.close() : ctx.navigate('chat', 'main'), 'conversation-back');
        back.id = 'chat-back';
        group.append(back);
    }
    const toggle = h('button', {
        id: 'chat-navigation-toggle', type: 'button', class: `chat-navigation-pill${hasUnread ? ' has-unread' : ''}`,
        'aria-label': 'Toggle navigation', 'aria-description': `${breadcrumb.description}. ${ctx.ui.navOpen ? 'Hide' : 'Show'} the chat list.${hasUnread ? ' Unread messages available.' : ''}`,
        'aria-controls': 'chat-navigation', 'aria-expanded': ctx.ui.navOpen,
    }, h('span', { class: 'chat-navigation-glyph', 'aria-hidden': 'true' }, icon('menu', 20)), h('span', { class: 'conversation-title' }, breadcrumb.label));
    // Do not duplicate a short visible label in a tooltip. Long titles remain discoverable.
    if (!breadcrumb.isMain)
        toggle.dataset['tooltip'] = breadcrumb.label;
    toggle.addEventListener('click', () => setChatNavigationOpen(ctx, !ctx.ui.navOpen, true));
    group.append(toggle);
    // The owner's reset of what the agent is given: the messages stay, the agent forgets them.
    const port = ctx.conversations;
    if (port && chat && !browserOpen && !chat.archived && chat.channelId === null) {
        const fresh = iconButton('plus', 'New conversation', () => {
            void port.start(chat.id).then(result => {
                if (!result.ok)
                    ctx.toast(result.message, true);
            });
        }, 'conversation-new');
        fresh.id = 'chat-new-conversation';
        group.append(fresh);
    }
    header.append(group);
    return header;
}
