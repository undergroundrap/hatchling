// The composer's context ring and panel (WO-056): the words and numbers
// they show, from the broker's counts.
import test from 'node:test';
import assert from 'node:assert/strict';
import { contextRows, countedNote, exactWords, headline, shareOf, tokensShort, windowSize } from '../../dist/app/features/chat/context.js';
import { SAMPLE_CONTEXT, ScriptedUsage } from '../../dist/app/adapters/scripted-usage.js';
const use = (tokens, window, parts = null) => ({ tokens, window, onCpu: null, memoryLow: false, model: null, parts });
test('how full the window was, in whole percent, at most 100, and none without a window', () => {
    assert.equal(shareOf(use(8320, 65536)), 13);
    assert.equal(shareOf(use(65535, 65536)), 100);
    assert.equal(shareOf(use(90000, 65536)), 100, 'never past full');
    assert.equal(shareOf(use(0, 65536)), 0);
    assert.equal(shareOf(use(100, null)), null);
    assert.equal(shareOf(use(100, 0)), null, 'a window of nothing is no window');
});
test('the exact numbers say what they are, with or without a window', () => {
    assert.equal(exactWords(use(8320, 65536)), '8.3K of 64K tokens of context (13%) used on the last answer');
    assert.equal(exactWords(use(700, null)), '700 tokens of context used on the last answer; the model’s window isn’t known');
    assert.equal(headline(use(8320, 65536)), '13% used · 8.3K of 64K');
    assert.equal(headline(use(912000, 1000000)), '91% used · 912K of 1M');
    assert.equal(headline(use(700, null)), '700 used');
    assert.equal(windowSize(131072), '128K');
    assert.equal(windowSize(1000000), '1M');
    assert.equal(tokensShort(1234567), '1.2M');
});
test('what filled it is about; what was free is exact; with neither, nothing', () => {
    const parts = { system: 3100, tools: 2400, memory: 720, conversation: 2100 };
    assert.deepEqual(contextRows(use(8320, 65536, parts)), [
        { label: 'Conversation', tokens: '2.1K', about: true },
        { label: 'Tools', tokens: '2.4K', about: true },
        { label: 'Memory', tokens: '720', about: true },
        { label: 'System', tokens: '3.1K', about: true },
        { label: 'Free', tokens: '57.2K', about: false },
    ]);
    assert.deepEqual(contextRows(use(8320, 65536)), [{ label: 'Free', tokens: '57.2K', about: false }]);
    assert.deepEqual(contextRows(use(70000, 65536)), [{ label: 'Free', tokens: '0', about: false }], 'never less than nothing');
    assert.deepEqual(contextRows(use(100, null, parts)).map(row => row.label), ['Conversation', 'Tools', 'Memory', 'System']);
    assert.deepEqual(contextRows(use(100, null)), []);
    assert.match(countedNote(use(1, 2, parts)), /splits it by the size of each part of what it sent, so each part is about right/);
    assert.match(countedNote(use(1, 2)), /What filled it isn’t known for this answer/);
});
test('the preview counts nothing, says so, and shows its sample in every chat', async () => {
    const usage = new ScriptedUsage();
    assert.deepEqual(usage.current(), { kind: 'failed', detail: 'The local preview runs no model, so it counts no usage. Connect to Hatchling to see yours.' });
    await usage.refresh();
    assert.deepEqual(await usage.setLimits({ daily: 1, monthly: null }), { ok: false, message: 'The local preview runs no model, so it counts no usage. Connect to Hatchling to see yours.' });
    assert.equal(usage.context('main'), SAMPLE_CONTEXT);
    assert.equal(usage.context('reference'), SAMPLE_CONTEXT);
    const { system, tools, memory, conversation } = SAMPLE_CONTEXT.parts;
    assert.equal(system + tools + memory + conversation, SAMPLE_CONTEXT.tokens, 'the sample adds up, as the broker\'s do');
});
