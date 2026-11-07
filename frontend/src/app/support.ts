import type { AppContext } from './context.js';
import { h, valueOf } from '../components/dom.js';
import { button, field, markdown, textareaField } from '../components/ui.js';
const shortcuts = [
    ['Search', 'Ctrl / ⌘ K'], ['Settings', 'Ctrl / ⌘ ,'], ['Keyboard shortcuts', 'Ctrl / ⌘ /'],
    ['Sample workflows', 'Ctrl / ⌘ Shift D'], ['Switch main area', 'Alt 1–6'], ['Send message', 'Enter'], ['New line', 'Shift Enter'], ['Close menu or window', 'Escape'],
];
export function openShortcuts(ctx: AppContext): void {
    const body = h('div', { class: 'shortcut-list' }, ...shortcuts.filter(([name]) => !(ctx.state.mode === 'live' && name === 'Sample workflows')).map(([name, keys]) => h('div', {}, h('span', {}, name ?? ''), h('kbd', {}, keys ?? ''))));
    ctx.dialogs.open('Keyboard shortcuts', body, 'shortcuts-dialog');
}
export function openDownloads(ctx: AppContext): void {
    if (ctx.state.mode === 'live') {
        ctx.dialogs.open('New versions', h('div', { class: 'stack' }, markdown('Hatchling never checks for updates by itself, because it sends nothing out on its own.\n\nNew versions are on [the releases page](https://github.com/undergroundrap/hatchling/releases).')));
        return;
    }
    const instructions = '# Hatchling browser workspace\n\nNo native installers are included in this frontend.\n\n## Desktop\nOpen the provided standalone HTML in a modern browser, or run npm ci and npm run dev from the source package. A browser shortcut can be created from its main menu.\n\n## Phone or tablet\nUse a hosted static copy in your browser; the layout reflows for touch. Add it to your home screen using your browser’s Share or main menu. Native background work is not provided.\n\nAll external services and agent work are local demonstrations.\n';
    const body = h('div', { class: 'stack' }, h('p', {}, 'Hatchling runs in your browser. Native desktop and mobile installers are not included in this frontend.'), h('section', { class: 'settings-group' }, h('h3', {}, 'Desktop'), h('p', {}, 'Open the standalone HTML, or run the source project. Use your browser’s shortcut option for quick access.')), h('section', { class: 'settings-group' }, h('h3', {}, 'Phone and tablet'), h('p', {}, 'Open a hosted copy in your browser. Add it to your home screen from the Share or browser menu.')), button('Download setup instructions', () => { void ctx.run({ type: 'support.download', title: 'hatchling-setup', body: instructions }); }, { icon: 'download' }));
    ctx.dialogs.open('Download apps', body);
}
export function openIssueReport(ctx: AppContext): void {
    if (ctx.state.mode === 'live') {
        ctx.dialogs.open('Report an issue', h('div', { class: 'stack' }, markdown('Tell me what happened in [an issue on GitHub](https://github.com/undergroundrap/hatchling/issues). Leave out keys, passwords and anything personal.\n\nIf it’s a security problem, please follow [the security policy](https://github.com/undergroundrap/hatchling/blob/main/SECURITY.md) instead.')), 'issue-report-dialog');
        return;
    }
    const body = h('form', { class: 'stack issue-report-form' }, h('p', { class: 'text-small muted' }, 'Create a local issue note to share with a developer. Nothing is automatically sent. Leave out credentials and personal records.'), field('Issue title', 'title', '', { required: true }), textareaField('What happened?', 'description', '', 5), textareaField('Expected behavior', 'expected', '', 3), h('button', { type: 'submit', class: 'button primary' }, 'Download issue note'));
    body.addEventListener('submit', event => { event.preventDefault(); void ctx.run({ type: 'support.download', title: 'hatchling-issue', body: `# ${valueOf(body, 'title')}\n\n## What happened\n${valueOf(body, 'description')}\n\n## Expected behavior\n${valueOf(body, 'expected')}\n\n## Local build\nHatchling Frontend 1.9.0 · ${ctx.ui.route} · ${innerWidth} × ${innerHeight}\n` }); });
    ctx.dialogs.open('Report an issue', body, 'issue-report-dialog');
}
