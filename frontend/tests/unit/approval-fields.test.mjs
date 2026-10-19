import test from 'node:test';
import assert from 'node:assert/strict';
import { revealSegments, reviewOrder } from '../../dist/app/features/approvals/fields.js';
const RLO = String.fromCodePoint(0x202e);
const ZWJ = String.fromCodePoint(0x200d);
test('visible text stays whole, and plain text is one piece', () => {
    assert.deepEqual(revealSegments('Build the zine'), [{ text: 'Build the zine' }]);
    assert.deepEqual(revealSegments(''), []);
    // Ordinary right-to-left text, emoji and accents are text, not hidden.
    for (const text of ['שלום', 'مرحبا', 'café', 'é', '👩‍', '日本語'])
        assert.ok(revealSegments(text).every(segment => 'text' in segment || segment.hidden === 'U+200D'), text);
});
test('a direction override, and every other hidden character, is shown as its code point', () => {
    assert.deepEqual(revealSegments(`setup${RLO}txt.exe`), [{ text: 'setup' }, { hidden: 'U+202E' }, { text: 'txt.exe' }]);
    assert.deepEqual(revealSegments(`${ZWJ}${ZWJ}`), [{ hidden: 'U+200D' }, { hidden: 'U+200D' }]);
    const hidden = [0x200b, 0x200e, 0x200f, 0x202a, 0x202c, 0x2066, 0x2069, 0xfeff, 0x00ad, 0x061c, 0x3164, 0xfe0f, 0xe0041, 0xe0100];
    for (const point of hidden) {
        const name = `U+${point.toString(16).toUpperCase().padStart(4, '0')}`;
        assert.deepEqual(revealSegments(`a${String.fromCodePoint(point)}b`), [{ text: 'a' }, { hidden: name }, { text: 'b' }], name);
    }
    // Nothing is dropped: the visible text and the hidden characters account for every one.
    const mixed = `pay ${RLO}100${ZWJ} to x`;
    const shown = revealSegments(mixed).map(segment => 'text' in segment ? segment.text : String.fromCodePoint(Number.parseInt(segment.hidden.slice(2), 16))).join('');
    assert.equal(shown, mixed);
});
test('the review shows the broker\'s fields first, then the agent\'s, each in the order it came', () => {
    const fields = [
        { label: 'Why', value: 'a', source: 'agent' },
        { label: 'Operation', value: 'b', source: 'broker' },
        { label: 'Note', value: 'c', source: 'agent' },
        { label: 'Target', value: 'd', source: 'broker' },
    ];
    assert.deepEqual(reviewOrder(fields).map(field => field.label), ['Operation', 'Target', 'Why', 'Note']);
    assert.deepEqual(reviewOrder([]), []);
});
