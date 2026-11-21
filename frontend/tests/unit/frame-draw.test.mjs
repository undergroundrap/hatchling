// FrameDraw: what the broker changes is drawn at the next frame, once
// however many changes arrive before it (#528).
import test from 'node:test';
import assert from 'node:assert/strict';
import { FrameDraw } from '../../dist/app/app/frame-draw.js';
/** Frames a test runs by hand: `run()` is the next frame. */
function frames() {
    const waiting = new Map();
    let serial = 0;
    return {
        requested: 0,
        cancelled: [],
        requestAnimationFrame(callback) { this.requested++; waiting.set(++serial, callback); return serial; },
        cancelAnimationFrame(handle) { this.cancelled.push(handle); waiting.delete(handle); },
        waiting: () => waiting.size,
        run() {
            const now = [...waiting.values()];
            waiting.clear();
            for (const callback of now)
                callback();
        },
    };
}
function setup() {
    const clock = frames();
    let draws = 0;
    const frame = new FrameDraw(() => { draws++; }, clock);
    return { clock, frame, draws: () => draws };
}
test('many changes before a frame are drawn once, at the frame', () => {
    const { clock, frame, draws } = setup();
    for (let i = 0; i < 270; i++)
        frame.soon();
    assert.equal(clock.requested, 1, 'one frame asked for');
    assert.equal(draws(), 0, 'nothing drawn before the frame');
    clock.run();
    assert.equal(draws(), 1);
    clock.run();
    assert.equal(draws(), 1, 'a frame with nothing new draws nothing');
});
test('a change after the frame waits for the next one', () => {
    const { clock, frame, draws } = setup();
    frame.soon();
    clock.run();
    frame.soon();
    frame.soon();
    assert.equal(clock.requested, 2);
    assert.equal(draws(), 1);
    clock.run();
    assert.equal(draws(), 2);
});
test('flushing draws what is waiting now, and its frame draws nothing more', () => {
    const { clock, frame, draws } = setup();
    frame.flush();
    assert.equal(draws(), 0, 'nothing waiting, nothing drawn');
    frame.soon();
    frame.flush();
    assert.equal(draws(), 1);
    assert.deepEqual(clock.cancelled, [1]);
    assert.equal(clock.waiting(), 0);
    frame.flush();
    assert.equal(draws(), 1, 'drawn once: nothing waits any more');
    frame.soon();
    assert.equal(clock.requested, 2, 'the next change asks for a frame of its own');
});
test('cancelling drops the waiting frame without drawing: the page drew, or is gone', () => {
    const { clock, frame, draws } = setup();
    frame.cancel();
    assert.deepEqual(clock.cancelled, [], 'nothing waiting, nothing cancelled');
    frame.soon();
    frame.cancel();
    assert.deepEqual(clock.cancelled, [1]);
    clock.run();
    assert.equal(draws(), 0);
    frame.soon();
    assert.equal(clock.requested, 2);
    clock.run();
    assert.equal(draws(), 1);
});
