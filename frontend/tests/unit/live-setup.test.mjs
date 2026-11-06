import assert from 'node:assert/strict';
import test from 'node:test';
import { LiveSetup, setupOf } from '../../dist/app/adapters/live/setup.js';
import { ago, setupFinished, setupProgress, setupReminder, setupSteps, STEP_STATUS } from '../../dist/app/session/setup.js';
const NOW = 2000000000;
const DAY = 86400;
const model = { endpoint: 'http://127.0.0.1:11434/v1', configured: 'qwen3.5:9b', chosen: null, available: null, window: null, size: null, onCpu: null, other: null, switched: false };
const bare = { tier: 'file', keys: 0, codes: 0, codesUnconfirmed: false, lastBackup: null };
const keyed = { ...bare, tier: 'hardware_key', keys: 2, codes: 10 };
const step = (id, setup = bare, noKey = false, chosen = model) => setupSteps(chosen, setup, noKey, NOW).find(found => found.id === id);
test('a new install has what it needs left to do, in order', () => {
    const steps = setupSteps({ ...model, endpoint: null, configured: null }, bare, false, NOW);
    assert.deepEqual(steps.map(found => [found.id, found.status]), [['model', 'next'], ['vault', 'next'], ['backup', 'next']]);
    assert.equal(setupFinished(steps), false);
    assert.equal(setupProgress(steps), '0 of 3 done');
    assert.equal(steps[0].commands.length, 2, 'hosted or local');
    assert.match(steps[1].commands[0].command, /vault-keyring setup/u);
    assert.match(steps[2].commands[0].command, /hatchling-broker backup/u);
    assert.deepEqual(steps.map(found => found.optional), [false, true, false]);
});
test('no command in the checklist carries a secret, and each is run by hand', () => {
    const steps = setupSteps({ ...model, endpoint: null }, bare, false, NOW);
    for (const found of steps)
        for (const item of found.commands)
            assert.match(item.command, /^sudo hatchling-broker /u);
});
test('the model is done with a server and a name, the owner\'s choice first', () => {
    assert.equal(step('model', bare, false, { ...model, configured: null }).status, 'next');
    assert.equal(step('model', bare, false, { ...model, endpoint: null }).status, 'next');
    const done = step('model');
    assert.equal(done.status, 'done');
    assert.equal(done.said, 'Hatchling uses qwen3.5:9b, on this machine.');
    assert.deepEqual(done.commands, []);
    assert.match(step('model', bare, false, { ...model, chosen: 'gemma4:12b' }).said, /gemma4:12b/u);
    assert.match(step('model', bare, false, { ...model, endpoint: 'https://openrouter.ai/api/v1' }).said, /hosted at openrouter\.ai/u);
});
test('with no key, the vault is left to do until the owner says they have none', () => {
    const open = step('vault');
    assert.equal(open.status, 'next');
    assert.match(open.said, /Hatchling works without one/u);
    const skipped = step('vault', bare, true);
    assert.equal(skipped.status, 'skipped');
    assert.match(skipped.said, /Skipped/u);
    assert.equal(skipped.commands.length, 1, 'still there to take up');
    assert.equal(step('vault', { ...bare, tier: 'totp' }).status, 'done');
    assert.deepEqual(step('vault', { ...bare, tier: 'totp' }).commands, []);
});
test('the vault\'s keys and codes are said as they stand', () => {
    const two = step('vault', keyed);
    assert.equal(two.status, 'done');
    assert.equal(two.said, '2 security keys open your vault, any one of them, with 10 backup codes to fall back on.');
    assert.deepEqual(two.commands, []);
    const one = step('vault', { ...keyed, keys: 1, codes: 1 });
    assert.equal(one.status, 'done');
    assert.match(one.said, /One security key.*1 backup code to fall back on.*Add a spare/u);
    assert.equal(one.commands.length, 1);
    const unsaved = step('vault', { ...keyed, codesUnconfirmed: true });
    assert.equal(unsaved.status, 'next');
    assert.match(unsaved.said, /2 security keys open your vault.*never confirmed saved/u);
    const spent = step('vault', { ...keyed, keys: 1, codes: 0 });
    assert.equal(spent.status, 'next');
    assert.match(spent.said, /1 security key opens your vault.*no backup codes/u);
    assert.equal(step('vault', { ...keyed, keys: 1 }, true).status, 'done', 'a key beats having said there is none');
});
test('a backup is done once there is one, and says when', () => {
    assert.equal(step('backup').status, 'next');
    const done = step('backup', { ...bare, lastBackup: NOW - 3 * DAY });
    assert.equal(done.status, 'done');
    assert.equal(done.said, 'Your last backup was made 3 days ago.');
    assert.equal(ago(NOW - 1, NOW), 'today');
    assert.equal(ago(NOW - DAY + 1, NOW), 'today');
    assert.equal(ago(NOW - DAY, NOW), 'yesterday');
    assert.equal(ago(NOW - 2 * DAY + 1, NOW), 'yesterday');
    assert.equal(ago(NOW - 2 * DAY, NOW), '2 days ago');
});
test('a Windows install points to the Start menu, and types what it must in PowerShell', () => {
    const steps = setupSteps(model, { ...bare, windowsSetup: true }, false, NOW);
    const vault = steps.find(found => found.id === 'vault');
    const backup = steps.find(found => found.id === 'backup');
    assert.deepEqual(vault.commands, [], 'no sudo: its account has none');
    assert.match(vault.said, /Open the Start menu, then Hatchling Tools, then Protect your vault\.$/);
    assert.deepEqual(backup.commands, []);
    assert.match(backup.said, /then Make a backup\.$/);
    const unset = { ...model, endpoint: null };
    const first = setupSteps(unset, { ...bare, windowsSetup: true }, false, NOW)[0];
    assert.ok(first.commands.length > 0);
    assert.ok(first.commands.every(found => found.command.startsWith('wsl -d hatchling-wsl -u root -- hatchling-broker')), 'a model has no Start menu item');
    assert.ok(setupSteps(unset, bare, false, NOW)[0].commands.every(found => found.command.startsWith('sudo ')));
    const done = setupSteps(model, { ...bare, lastBackup: NOW, windowsSetup: true }, true, NOW);
    assert.equal(done.find(found => found.id === 'backup').said, 'Your last backup was made today.', 'done needs no directions');
});
test('skipping counts as dealt with, and finishes it', () => {
    const steps = setupSteps(model, { ...bare, lastBackup: NOW }, true, NOW);
    assert.equal(setupFinished(steps), true);
    assert.equal(setupProgress(steps), '3 of 3 done');
    const left = setupSteps(model, { ...bare, lastBackup: NOW }, false, NOW);
    assert.equal(setupFinished(left), false);
    assert.equal(setupProgress(left), '2 of 3 done');
    assert.equal(setupReminder(left).title, 'Finish setting up');
    assert.equal(setupReminder(left).note, `2 of 3 done. Next: ${left.find(found => found.status === 'next').title.toLowerCase()}.`);
    assert.equal(setupReminder(steps).title, 'You’re all set');
    assert.deepEqual(Object.keys(STEP_STATUS).sort(), ['done', 'next', 'skipped']);
});
class Client {
    connected = 'offline';
    listeners = [];
    replies = [];
    asked = [];
    connection() { return { kind: this.connected }; }
    subscribe(listener) { this.listeners.push(listener); return () => undefined; }
    request(id, action) { this.asked.push({ id, action }); return Promise.resolve(this.replies.shift()); }
    change(kind) {
        this.connected = kind;
        for (const listener of this.listeners)
            listener();
    }
}
const wire = { vault: { tier: 'hardware_key', keys: 1, codes: 9 } };
const answered = result => ({ kind: 'answered', result });
function live() {
    const client = new Client();
    let changes = 0;
    let serial = 0;
    const port = new LiveSetup(client, () => { changes += 1; }, () => `id-${++serial}`);
    return { client, port, changes: () => changes };
}
const settle = () => new Promise(resolve => setTimeout(resolve, 0));
test('the broker\'s word, in the app\'s', () => {
    assert.deepEqual(setupOf(wire), { tier: 'hardware_key', keys: 1, codes: 9, codesUnconfirmed: false, lastBackup: null, windowsSetup: false });
    assert.deepEqual(setupOf({ vault: { ...wire.vault, codes_unconfirmed: true }, last_backup: 7, windows_setup: true }), { tier: 'hardware_key', keys: 1, codes: 9, codesUnconfirmed: true, lastBackup: 7, windowsSetup: true });
});
test('it reads each time the connection opens, and only then', async () => {
    const { client, port, changes } = live();
    assert.deepEqual(port.current(), { kind: 'unread' });
    client.replies.push(answered({ result: 'setup', setup: wire }), answered({ result: 'setup', setup: wire }));
    client.change('ready');
    await settle();
    assert.deepEqual(client.asked, [{ id: 'id-1', action: { action: 'read_setup' } }]);
    assert.equal(port.current().kind, 'read');
    assert.equal(port.current().setup.keys, 1);
    assert.equal(changes(), 1);
    client.change('ready');
    await settle();
    assert.equal(client.asked.length, 1, 'still the same connection');
    client.change('offline');
    client.change('ready');
    await settle();
    assert.equal(client.asked.length, 2, 'a new one');
});
test('what can\'t be read says so, and nothing is made up', async () => {
    const { client, port, changes } = live();
    client.replies.push({ kind: 'unsent', detail: 'Not connected.' });
    await port.refresh();
    assert.deepEqual(port.current(), { kind: 'failed', detail: 'Not connected.' });
    client.replies.push(answered({ result: 'refused', reason: 'unavailable' }));
    await port.refresh();
    assert.match(port.current().detail, /hatchling-broker doctor/u);
    client.replies.push(answered({ result: 'model', model: {} }));
    await port.refresh();
    assert.match(port.current().detail, /something else/u);
    assert.equal(changes(), 3);
});
test('with no answer, what was read still stands', async () => {
    const { client, port, changes } = live();
    client.replies.push(answered({ result: 'setup', setup: wire }), { kind: 'unknown', detail: 'Gone.' });
    await port.refresh();
    await port.refresh();
    assert.equal(port.current().kind, 'read');
    assert.equal(changes(), 2);
});
test('a choice lasts in the browser, and for the page where the browser keeps nothing', () => {
    const held = new Map();
    globalThis.window = { localStorage: { getItem: key => held.get(key) ?? null, setItem: (key, value) => { held.set(key, value); } } };
    const { port, changes } = live();
    assert.equal(port.remembers('closed'), false);
    port.remember('closed');
    assert.equal(port.remembers('closed'), true);
    assert.equal(port.remembers('no-key'), false, 'each on its own');
    assert.deepEqual([...held.keys()], ['hatchling.setup.closed']);
    assert.equal(held.get('hatchling.setup.closed'), 'yes');
    assert.equal(live().port.remembers('closed'), true, 'a later visit');
    held.set('hatchling.setup.no-key', 'no');
    assert.equal(live().port.remembers('no-key'), false, 'only a yes counts');
    assert.equal(changes(), 0);
    const refuse = () => { throw new Error('denied'); };
    globalThis.window = { localStorage: { getItem: refuse, setItem: refuse } };
    const closed = live();
    assert.equal(closed.port.remembers('closed'), false);
    closed.port.remember('closed');
    assert.equal(closed.port.remembers('closed'), true, 'for this page');
    assert.equal(closed.changes(), 1);
    assert.equal(live().port.remembers('closed'), false, 'and no longer');
    delete globalThis.window;
});
