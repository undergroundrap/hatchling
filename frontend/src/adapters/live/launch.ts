// How the app starts live: when `hatchling-host` served the page, the address
// it printed carries the UI token in its fragment, `#hatchling-ui=<token>`. A
// browser never sends a fragment to a server. The token is taken from it and
// the fragment cleared at once, before the app's own hash routing reads it,
// so it is neither routed nor kept in the history.
import { LiveAdapter } from './adapter.js';
import { WebSocketTransport } from './websocket.js';
/** The fragment's prefix that marks the UI token. */
export const TOKEN_FRAGMENT = '#hatchling-ui=';
/** The token in `hash`, when it holds one: hexadecimal, 16 to 128 digits. */
export function tokenIn(hash: string): string | undefined {
    if (!hash.startsWith(TOKEN_FRAGMENT))
        return undefined;
    const token = hash.slice(TOKEN_FRAGMENT.length);
    return /^[0-9a-f]{16,128}$/.test(token) ? token : undefined;
}
/** The address of the host's channel, on the page's own origin. */
export function channelUrl(place: {
    readonly protocol: string;
    readonly host: string;
}): string {
    return `${place.protocol === 'https:' ? 'wss' : 'ws'}://${place.host}/ui`;
}
/** Where the page is, and how to rewrite its address without the fragment. */
export interface Place {
    readonly protocol: string;
    readonly host: string;
    readonly pathname: string;
    readonly search: string;
    readonly hash: string;
}
/**
 * The live adapter, when the page holds a UI token; `undefined` otherwise, and
 * the app starts as before. The fragment is cleared through `replace`.
 */
export function liveFromPlace(place: Place, replace: (address: string) => void): LiveAdapter | undefined {
    const token = tokenIn(place.hash);
    if (token === undefined)
        return undefined;
    replace(`${place.pathname}${place.search}`);
    return new LiveAdapter(new WebSocketTransport(channelUrl(place)), 'hatchling-frontend', token);
}
