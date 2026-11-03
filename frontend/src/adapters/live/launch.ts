// How the app starts live: when `hatchling-host` served the page, the address
// it printed carries the UI token in its fragment, `#hatchling-ui=<token>`. A
// browser never sends a fragment to a server. The token is taken from it and
// the fragment cleared at once, before the app's own hash routing reads it,
// so it is neither routed nor kept in the history. The tab keeps it in its own
// session storage, so a reload stays connected rather than falling back to the
// sample workspace at the install's address.
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
/** Where a tab keeps its token across a reload. */
export interface TokenStore {
    get(): string | undefined;
    /** Keep `token`; whether it was kept. */
    set(token: string): boolean;
}
/** The key the token is kept under. */
export const TOKEN_KEY = 'hatchling-ui-token';
/**
 * The tab's session storage (`storage`) as a {@link TokenStore}: one tab's,
 * gone when it closes. A read or write that fails keeps nothing, and what's
 * kept is used only if it is still a token.
 */
export function sessionTokens(storage: () => Storage): TokenStore {
    return {
        get: () => {
            try {
                return tokenIn(`${TOKEN_FRAGMENT}${storage().getItem(TOKEN_KEY) ?? ''}`);
            }
            catch {
                return undefined;
            }
        },
        set: (token) => {
            try {
                storage().setItem(TOKEN_KEY, token);
                return true;
            }
            catch {
                // Kept for this load only: a reload opens the sample workspace.
                return false;
            }
        },
    };
}
/** This browser tab's session storage, as a {@link TokenStore}. */
export function tabTokens(): TokenStore {
    return sessionTokens(() => globalThis.sessionStorage);
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
 * The live adapter, when the page holds a UI token, or the tab kept one in
 * `tokens` from an earlier load; `undefined` otherwise, and the app starts as
 * before. A token in the fragment is kept, and the fragment cleared through
 * `replace`.
 */
export function liveFromPlace(place: Place, replace: (address: string) => void, tokens?: TokenStore): LiveAdapter | undefined {
    const given = tokenIn(place.hash);
    if (given !== undefined) {
        replace(`${place.pathname}${place.search}`);
        tokens?.set(given);
    }
    const token = given ?? tokens?.get();
    if (token === undefined)
        return undefined;
    return new LiveAdapter(new WebSocketTransport(channelUrl(place)), 'hatchling-frontend', token);
}
