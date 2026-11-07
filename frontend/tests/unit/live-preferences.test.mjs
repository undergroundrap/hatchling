import assert from 'node:assert/strict';
import test from 'node:test';
import { keepPreferences, rememberedPreferences } from '../../dist/app/adapters/live/preferences.js';
function storage(initial) {
    const held = new Map(initial === undefined ? [] : [['hatchling-preferences', initial]]);
    globalThis.window = {
        localStorage: {
            getItem: key => held.get(key) ?? null,
            setItem: (key, value) => { held.set(key, value); },
        },
    };
    return held;
}
test('a choice comes back on the next visit', () => {
    storage();
    const chosen = { theme: 'light', accent: 'blue', interfaceSize: 'large', keepChatPanelVisible: false };
    assert.equal(keepPreferences(chosen), true);
    assert.deepEqual(rememberedPreferences(), chosen);
});
test('nothing kept means no preferences', () => {
    storage();
    assert.deepEqual(rememberedPreferences(), {});
});
test('what storage holds is checked, field by field', () => {
    storage(JSON.stringify({ theme: 'neon', accent: 'url(x)', interfaceSize: 9, keepChatPanelVisible: 'yes', extra: true }));
    assert.deepEqual(rememberedPreferences(), {});
    storage(JSON.stringify({ theme: 'system', accent: 'url(x)' }));
    assert.deepEqual(rememberedPreferences(), { theme: 'system' });
});
test('junk in storage is the same as nothing', () => {
    storage('not json');
    assert.deepEqual(rememberedPreferences(), {});
    storage('null');
    assert.deepEqual(rememberedPreferences(), {});
    storage('7');
    assert.deepEqual(rememberedPreferences(), {});
});
test('a browser that keeps nothing still works', () => {
    globalThis.window = { get localStorage() { throw new Error('denied'); } };
    assert.deepEqual(rememberedPreferences(), {});
    assert.equal(keepPreferences({ theme: 'dark', accent: 'white', interfaceSize: 'comfortable', keepChatPanelVisible: true }), false);
});
