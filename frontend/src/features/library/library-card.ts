// One card of the Library page, as the demo draws it (WO-043, #523): shared
// by the demo's artifacts and the files and folders the agent gave.
import type { IconName } from '../../protocol/models.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { menu } from '../../components/ui.js';
/** What a Library card shows and does: the demo's artifacts and the files the agent gave share it. */
export interface CardParts {
    /** Its name, shown whole where the card says it must be (`given`). */
    readonly title: string;
    /** The line under the name. */
    readonly sub: string;
    /** What fills the preview area. */
    readonly preview: readonly HTMLElement[];
    /** The preview area's own class, by its kind. */
    readonly previewClass: string;
    readonly typeIcon: IconName;
    readonly typeClass: string;
    /** The open and preview buttons' ids, so a redraw gives either back its focus. */
    readonly openId?: string;
    readonly previewId?: string;
    /** False for a card nothing opens: its parts are then no buttons. */
    readonly openable?: boolean;
    /** The open and actions buttons' accessible names. */
    readonly openLabel: string;
    readonly menuLabel: string;
    readonly actions: Parameters<typeof menu>[1];
    /** Whether cards are being chosen, and this one is. */
    readonly selecting: boolean;
    readonly selected: boolean;
    /** Opening it, or, while choosing, choosing it or letting it go. */
    readonly onOpen: () => void;
    readonly className?: string;
    readonly attributes?: Readonly<Record<string, string>>;
    /** More lines under the name: a word of caution that must stay on the card. */
    readonly notes?: readonly HTMLElement[];
}
/** One Library card, as the demo draws it: a preview, its type, name and line, and a menu of its actions. */
export function libraryCard(parts: CardParts): HTMLElement {
    const pressed = parts.selecting ? { 'aria-pressed': parts.selected } : {};
    const card = h('article', { class: ['artifact-card', parts.className, parts.selected ? 'selected' : ''].filter(Boolean).join(' '), ...parts.attributes });
    const copy = [h('span', { class: `artifact-type-icon ${parts.typeClass}` }, icon(parts.typeIcon, 23)), h('span', { class: 'row-copy' }, h('strong', {}, parts.title), h('span', { class: 'muted text-small' }, parts.sub), ...(parts.notes ?? []))];
    if (parts.openable === false)
        card.append(h('div', { class: `artifact-preview ${parts.previewClass}` }, ...parts.preview), h('div', { class: 'artifact-meta' }, ...copy));
    else {
        const preview = h('button', { type: 'button', class: `artifact-preview ${parts.previewClass}`, 'aria-label': parts.openLabel, id: parts.previewId, ...pressed }, ...parts.preview);
        preview.addEventListener('click', parts.onOpen);
        const meta = h('button', { class: 'artifact-meta', type: 'button', id: parts.openId, ...pressed }, ...copy);
        meta.addEventListener('click', parts.onOpen);
        card.append(preview, meta);
    }
    if (parts.actions.length > 0)
        card.append(menu(parts.menuLabel, parts.actions));
    if (parts.selecting)
        card.append(h('span', { class: 'selection-mark', 'aria-label': parts.selected ? 'Selected' : 'Not selected' }, parts.selected ? icon('check', 14) : null));
    return card;
}
