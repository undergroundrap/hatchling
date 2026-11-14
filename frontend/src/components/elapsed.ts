import { h } from './dom.js';
/** How long `ms` is, in a few words: `45 s`, `2 min 5 s`, `1 h 4 min`. */
export function elapsedWords(ms: number): string {
    const seconds = Math.floor(ms / 1000);
    if (seconds < 60)
        return `${seconds} s`;
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60)
        return `${minutes} min ${seconds % 60} s`;
    return `${Math.floor(minutes / 60)} h ${minutes % 60} min`;
}
/**
 * The time since `since`, counted up each second beside a status that says
 * what is going on. It is a real time, from when the broker said the work
 * began, never a progress figure. It isn't read out: the status beside it
 * is, when it changes. It stops counting when it leaves the page. With a
 * time that can't be read there is nothing to show.
 */
export function elapsedSince(since: string, now: () => number = Date.now): HTMLElement | null {
    const started = Date.parse(since);
    if (Number.isNaN(started))
        return null;
    const node = h('span', { class: 'turn-elapsed', 'aria-hidden': 'true' });
    const show = (): void => {
        node.textContent = `· ${elapsedWords(Math.max(0, now() - started))}`;
    };
    show();
    const timer = setInterval(() => {
        if (node.isConnected)
            show();
        else
            clearInterval(timer);
    }, 1000);
    return node;
}
