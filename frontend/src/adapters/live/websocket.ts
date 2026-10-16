// The live adapter's channel to `hatchling-host`: one WebSocket on the page's
// own origin, each frame one JSON request or response, in order. The host
// carries every frame to the broker unchanged; the broker decides what they may do.
import type { TransportHandlers, UiTransport } from './client.js';
/** The part of a WebSocket the transport uses, so a test can stand in for one. */
export interface SocketLike {
    onopen: (() => void) | null;
    onmessage: ((data: unknown) => void) | null;
    onclose: (() => void) | null;
    send(text: string): void;
    close(): void;
}
const ENDED = 'The connection to Hatchling ended.';
const NOT_TEXT = 'Hatchling sent something other than text, so the app disconnected.';
const UNSENDABLE = 'The app could not send to Hatchling, so it disconnected.';
export class WebSocketTransport implements UiTransport {
    readonly #url: string;
    readonly #open: (url: string) => SocketLike;
    #socket: SocketLike | null = null;
    #end: ((detail: string) => void) | null = null;
    /** `open` makes the socket; by default, the browser's own WebSocket. */
    constructor(url: string, open: (url: string) => SocketLike = browserSocket) {
        this.#url = url;
        this.#open = open;
    }
    connect(handlers: TransportHandlers): void {
        const socket = this.#open(this.#url);
        let ended = false;
        const end = (detail: string): void => {
            if (ended)
                return;
            ended = true;
            if (this.#socket === socket) {
                this.#socket = null;
                this.#end = null;
            }
            socket.close();
            handlers.closed(detail);
        };
        this.#socket = socket;
        this.#end = end;
        socket.onopen = () => { handlers.opened(); };
        socket.onmessage = data => {
            if (typeof data === 'string')
                handlers.received(data);
            else
                end(NOT_TEXT);
        };
        socket.onclose = () => { end(ENDED); };
    }
    send(text: string): void {
        const socket = this.#socket;
        if (socket === null)
            return;
        try {
            socket.send(text);
        }
        catch {
            this.#end?.(UNSENDABLE);
        }
    }
    close(): void {
        this.#end?.(ENDED);
    }
}
/** The browser's WebSocket, behind the shape the transport uses. */
export function browserSocket(url: string): SocketLike {
    const socket = new WebSocket(url);
    const like: SocketLike = {
        onopen: null,
        onmessage: null,
        onclose: null,
        send: text => { socket.send(text); },
        close: () => { socket.close(); },
    };
    socket.addEventListener('open', () => { like.onopen?.(); });
    socket.addEventListener('message', event => { like.onmessage?.(event.data); });
    socket.addEventListener('close', () => { like.onclose?.(); });
    socket.addEventListener('error', () => { like.onclose?.(); });
    return like;
}
