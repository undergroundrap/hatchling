import type { ContentResource, ContentPreview } from './content-loading.js';
import { loadingRegion } from '../components/skeleton.js';
import { loadingImage } from '../components/loading-image.js';
import type { ApprovalScenario } from '../session/task-approvals.js';
import { reliabilityScenarios } from '../features/chat/reliability-ui.js';
import type { AppContext } from './context.js';
import type { IconName, Scenario } from '../protocol/models.js';
import { h } from '../components/dom.js';
import { icon } from '../components/icons.js';
export const scenarios: readonly {
    id: Scenario;
    title: string;
    description: string;
    icon: IconName;
    destination: 'chat' | 'browser' | 'channels';
}[] = [
    { id: 'browser-watch', title: 'Watch the browser in chat', description: 'An updating browser card, a right-hand session, takeover, permission gate, stop and retained preview. Local frames only.', icon: 'browser', destination: 'chat' },
    { id: 'bundle-files', title: 'Bundle files · active activity', description: 'Two running tasks, spinning indicators, a retained failed step and real local file previews. No external commands run.', icon: 'run', destination: 'chat' },
    { id: 'fix-ci', title: 'Live status & unread completion', description: 'Fixing CI → Checking tests → Connected. A finished result lands in the frontend side chat.', icon: 'code', destination: 'chat' },
    { id: 'research', title: 'Research & delegation', description: 'Live subtitles, child lanes, a failed step, and a successful finish with evidence.', icon: 'run', destination: 'chat' },
    { id: 'purchase', title: 'Purchase review', description: 'Full material terms, masked cards, plain “yes”, and one sample order.', icon: 'cart', destination: 'chat' },
    { id: 'price-change', title: 'Changed checkout quote', description: 'Approve a quote, then simulate a higher total. Fresh approval is required.', icon: 'wallet', destination: 'chat' },
    { id: 'unknown-payment', title: 'Unknown payment outcome', description: 'A submitted order with no confirmation. No retry; takeover only.', icon: 'warning', destination: 'chat' },
    { id: 'captcha', title: 'Permission-gated challenge', description: 'Show site and blocked step, then grant once or take over.', icon: 'shield', destination: 'browser' },
    { id: 'persistent-captcha', title: 'Persistent site challenge', description: 'Attempts exhausted. A clear handoff, never an automatic loop.', icon: 'device', destination: 'browser' },
    { id: 'channels', title: 'Every channel state', description: 'Linked, unlinked, link pending, checking, unavailable, and an approval in Messenger.', icon: 'chat', destination: 'channels' },
    { id: 'secure', title: 'Secure entry & site codes', description: 'Native cards and a protected handoff without collecting a secret.', icon: 'lock', destination: 'chat' },
    { id: 'choices', title: 'Text cadence & choices', description: 'Explicit bubble breaks, retained newlines, bounded buttons, and a free-form question.', icon: 'list', destination: 'chat' },
    { id: 'mixed-stream', title: 'A mixed conversation', description: 'Scheduled results, device notices, handoffs, files, images, and a friendly error.', icon: 'paperclip', destination: 'chat' },
    { id: 'interrupted', title: 'Interrupted work', description: 'An explicit pause with a way to resume from saved context.', icon: 'pause', destination: 'chat' },
    { id: 'stalled', title: 'Waiting on a source', description: 'A stalled turn with a plain-language explanation, not a raw error.', icon: 'clock', destination: 'chat' },
];
export function openScenarios(ctx: AppContext): void {
    if (ctx.state.mode !== 'scripted') {
        ctx.toast('Sample scenarios are available only in the scripted workspace.');
        return;
    }
    const body = h('div', { class: 'scenario-list' }, h('p', { class: 'muted' }, 'Choose a deterministic walkthrough. It replaces the main chat’s sample conversation; your artifacts and other work remain. No real external action runs.'));
    const ref = ctx.dialogs.open('Scripted demo scenarios', body, 'scenarios-dialog');
    if (ctx.delivery.port?.preview) {
        body.append(h('h3', { class: 'scenario-section-heading' }, 'Loading & delivery edge cases'));
        for (const scenario of reliabilityScenarios) {
            const row = h('button', { type: 'button', class: 'scenario-row', 'data-reliability-scenario': scenario.id }, h('span', { class: 'row-icon' }, icon(scenario.icon, 21)), h('span', { class: 'row-copy' }, h('strong', {}, scenario.title), h('span', { class: 'muted text-small' }, scenario.description)), icon('chevron', 18));
            row.addEventListener('click', () => {
                ref.close();
                if (ctx.state.chats.find(chat => chat.id === ctx.ui.chatId)?.channelId || ctx.ui.route !== 'chat')
                    ctx.navigate('chat', 'main');
                ctx.delivery.preview(scenario.id, ctx.ui.chatId);
                if (scenario.draft && !ctx.ui.draft.trim()) {
                    ctx.ui.draft = scenario.draft;
                    ctx.render();
                }
            });
            body.append(row);
        }
        body.append(h('h3', { class: 'scenario-section-heading' }, 'Product walkthroughs'));
    }
    if (ctx.approvals.port?.preview) {
        body.append(h('h3', { class: 'scenario-section-heading' }, 'Task approvals'));
        const approvals: readonly {
            id: ApprovalScenario;
            title: string;
            description: string;
        }[] = [
            { id: 'network', title: 'Task approval above the input', description: 'One scoped permission request, mirrored in Activity and Approvals history.' },
            { id: 'multiple', title: 'Multiple approval requests', description: 'Review each request separately; no blanket approval.' },
            { id: 'expired', title: 'Expired approval', description: 'A timed-out request stays in history and cannot be approved.' },
            { id: 'lapsed', title: 'Answer arrives too late', description: 'The request timed out first: nothing was decided, and the app says so.' },
            { id: 'failed', title: 'Decision could not be saved', description: 'The request stays visible with an explicit retryable failure.' },
            { id: 'unknown', title: 'Decision not confirmed', description: 'Check the result without sending permission a second time.' },
        ];
        for (const scenario of approvals) {
            const row = h('button', { type: 'button', class: 'scenario-row', 'data-approval-scenario': scenario.id }, h('span', { class: 'row-icon' }, icon('shield', 21)), h('span', { class: 'row-copy' }, h('strong', {}, scenario.title), h('span', { class: 'muted text-small' }, scenario.description)), icon('chevron', 18));
            row.addEventListener('click', () => {
                ref.close();
                if (ctx.ui.route !== 'chat' || ctx.state.chats.find(chat => chat.id === ctx.ui.chatId)?.channelId)
                    ctx.navigate('chat', 'main');
                ctx.ui.contextOpen = innerWidth > 1100;
                ctx.ui.contextTab = 'approvals';
                ctx.approvals.port?.preview?.(scenario.id, ctx.ui.chatId);
                ctx.render();
            });
            body.append(row);
        }
    }
    body.append(h('h3', { class: 'scenario-section-heading' }, 'Loading & skeletons'));
    const loadingPreviews: readonly {
        resource: ContentResource;
        mode: ContentPreview;
        title: string;
        description: string;
    }[] = [
        { resource: 'ideas', mode: 'loading', title: 'Ideas loading skeletons', description: 'Six-second first-load preview. Real content returns without changing your ideas.' },
        { resource: 'feed', mode: 'loading', title: 'Feed text & image skeletons', description: 'An editorial first-load layout. Sample content only; no articles are fetched.' },
        { resource: 'library', mode: 'loading', title: 'Artifact library skeletons', description: 'Reserved preview and metadata space; works in grid and list views.' },
        { resource: 'ideas', mode: 'error', title: 'Ideas loading failed', description: 'The skeleton gives way to an explicit error and retry action.' },
        { resource: 'feed', mode: 'refresh', title: 'Refresh without hiding content', description: 'A small updating notice; the last loaded feed remains readable.' },
        { resource: 'feed', mode: 'refresh-error', title: 'Refresh failed with cached content', description: 'Keep the previous edition visible and offer a retry.' },
    ];
    for (const preview of loadingPreviews) {
        const row = h('button', { type: 'button', class: 'scenario-row', 'data-loading-preview': `${preview.resource}-${preview.mode}` }, h('span', { class: 'row-icon' }, icon('clock', 21)), h('span', { class: 'row-copy' }, h('strong', {}, preview.title), h('span', { class: 'muted text-small' }, preview.description)), icon('chevron', 18));
        row.addEventListener('click', () => {
            ref.close();
            ctx.navigate(preview.resource);
            if (!ctx.contentLoading.preview(preview.resource, preview.mode))
                ctx.toast('Finish the current generation before starting a preview.');
        });
        body.append(row);
    }
    const specimens = h('button', { type: 'button', class: 'scenario-row', 'data-loading-preview': 'components' }, h('span', { class: 'row-icon' }, icon('library', 21)), h('span', { class: 'row-copy' }, h('strong', {}, 'Loading component reference'), h('span', { class: 'muted text-small' }, 'Shared document, conversation and media components used by the style guide.')), icon('chevron', 18));
    specimens.addEventListener('click', () => {
        ref.close();
        ctx.dialogs.open('Loading component reference', h('div', { class: 'stack' }, h('p', { class: 'muted text-small' }, 'Component samples only. These placeholders are not a document being generated. Production views render available content immediately.'), h('section', { class: 'loading-specimen' }, h('h3', {}, 'Document'), loadingRegion('Document skeleton example', 'document')), h('section', { class: 'loading-specimen' }, h('h3', {}, 'Conversation'), loadingRegion('Conversation skeleton example', 'chat')), h('section', { class: 'loading-specimen' }, h('h3', {}, 'Media with real load events'), loadingImage({ src: './media/hatchling-mascot.png', alt: 'Hatchling mascot', retry: true }))));
    });
    body.append(specimens, h('h3', { class: 'scenario-section-heading' }, 'More product walkthroughs'));
    for (const scenario of scenarios) {
        const row = h('button', { type: 'button', class: 'scenario-row', 'data-scenario': scenario.id }, h('span', { class: 'row-icon' }, icon(scenario.icon, 21)), h('span', { class: 'row-copy' }, h('strong', {}, scenario.title), h('span', { class: 'muted text-small' }, scenario.description)), icon('chevron', 18));
        row.addEventListener('click', () => {
            ref.close();
            void ctx.run({ type: 'demo.scenario', scenario: scenario.id }, true).then((result) => {
                if (!result.ok)
                    return;
                ctx.ui.contextOpen = window.innerWidth > 1100;
                ctx.ui.contextTab = 'activity';
                ctx.navigate(scenario.destination === 'browser' ? 'browser' : 'chat', scenario.id === 'bundle-files' ? 'reference' : 'main');
                if (scenario.destination === 'channels')
                    ctx.openSettings('channels');
            });
        });
        body.append(row);
    }
}
