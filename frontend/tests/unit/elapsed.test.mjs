import test from 'node:test';
import assert from 'node:assert/strict';
import { elapsedWords } from '../../dist/app/components/elapsed.js';
test('the time a run has taken is said in a few plain words, never as a fraction of anything', () => {
    const cases = [
        [0, '0 s'],
        [999, '0 s'],
        [1000, '1 s'],
        [59999, '59 s'],
        [60000, '1 min 0 s'],
        [125000, '2 min 5 s'],
        [3599000, '59 min 59 s'],
        [3600000, '1 h 0 min'],
        [3840000, '1 h 4 min'],
        [90000000, '25 h 0 min'],
    ];
    for (const [ms, words] of cases)
        assert.equal(elapsedWords(ms), words, String(ms));
});
