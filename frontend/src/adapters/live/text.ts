// Wording helpers for the live adapter . Strings from the agent, its
// tools and the broker are untrusted: the views render them through the
// markdown renderer, whose allowlist (emphasis, code, lists, quotes, tables,
// links) is safe against markup but would still turn `[label](target)` into
// a link or an artifact button. Untrusted text therefore never forms one: the
// adapter separates `]` from `(`, so the renderer shows the label and the
// target as the plain text they are.
/** Untrusted text with every link form broken apart; nothing else changes. */
export function inertLinks(text: string): string {
    return text.replaceAll('](', '] (');
}
/**
 * Untrusted text as a quotation inside a report Hatchling writes: set apart
 * from Hatchling's own words, and read inline only (a quote holds no
 * headings, lists, tables or code blocks).
 */
export function quoted(text: string): string {
    return inertLinks(text).replaceAll('\r\n', '\n').split('\n').map(line => `> ${line}`).join('\n');
}
/** `1 step`, `2 steps`. */
export function count(value: number, one: string, many: string): string {
    return `${value} ${value === 1 ? one : many}`;
}
/** The wall-clock time of an RFC 3339 timestamp as `HH:MM` on the person's clock, or `''` when it has none. */
export function clockTime(timestamp: string): string {
    const at = Date.parse(timestamp);
    if (!Number.isFinite(at))
        return '';
    // The person's own clock, not UTC: the broker's times are UTC instants.
    const local = new Date(at);
    return `${String(local.getHours()).padStart(2, '0')}:${String(local.getMinutes()).padStart(2, '0')}`;
}
/** Milliseconds between two timestamps; `null` unless both are real and in order. Durations are derived, never emitted. */
export function between(start: string | null, end: string): number | null {
    if (start === null)
        return null;
    const elapsed = Date.parse(end) - Date.parse(start);
    return Number.isFinite(elapsed) && elapsed >= 0 ? elapsed : null;
}
