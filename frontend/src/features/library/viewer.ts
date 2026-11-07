import { loadingImage } from '../../components/loading-image.js';
import { createMarkdownEditor } from './markdown-editor.js';
import type { AppContext } from '../../app/context.js';
import type { Artifact, DeepReadonly, SlideLayer } from '../../protocol/models.js';
import { h, valueOf } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button, field, markdown, pill, textareaField } from '../../components/ui.js';
import { openShare } from './library.js';
export function openViewer(ctx: AppContext, id: string, onClose?: () => void): void {
    const initial = ctx.state.artifacts.find((item) => item.id === id);
    if (!initial) {
        ctx.toast('This artifact is no longer available.', true);
        return;
    }
    void ctx.run({ type: 'artifact.open', id }, true);
    const root = h('div', { class: 'viewer-root' });
    const ref = ctx.dock.open(initial.title, root, initial.kind === 'media' ? 'media-dock' : 'viewer-dock', onClose);
    if (initial.format === 'markdown') {
        const editor = createMarkdownEditor(ctx, { key: `artifact:${id}`, content: () => ctx.state.artifacts.find((item) => item.id === id)?.content ?? initial.content, save: async (content) => (await ctx.run({ type: 'artifact.save', id, content }, true)).ok });
        root.append(h('div', { class: 'viewer-toolbar' }, pill('Markdown'), h('span', { class: 'toolbar-spacer' }), button('Share', () => openShare(ctx, id), { icon: 'share' }), button('Download', () => { void ctx.run({ type: 'artifact.download', id }); }, { icon: 'download' })), editor.root);
        if (!(ctx.state.mode === 'live'))
            ref.element.querySelector('.dock-header-actions')?.before(editor.toolbar);
        return;
    }
    let slideIndex = 0;
    let layerId = '';
    let zoom = 100;
    const refresh = (): void => {
        const item = ctx.state.artifacts.find((entry) => entry.id === id);
        if (!item)
            return;
        const content = h('div', { class: 'viewer-content', 'data-scroll': 'viewer-content' });
        const toolbar = h('div', { class: 'viewer-toolbar' }, pill(item.format), h('span', { class: 'muted text-small' }, `Version ${item.version}`), h('span', { class: 'toolbar-spacer' }));
        if (item.kind === 'document' && item.format !== 'slides')
            toolbar.append(button('Ask hatchling for changes', () => {
                ctx.navigate('chat', ctx.ui.chatId);
                ctx.ui.draft = `Please revise ${item.title}: `;
                ctx.render();
                document.querySelector<HTMLElement>('.composer textarea')?.focus({ preventScroll: true });
            }, { icon: 'chat' }));
        toolbar.append(button('Share', () => openShare(ctx, id), { icon: 'share' }));
        if (item.kind === 'document' || item.kind === 'media')
            toolbar.append(button('Download', () => { void ctx.run({ type: 'artifact.download', id }); }, { icon: 'download', variant: 'primary' }));
        if (item.format === 'archive') {
            content.append(h('article', { class: 'archive-preview' }, icon('file', 48), h('h2', {}, item.title), h('p', { class: 'muted' }, item.description), markdown(item.content), button('Download archive', () => { void ctx.run({ type: 'artifact.download', id }, true); }, { icon: 'download', variant: 'primary' })));
        }
        else if (item.format === 'slides') {
            const slide = item.slides[slideIndex] ?? [];
            const selected = slide.find((layer) => layer.id === layerId) ?? slide[0];
            const canvas = h('div', { class: 'slide-canvas', 'aria-label': `Slide ${slideIndex + 1}` });
            for (const layer of slide) {
                const node = h('button', { class: `slide-layer ${selected?.id === layer.id ? 'selected' : ''}`, type: 'button', 'aria-label': `Edit layer: ${layer.text.replaceAll('\n', ' ')}`, style: `left:${layer.x}%;top:${layer.y}%;width:${layer.width}%;font-size:clamp(12px,${layer.size / 10}vw,${layer.size}px)` }, layer.text);
                node.addEventListener('click', () => { layerId = layer.id; refresh(); });
                addLayerDrag(node, canvas, layer, ctx, id, slideIndex, refresh);
                canvas.append(node);
            }
            const controls = h('div', { class: 'slide-pagination' }, button('Previous', () => { slideIndex--; refresh(); }, { disabled: slideIndex === 0 }), h('span', {}, `${slideIndex + 1} / ${item.slides.length}`), button('Next', () => { slideIndex++; refresh(); }, { disabled: slideIndex >= item.slides.length - 1 }));
            const pane = h('div', { class: 'slide-inspector' }, h('h3', {}, 'Layer editor'), h('p', { class: 'muted text-small' }, 'Select a text layer. Drag to move it, or enter precise position and width.'));
            if (selected) {
                const form = h('form', { class: 'stack' }, textareaField('Text', 'layerText', selected.text, 3), h('div', { class: 'field-grid' }, field('Left (%)', 'x', String(selected.x), { type: 'number', min: 0, max: 100 }), field('Top (%)', 'y', String(selected.y), { type: 'number', min: 0, max: 85 }), field('Width (%)', 'width', String(selected.width), { type: 'number', min: 10, max: 100 }), field('Type size', 'size', String(selected.size), { type: 'number', min: 12, max: 80 })), h('button', { type: 'submit', class: 'button primary' }, 'Save layer'));
                form.addEventListener('submit', (event) => { event.preventDefault(); void ctx.run({ type: 'artifact.save-slide', id, slide: slideIndex, layerId: selected.id, text: valueOf(form, 'layerText'), x: Number(valueOf(form, 'x')), y: Number(valueOf(form, 'y')), width: Number(valueOf(form, 'width')), size: Number(valueOf(form, 'size')) }).then(() => refresh()); });
                pane.append(form);
            }
            content.append(h('div', { class: 'slides-layout' }, h('div', {}, canvas, controls, h('p', { class: 'tiny muted' }, 'Download rebuilds an offline HTML slide deck from the saved layers.')), pane));
        }
        else if (item.format === 'image' && item.asset) {
            toolbar.append(button(zoom === 100 ? 'Zoom 150%' : 'Fit image', () => { zoom = zoom === 100 ? 150 : 100; refresh(); }));
            const image = loadingImage({ src: item.asset, alt: item.title, retry: true });
            image.style.width = `${zoom}%`;
            content.append(h('div', { class: 'image-lightbox' }, image));
        }
        else if (item.format === 'video' && item.asset) {
            content.append(h('video', { src: item.asset, poster: item.id === 'video' ? './media/video-poster.png' : undefined, preload: 'metadata', controls: true, playsinline: true, class: 'media-player', 'aria-label': item.title }), h('div', { class: 'pipeline' }, ...['Sample footage', 'Local render', 'Ready to preview'].map((label) => h('span', {}, icon('check', 13), label))), h('p', { class: 'muted text-small' }, item.content));
        }
        else if (item.format === 'audio' && item.asset) {
            content.append(h('div', { class: 'audio-viewer' }, icon('audio', 68), h('h2', {}, item.title), h('p', { class: 'muted' }, item.description), h('audio', { src: item.asset, controls: true, 'aria-label': item.title })));
        }
        else if (item.kind === 'app')
            content.append(renderPackingApp(ctx, item, refresh));
        else if (item.format === 'csv' || item.format === 'spreadsheet') {
            const rows = item.content.split('\n').map((line) => line.split(','));
            const header = rows.shift() ?? [];
            content.append(h('p', { class: 'view-only-note' }, 'View-only · ask the agent for changes or download for local editing.'), h('table', { class: 'data-table spreadsheet-preview' }, h('thead', {}, h('tr', {}, ...header.map((label) => h('th', { scope: 'col' }, label)))), h('tbody', {}, ...rows.map((row) => h('tr', {}, ...row.map((cell, index) => h('td', { 'data-label': header[index] ?? 'Value' }, cell)))))));
        }
        else if (item.format === 'pdf' && item.asset && !item.asset.startsWith('data:')) {
            content.append(h('p', { class: 'view-only-note' }, 'PDF · view-only. Ask for changes to receive a refreshed preview.'), h('iframe', { class: 'pdf-preview', src: item.asset, title: `${item.title} PDF preview` }));
        }
        else {
            const isCode = item.format === 'code' || item.format === 'text';
            const documentView = isCode ? h('pre', { class: 'code-document', tabindex: 0, 'aria-label': `${item.title} source code` }, h('code', {}, ...item.content.split('\n').map((line, index) => h('span', { class: 'code-line', 'data-line': index + 1 }, line || ' ')))) : markdown(item.content);
            const search = h('input', { type: 'search', placeholder: 'Find in document', 'aria-label': 'Find in document' });
            const count = h('span', { class: 'muted tiny', role: 'status' });
            search.addEventListener('input', () => {
                const matches = [...documentView.querySelectorAll<HTMLElement>('p,h1,h2,h3,li,.code-line')].filter((node) => { const matched = search.value.length > 0 && (node.textContent ?? '').toLowerCase().includes(search.value.toLowerCase()); node.classList.toggle('search-match', matched); return matched; });
                count.textContent = search.value ? `${matches.length} matches` : '';
                matches[0]?.scrollIntoView({ block: 'nearest' });
            });
            const outline = h('nav', { class: 'document-outline', 'aria-label': 'Document outline' }, h('span', { class: 'eyebrow' }, 'Outline'), ...[...documentView.querySelectorAll<HTMLElement>('h1,h2,h3')].map((heading) => button(heading.textContent ?? 'Section', () => heading.scrollIntoView({ behavior: 'smooth', block: 'start' }), { variant: 'ghost' })));
            const zoomControl = h('select', { 'aria-label': 'Document zoom' }, ...[80, 100, 125, 150].map((value) => h('option', { value, selected: value === zoom }, `${value}%`)));
            zoomControl.addEventListener('change', () => { zoom = Number(zoomControl.value); documentView.style.fontSize = isCode ? `${13 * zoom / 100}px` : `${zoom}%`; });
            documentView.style.fontSize = isCode ? `${13 * zoom / 100}px` : `${zoom}%`;
            content.append(h('div', { class: 'document-reading' }, h('div', { class: 'document-tools' }, search, count, zoomControl), item.kind === 'page' ? h('p', { class: 'view-only-note' }, 'Static live page · all content is read-only. No form data is collected.') : item.format !== 'markdown' ? h('p', { class: 'view-only-note' }, 'View-only · ask the agent for changes or download for local editing.') : null, h('div', { class: `document-layout ${item.kind === 'page' ? 'page-artifact' : ''}` }, documentView.querySelector('h1,h2,h3') ? outline : null, h('div', { class: `document-paper ${isCode ? 'code-paper' : ''}` }, documentView))));
        }
        root.replaceChildren(toolbar, content);
    };
    refresh();
}
function addLayerDrag(node: HTMLElement, canvas: HTMLElement, layer: DeepReadonly<SlideLayer>, ctx: AppContext, id: string, slide: number, refresh: () => void): void {
    node.addEventListener('pointerdown', (event) => {
        if (event.button !== 0)
            return;
        const rect = canvas.getBoundingClientRect();
        const originX = event.clientX;
        const originY = event.clientY;
        let x = layer.x;
        let y = layer.y;
        let moved = false;
        node.setPointerCapture(event.pointerId);
        const move = (pointer: PointerEvent): void => {
            moved = Math.abs(pointer.clientX - originX) + Math.abs(pointer.clientY - originY) > 4;
            x = Math.round(Math.max(0, Math.min(100 - layer.width, layer.x + (pointer.clientX - originX) / rect.width * 100)));
            y = Math.round(Math.max(0, Math.min(85, layer.y + (pointer.clientY - originY) / rect.height * 100)));
            node.style.left = `${x}%`;
            node.style.top = `${y}%`;
        };
        const up = (): void => {
            node.removeEventListener('pointermove', move);
            if (moved)
                void ctx.run({ type: 'artifact.save-slide', id, slide, layerId: layer.id, text: layer.text, x, y, width: layer.width, size: layer.size }, true).then(refresh);
        };
        node.addEventListener('pointermove', move);
        node.addEventListener('pointerup', up, { once: true });
    });
}
function renderPackingApp(ctx: AppContext, item: DeepReadonly<Artifact>, refresh: () => void): HTMLElement {
    const lines = item.content.split('\n');
    const complete = lines.filter((line) => line.startsWith('[x] ')).length;
    const app = h('div', { class: 'packing-app' }, h('span', { class: 'eyebrow' }, 'A LITTLE LESS TO CARRY'), h('h1', {}, 'The packing list'), h('p', { class: 'muted' }, 'Just the things that make a weekend easier.'), h('progress', { max: lines.length, value: complete, 'aria-label': 'Packing progress' }), h('p', { class: 'text-small' }, `${complete} of ${lines.length} packed`));
    for (const [index, line] of lines.entries()) {
        const checked = line.startsWith('[x] ');
        const input = h('input', { type: 'checkbox', checked });
        input.addEventListener('change', () => { void ctx.run({ type: 'artifact.app-toggle', id: item.id, index }, true).then(refresh); });
        app.append(h('label', { class: `packing-item ${checked ? 'packed' : ''}` }, input, checked ? line.slice(4) : line));
    }
    app.append(h('p', { class: 'tiny muted' }, 'Private app data · persisted by the local adapter · publishing is not available.'));
    return app;
}
