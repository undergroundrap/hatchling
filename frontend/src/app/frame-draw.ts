/** What draws at the next frame: the page's window, or a test's. */
export interface Frames {
    requestAnimationFrame(callback: () => void): number;
    cancelAnimationFrame(handle: number): void;
}
/**
 * Draws a page at most once a frame for the changes it's told of, however many
 * arrive before the frame (#528). A long chat replays every event as it opens,
 * and drawing the whole page for each one froze it for minutes. A page that's
 * hidden runs no frames, so it draws when it's shown.
 */
export class FrameDraw {
    #frame: number | undefined;
    constructor(private readonly draw: () => void, private readonly frames: Frames) { }
    /** Something changed: draw it at the next frame, once. */
    readonly soon = (): void => {
        if (this.#frame === undefined)
            this.#frame = this.frames.requestAnimationFrame(() => { this.#frame = undefined; this.draw(); });
    };
    /** Draw now what's waiting for the next frame, if anything is. */
    readonly flush = (): void => {
        if (this.#frame !== undefined) {
            this.cancel();
            this.draw();
        }
    };
    /** What's waiting needn't wait: the page is being drawn now, or never will be again. */
    cancel(): void {
        if (this.#frame !== undefined) {
            this.frames.cancelAnimationFrame(this.#frame);
            this.#frame = undefined;
        }
    }
}
