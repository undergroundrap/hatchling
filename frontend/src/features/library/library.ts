import { appendContentState } from '../../components/content-state.js';
import { loadingImage } from '../../components/loading-image.js';
import { renderSystem } from './system.js';
import type { AppContext, LibraryFilter } from '../../app/context.js';
import type { Artifact, ArtifactFormat, DeepReadonly, IconName } from '../../protocol/models.js';
import { canPublish } from '../../presentation/format.js';
import { isFormat } from '../../presentation/format.js';
import { h, valueOf } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, emptyState, field, iconButton, markdown, menu, pill, selectField } from '../../components/ui.js';
export const libraryNames: Record<LibraryFilter, string> = { all: 'All artifacts', documents: 'Documents', web: 'Web artifacts', images: 'Images', videos: 'Videos', audio: 'Podcasts & audio', system: 'System files', trash: 'Trash' };
export function artifactIcon(format: ArtifactFormat): IconName { return format === 'image' ? 'image' : format === 'video' ? 'video' : format === 'audio' ? 'audio' : format === 'html' ? 'globe' : format === 'app' ? 'library' : 'file'; }
function matches(item: DeepReadonly<Artifact>, filter: LibraryFilter): boolean {
    if (filter === 'trash')
        return item.trashUntil !== null;
    if (item.trashUntil !== null)
        return false;
    switch (filter) {
        case 'documents': return item.kind === 'document';
        case 'web': return item.kind === 'page' || item.kind === 'app';
        case 'images': return item.format === 'image';
        case 'videos': return item.format === 'video';
        case 'audio': return item.format === 'audio';
        default: return item.kind !== 'media';
    }
}
export function renderLibrary(ctx: AppContext): HTMLElement {
    if (ctx.ui.libraryFilter === 'system' && !(ctx.state.mode === 'live'))
        return renderSystem(ctx);
    const filter = ctx.ui.libraryFilter === 'system' ? 'all' : ctx.ui.libraryFilter;
    const items = ctx.state.artifacts.filter((item) => matches(item, filter) && item.title.toLowerCase().includes(ctx.ui.navSearch.toLowerCase())).toSorted((a, b) => Number(b.pinned) - Number(a.pinned) || (ctx.ui.librarySort === 'title' ? a.title.localeCompare(b.title) : b[ctx.ui.librarySort].localeCompare(a[ctx.ui.librarySort])));
    const sortOptions = [
        { value: 'opened' as const, label: 'Last opened' },
        { value: 'created' as const, label: 'Last created' },
        { value: 'title' as const, label: 'Title' },
    ];
    const currentSort = sortOptions.find((option) => option.value === ctx.ui.librarySort) ?? { value: 'opened' as const, label: 'Last opened' };
    const sort = h('details', { class: 'menu library-sort-menu' });
    const sortSummary = h('summary', { class: 'sort-trigger', 'aria-label': `Sort artifacts: ${currentSort.label}`, 'aria-haspopup': 'menu' }, h('span', {}, currentSort.label), icon('chevron', 14));
    const sortPopover = h('div', { class: 'menu-popover sort-menu-popover', role: 'menu', 'aria-label': 'Sort artifacts' });
    for (const option of sortOptions) {
        const selected = option.value === ctx.ui.librarySort;
        const control = h('button', { type: 'button', class: `button ghost sort-menu-option ${selected ? 'selected' : ''}`, role: 'menuitemradio', 'aria-checked': String(selected) }, h('span', { class: 'menu-item-label' }, option.label), selected ? icon('check', 14) : h('span', { class: 'sort-menu-check-spacer', 'aria-hidden': 'true' }));
        control.addEventListener('click', () => {
            ctx.ui.librarySort = option.value;
            sort.open = false;
            ctx.render();
        });
        sortPopover.append(control);
    }
    sort.addEventListener('keydown', (event) => {
        if (event.key === 'Escape' && sort.open) {
            event.preventDefault();
            sort.open = false;
            sortSummary.focus({ preventScroll: true });
        }
    });
    sort.append(sortSummary, sortPopover);
    const page = h('div', { class: 'page wide-page library-page' }, h('header', { class: 'page-header' }, h('h1', {}, libraryNames[filter]), h('div', { class: 'page-actions' }, sort, ctx.state.mode === 'live' ? null : button(ctx.ui.selectionMode ? 'Done' : 'Select', () => { ctx.ui.selectionMode = !ctx.ui.selectionMode; ctx.ui.selectedArtifacts.clear(); ctx.render(); }), iconButton(ctx.ui.libraryLayout === 'grid' ? 'list' : 'grid', ctx.ui.libraryLayout === 'grid' ? 'Switch to list view' : 'Switch to grid view', () => { ctx.ui.libraryLayout = ctx.ui.libraryLayout === 'grid' ? 'list' : 'grid'; ctx.render(); }), ctx.state.mode === 'live' ? null : button('Create an artifact', () => ctx.createArtifact(), { icon: 'plus', variant: 'primary' }))));
    if (appendContentState(ctx, page, 'library'))
        return page;
    if (ctx.ui.selectionMode)
        page.append(h('div', { class: 'selection-bar' }, `${ctx.ui.selectedArtifacts.size} selected`, button(filter === 'trash' ? 'Restore selected' : 'Move selected to Trash', () => {
            const ids = [...ctx.ui.selectedArtifacts];
            ctx.dialogs.confirm(filter === 'trash' ? 'Restore selected artifacts?' : 'Move selected artifacts to Trash?', filter === 'trash' ? 'The selected files will return to your library.' : 'These files can be restored for 30 days. Published snapshots are not changed.', () => {
                void (async () => {
                    for (const id of ids)
                        await ctx.run({ type: filter === 'trash' ? 'artifact.restore' : 'artifact.trash', id }, true);
                    ctx.ui.selectedArtifacts.clear();
                    ctx.render();
                })();
            }, { label: filter === 'trash' ? 'Restore' : 'Move to Trash', danger: filter !== 'trash' });
        }, { variant: filter === 'trash' ? 'primary' : 'danger', disabled: ctx.ui.selectedArtifacts.size === 0 })));
    page.append(h('div', { class: 'section-heading library-section-label' }, h('h2', {}, filter === 'trash' ? 'Recoverable for 30 days' : libraryNames[filter]), h('span', { class: 'muted tiny' }, `${items.length} ${items.length === 1 ? 'item' : 'items'}${ctx.state.mode === 'live' ? '' : ' · sample workspace'}`)));
    if (!items.length)
        page.append(emptyState(filter === 'trash' ? 'trash' : 'library', 'Nothing here yet.', ctx.state.mode === 'live' ? 'Hatchling can’t make files you can download yet. They’ll appear here when it can.' : 'Create an artifact using the button above.'));
    else
        page.append(h('div', { class: ctx.ui.libraryLayout === 'grid' ? 'artifact-grid' : 'artifact-list' }, ...items.map((item) => artifactCard(ctx, item))));
    return page;
}
function artifactCard(ctx: AppContext, item: DeepReadonly<Artifact>): HTMLElement {
    const open = (): void => {
        if (ctx.ui.selectionMode) {
            if (ctx.ui.selectedArtifacts.has(item.id))
                ctx.ui.selectedArtifacts.delete(item.id);
            else
                ctx.ui.selectedArtifacts.add(item.id);
            ctx.render();
        }
        else
            ctx.openArtifact(item.id);
    };
    const card = h('article', { class: `artifact-card ${ctx.ui.selectedArtifacts.has(item.id) ? 'selected' : ''}`, 'data-artifact': item.id });
    const preview = h('button', { type: 'button', class: `artifact-preview preview-${item.format}`, 'aria-label': `Open ${item.title}` });
    preview.addEventListener('click', open);
    if (item.format === 'image' && item.asset)
        preview.append(loadingImage({ src: item.asset, alt: item.title, lazy: true, fit: 'contain', className: 'thumbnail-loading-image' }));
    else if (item.format === 'slides')
        preview.append(h('div', { class: 'slide-thumbnail' }, h('span', {}, 'A WEEKEND, WELL SPENT'), h('strong', {}, item.slides[0]?.[0]?.text ?? item.title), h('small', {}, `${item.slides.length} editable slides`)));
    else if (item.format === 'video')
        preview.append(h('div', { class: 'video-thumb' }, loadingImage({ src: item.id === 'video' ? './media/video-poster.png' : './media/hatchling-avatar.png', alt: '', lazy: true, className: 'thumbnail-loading-image' }), icon('play', 32)));
    else if (item.format === 'audio')
        preview.append(h('div', { class: 'audio-thumb' }, icon('audio', 48), h('span', {}, 'A small pause')));
    else if (item.format === 'app')
        preview.append(h('div', { class: 'app-thumbnail' }, h('span', { class: 'eyebrow' }, 'A LITTLE LESS TO CARRY'), h('strong', {}, 'The packing list'), ...item.content.split('\n').slice(0, 3).map((line) => h('span', {}, icon('goal', 13), line))));
    else
        preview.append(h('div', { class: 'document-thumbnail' }, markdown(item.content)));
    const meta = h('button', { class: 'artifact-meta', type: 'button' }, h('span', { class: `artifact-type-icon ${item.format}` }, icon(artifactIcon(item.format), 23)), h('span', { class: 'row-copy' }, h('strong', {}, item.title), h('span', { class: 'muted text-small' }, `${item.format === 'markdown' ? 'Text' : item.kind === 'document' ? 'Document' : item.kind === 'page' ? 'Live page' : item.kind === 'app' ? 'Interactive app' : item.format} · ${item.pinned ? 'Pinned' : 'No recent activity'}`)));
    meta.addEventListener('click', open);
    card.append(preview, meta, menu(`Actions for ${item.title}`, item.trashUntil ? [
        { label: 'Restore', icon: 'refresh', action: () => { void ctx.run({ type: 'artifact.restore', id: item.id }); } },
    ] : [
        ...(ctx.state.mode === 'live' ? [] : [{ label: item.pinned ? 'Unpin' : 'Pin', icon: 'pin' as const, action: () => { void ctx.run({ type: 'artifact.pin', id: item.id }); } }]),
        { label: 'Share', icon: 'share', action: () => openShare(ctx, item.id) },
        { label: item.kind === 'page' || item.kind === 'app' ? 'Open live artifact' : 'Download', icon: item.kind === 'page' || item.kind === 'app' ? 'globe' : 'download', action: () => {
                if (item.kind === 'page' || item.kind === 'app')
                    ctx.openArtifact(item.id);
                else
                    void ctx.run({ type: 'artifact.download', id: item.id });
            } },
        ...(ctx.state.mode === 'live' ? [] : [{ label: 'Move to Trash', icon: 'trash' as const, danger: true, action: () => ctx.dialogs.confirm('Move to Trash?', 'You can restore this artifact for 30 days. Published snapshots stay unchanged.', () => { void ctx.run({ type: 'artifact.trash', id: item.id }); }, { label: 'Move to Trash', danger: true }) }]),
    ]));
    if (ctx.ui.selectionMode)
        card.append(h('span', { class: 'selection-mark', 'aria-label': ctx.ui.selectedArtifacts.has(item.id) ? 'Selected' : 'Not selected' }, ctx.ui.selectedArtifacts.has(item.id) ? icon('check', 14) : null));
    return card;
}
export function openShare(ctx: AppContext, id: string): void {
    const item = ctx.state.artifacts.find((entry) => entry.id === id);
    if (!item)
        return;
    if (item.kind === 'media') {
        ctx.dialogs.open('Share a media copy', h('div', { class: 'stack' }, h('p', {}, ctx.state.mode === 'live' ? 'Download this media to share a copy.' : 'Download this media to share a local copy. Public media hosting is not provided by the scripted adapter.'), button('Download media', () => { void ctx.run({ type: 'artifact.download', id: item.id }); }, { icon: 'download', variant: 'primary' })));
        return;
    }
    if (!canPublish(item.kind)) {
        ctx.dialogs.open('This app stays private', h('div', { class: 'stack' }, icon('lock', 32), h('p', {}, 'Interactive apps cannot be published. Their application data never leaves their host-side workspace.'), h('p', { class: 'muted' }, 'Chat artifact links are not share links.')));
        return;
    }
    const content = h('div', { class: 'stack' }, h('div', { class: 'share-artifact' }, icon(artifactIcon(item.format), 28), h('strong', {}, item.title), pill(`Version ${item.version}`)), h('p', {}, item.publishedVersion ? 'An existing snapshot is unchanged. Updating it needs a fresh approval for this exact version.' : ctx.state.mode === 'live' ? 'Sharing needs your approval of this exact version, so review it first. Later edits don’t change what’s shared.' : 'Publishing makes a static copy visible to anyone with its public link. Review the content before approving.'), ctx.state.mode === 'live' ? null : h('div', { class: 'notice' }, icon('shield', 18), 'This frontend only creates a local preview link. No content is publicly hosted or uploaded.'));
    const ref = ctx.dialogs.open('Share an approved snapshot', content);
    const approve = button(item.publishedVersion ? 'Approve updated snapshot' : 'Approve snapshot', () => {
        void ctx.run({ type: 'artifact.publish', id: item.id, approvedVersion: item.version }).then((result) => {
            if (result.ok && ctx.state.mode === 'live')
                ref.close();
            if (!result.ok || !result.id)
                return;
            const link = `${location.href.split('#')[0] ?? ''}#published/${result.id}`;
            const input = h('input', { readonly: true, value: link, 'aria-label': 'Local snapshot preview link' });
            content.replaceChildren(h('div', { class: 'notice good-notice' }, icon('check', 20), 'Approved local snapshot. No public upload occurred.'), h('label', { class: 'form-field' }, 'Local preview link', input), button('Copy preview link', () => { void navigator.clipboard.writeText(link).then(() => ctx.toast('Local preview link copied.')).catch(() => { input.select(); ctx.toast('Select and copy the preview link.'); }); }, { icon: 'copy' }), button('Open approved snapshot', () => { ref.close(); ctx.navigate('published', result.id); }, { variant: 'primary' }));
        });
    }, { variant: 'primary', icon: 'share' });
    content.append(approve);
}
export function createArtifactDialog(ctx: AppContext, format: ArtifactFormat = 'markdown'): void {
    const form = h('form', { class: 'stack' }, field('Artifact title', 'title', '', { placeholder: 'A useful next step', required: true, autofocus: true }), selectField('Format', 'format', [{ value: 'markdown', label: 'Markdown document · editable' }, { value: 'slides', label: 'Slide deck · editable layers' }, { value: 'csv', label: 'CSV · downloadable table' }, { value: 'html', label: 'Static page · live preview' }, { value: 'app', label: 'Interactive app · never publishable' }], format), h('p', { class: 'muted text-small' }, 'The scripted adapter creates finished sample content. A live adapter can replace creation without changing this screen.'));
    const submit = h('button', { class: 'button primary', type: 'submit' }, icon('plus', 16), 'Create artifact');
    form.append(submit);
    const ref = ctx.dialogs.open('Create an artifact', form);
    form.addEventListener('submit', (event) => {
        event.preventDefault();
        const chosen = valueOf(form, 'format');
        if (!isFormat(chosen))
            return;
        void ctx.run({ type: 'artifact.create', title: valueOf(form, 'title'), format: chosen }).then((result) => {
            if (result.ok && result.id) {
                ref.close();
                ctx.openArtifact(result.id);
            }
        });
    });
}
