import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { YOUR_SECURITY } from '../../dist/app/features/settings/your-security-text.js';
import { onThisInstall, roomsOnThisInstall } from '../../dist/app/features/settings/security.js';
const page = readFileSync(new URL('../../../docs/your-security.md', import.meta.url), 'utf8');
const REPO = 'https://github.com/undergroundrap/hatchling/blob/main';
/** The docs page as the app shows it: no title, the dialog has one, and links that work from there. */
function asShown(text) {
    return text.replace(/^# Your security\n\n/u, '')
        .replaceAll('](../SECURITY.md)', `](${REPO}/SECURITY.md)`)
        .replace(/\]\((?!https:)([^)]+)\)/gu, `](${REPO}/docs/$1)`)
        .trimEnd();
}
test('the app shows the docs page, word for word', () => {
    assert.equal(YOUR_SECURITY, asShown(page));
});
test('the page never says antivirus, and its limits are the same size as its claims', () => {
    assert.doesNotMatch(page, /antivirus/iu);
    assert.ok(page.includes('## Who does what') && page.includes('## What it can\'t do'));
});
test('the page says only what is true of this install', () => {
    const read = (tier) => ({ kind: 'read', setup: { tier, keys: 0, codes: 0, codesUnconfirmed: false, lastBackup: null, windowsSetup: false } });
    assert.match(onThisInstall(read('hardware_key')), /security key or a backup code/u);
    assert.match(onThisInstall(read('totp')), /stolen disk still opens/u);
    assert.match(onThisInstall(read('file')), /same disk.*Protect your vault/u);
    assert.doesNotMatch(onThisInstall(read('file')), /security key/u);
    for (const unknown of [undefined, { kind: 'unread' }, { kind: 'failed', detail: 'x' }]) {
        assert.equal(onThisInstall(unknown), null, 'said only of what has been read');
        assert.equal(roomsOnThisInstall(unknown), null, 'said only of what has been read');
    }
    // Rooms held by process limits alone are the weaker way, and said; a cgroup of their own goes without saying.
    const rooms = (processLimitsOnly) => ({ kind: 'read', setup: { ...read('file').setup, processLimitsOnly } });
    assert.match(roomsOnThisInstall(rooms(true)), /process limits alone.*no cgroup to bound a room's memory and CPU.*doctor/u);
    assert.equal(roomsOnThisInstall(rooms(false)), null);
});
