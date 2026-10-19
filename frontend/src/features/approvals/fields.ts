// The fields of an approval request, as the owner reviews them (WO-027).
//
// The broker sends what it resolved and what the agent wrote, labeled. The
// review keeps that order and that label, shows each value whole and as
// inert text in its own direction, and shows every character a reviewer
// couldn't otherwise see (a direction override, a zero-width joiner) as its
// code point. The action that runs is the broker's record, never this view:
// the view only makes sure nothing in it is hidden or reordered.
import type { ReviewField } from '../../session/task-approvals.js';
import { h } from '../../components/dom.js';
/** One piece of a value: text shown as it is, or a hidden character, shown as its code point. */
export type Segment = {
    readonly text: string;
} | {
    readonly hidden: string;
};
/**
 * Characters a reviewer can't see, or that reorder the text around them:
 * format characters (direction controls, zero-width characters, the soft
 * hyphen, tag characters), the Hangul fillers, and variation selectors.
 */
const HIDDEN = /\p{Cf}|[ᅟᅠㅤﾠ︀-️]|[\u{e0100}-\u{e01ef}]/u;
/** `U+` and the code point of `character`, in hex. */
function codePoint(character: string): string {
    return `U+${(character.codePointAt(0) ?? 0).toString(16).toUpperCase().padStart(4, '0')}`;
}
/** `text` in pieces: runs of visible text, and each hidden character on its own. */
export function revealSegments(text: string): readonly Segment[] {
    const segments: Segment[] = [];
    let run = '';
    for (const character of text) {
        if (HIDDEN.test(character)) {
            if (run !== '')
                segments.push({ text: run });
            run = '';
            segments.push({ hidden: codePoint(character) });
        }
        else
            run += character;
    }
    if (run !== '')
        segments.push({ text: run });
    return segments;
}
/** The broker's fields first, then the agent's, each kind in the order it came. */
export function reviewOrder(fields: readonly ReviewField[]): readonly ReviewField[] {
    return [...fields.filter(field => field.source === 'broker'), ...fields.filter(field => field.source === 'agent')];
}
/** A value as inert text in an isolated direction, each hidden character shown as its code point. */
export function revealedValue(value: string): HTMLElement {
    const parts = revealSegments(value).map(segment => 'hidden' in segment
        ? h('span', { class: 'task-approval-hidden', title: `Hidden character ${segment.hidden}` }, `⟨${segment.hidden}⟩`)
        : segment.text);
    return h('bdi', { dir: 'auto', class: 'task-approval-value' }, ...parts);
}
/** A request's fields, in review order, the agent's labeled as the agent's. */
export function reviewFields(fields: readonly ReviewField[]): HTMLElement {
    const list = h('dl', { class: 'task-approval-fields' });
    for (const field of reviewOrder(fields)) {
        const label = field.source === 'agent'
            ? h('dt', {}, h('span', {}, field.label), h('span', { class: 'task-approval-source' }, 'From the agent'))
            : h('dt', {}, h('span', {}, field.label));
        list.append(h('div', { class: `task-approval-field ${field.source}` }, label, h('dd', {}, revealedValue(field.value))));
    }
    return list;
}
