// How the Machine view starts: `hatchling-host` printed its address with the
// operator token in the fragment, `#hatchling-operator=<token>`, which a
// browser never sends to a server. The token is taken and the fragment cleared
// at once, so it is neither kept in the history nor shown in the address bar.
import type { Place } from '../live/launch.js';
import { WebSocketTransport, type SocketLike, browserSocket } from '../live/websocket.js';
import { type Every, type Machine, OperatorClient, browserEvery } from './client.js';
/** The fragment's prefix that marks the operator token. */
export const OPERATOR_FRAGMENT = '#hatchling-operator=';
/** The token in `hash`, when it holds one: hexadecimal, 16 to 128 digits. */
export function operatorTokenIn(hash: string): string | undefined {
    if (!hash.startsWith(OPERATOR_FRAGMENT))
        return undefined;
    const token = hash.slice(OPERATOR_FRAGMENT.length);
    return /^[0-9a-f]{16,128}$/.test(token) ? token : undefined;
}
/** The address of the host's operator channel, on the page's own origin. */
export function operatorChannelUrl(place: {
    readonly protocol: string;
    readonly host: string;
}): string {
    return `${place.protocol === 'https:' ? 'wss' : 'ws'}://${place.host}/operator`;
}
/**
 * The operator client, started, when the page holds an operator token;
 * `undefined` otherwise. The fragment is cleared through `replace`.
 */
export function operatorFromPlace(place: Place, replace: (address: string) => void, changed: (machine: Machine) => void, open: (url: string) => SocketLike = browserSocket, every: Every = browserEvery): OperatorClient | undefined {
    const token = operatorTokenIn(place.hash);
    if (token === undefined)
        return undefined;
    replace(`${place.pathname}${place.search}`);
    const client = new OperatorClient(new WebSocketTransport(operatorChannelUrl(place), open), token, changed, every);
    client.start();
    return client;
}
