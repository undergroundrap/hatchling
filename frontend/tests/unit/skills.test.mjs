import test from 'node:test';
import assert from 'node:assert/strict';
import { createFixtures } from '../../dist/app/adapters/fixtures.js';
import { ScriptedAdapter } from '../../dist/app/adapters/scripted-adapter.js';
import { parseCommand } from '../../dist/app/protocol/codec.js';
import { actionPolicy } from '../../dist/app/automation/policy.js';
import { grants, short, size, watchSkills } from '../../dist/app/features/settings/skills.js';
const storage = () => {
    const map = new Map();
    return { getItem: key => map.get(key) ?? null, setItem: (key, value) => map.set(key, value), removeItem: key => map.delete(key) };
};
const named = (state, name) => state.skills.find(skill => skill.name === name);
test('the demo has sample skills in each state, each its own copy', () => {
    const a = createFixtures(), b = createFixtures();
    assert.deepEqual(a.skills.map(skill => [skill.name, skill.state]), [['release-check', 'changed'], ['tidy-notes', 'enabled'], ['weekly-review', 'waiting']]);
    for (const skill of a.skills) {
        assert.match(skill.digest, /^[0-9a-f]{64}$/u);
        assert.match(skill.signer, /^[0-9a-f]{64}$/u);
    }
    named(a, 'tidy-notes').state = 'waiting';
    assert.equal(named(b, 'tidy-notes').state, 'enabled');
});
test('the demo enables a skill only at the digest shown, disables it, and replays both', async () => {
    const port = storage(), adapter = new ScriptedAdapter({ animate: false, storage: port });
    const review = named(adapter.getSnapshot(), 'weekly-review');
    const stale = await adapter.execute({ type: 'skill.enable', name: 'weekly-review', digest: '0'.repeat(64) });
    assert.equal(stale.ok, false);
    assert.match(stale.message, /changed since you were shown it, so nothing was enabled/u);
    assert.equal(named(adapter.getSnapshot(), 'weekly-review').state, 'waiting');
    assert.equal((await adapter.execute({ type: 'skill.enable', name: 'weekly-review', digest: review.digest })).ok, true);
    assert.equal(named(adapter.getSnapshot(), 'weekly-review').state, 'enabled');
    assert.equal((await adapter.execute({ type: 'skill.disable', name: 'tidy-notes' })).ok, true);
    assert.equal(named(adapter.getSnapshot(), 'tidy-notes').state, 'waiting');
    for (const command of [{ type: 'skill.enable', name: 'nope', digest: review.digest }, { type: 'skill.disable', name: 'nope' }]) {
        const before = structuredClone(adapter.getSnapshot().skills);
        const result = await adapter.execute(command);
        assert.equal(result.ok, false);
        assert.match(result.message, /no longer here/u);
        assert.deepEqual(adapter.getSnapshot().skills, before, 'nothing is added');
    }
    const restored = new ScriptedAdapter({ animate: false, storage: port });
    assert.equal(named(restored.getSnapshot(), 'weekly-review').state, 'enabled');
    assert.equal(named(restored.getSnapshot(), 'tidy-notes').state, 'waiting');
    adapter.dispose();
    restored.dispose();
});
test('the codec reads the two commands exactly, and they are the owner\'s alone', () => {
    assert.deepEqual(parseCommand({ type: 'skill.enable', name: 'notes', digest: 'ab' }), { type: 'skill.enable', name: 'notes', digest: 'ab' });
    assert.deepEqual(parseCommand({ type: 'skill.disable', name: 'notes' }), { type: 'skill.disable', name: 'notes' });
    for (const bad of [{ type: 'skill.enable', name: 'notes' }, { type: 'skill.enable', digest: 'ab' }, { type: 'skill.disable' }, { type: 'skill.disable', name: 7 }])
        assert.equal(parseCommand(bad), null, JSON.stringify(bad));
    assert.equal(actionPolicy['skill.enable'], 'human-only');
    assert.equal(actionPolicy['skill.disable'], 'human-only');
});
test('a skill is shown in the owner\'s words: what enabling grants, its sizes, short hashes', () => {
    assert.deepEqual(grants({ capabilities: [], program: null }), ['Nothing more: it is a playbook, with no program to run']);
    assert.deepEqual(grants({ capabilities: [], program: 'main' }), ['Nothing more: its program declares no capabilities']);
    assert.deepEqual(grants({ capabilities: ['fs.read', 'net.egress', 'custom.thing'], program: 'main' }), ['Read the files a task gives it', 'Reach the network, through Hatchling’s allowlist', 'custom.thing'], 'an unknown capability by its own name');
    assert.deepEqual([0, 1023, 1024, 1536, 1024 * 1024, 412096].map(size), ['0 B', '1023 B', '1.0 KB', '1.5 KB', '1.0 MB', '402.4 KB']);
    assert.equal(short('ab'.repeat(32)), 'abababab…abababab');
    assert.equal(short('abcdef'), 'abcdef');
});
test('the Skills pane is drawn again when the skills change, and only then (#625)', () => {
    const listeners = new Set();
    const adapter = { subscribe: listener => { listeners.add(listener); return () => listeners.delete(listener); } };
    const publish = () => {
        for (const listener of [...listeners])
            listener();
    };
    let skills = [];
    let drawn = 0;
    const stop = watchSkills(adapter, () => skills, () => { drawn += 1; });
    publish();
    assert.equal(drawn, 0, 'a publish that changed nothing draws nothing');
    // The list arrives after the pane was drawn empty.
    skills = [{ name: 'write-a-report', state: 'waiting' }];
    publish();
    assert.equal(drawn, 1);
    // The same skills again, in a new array: not drawn again.
    skills = [{ name: 'write-a-report', state: 'waiting' }];
    publish();
    assert.equal(drawn, 1);
    skills = [{ name: 'write-a-report', state: 'enabled' }];
    publish();
    assert.equal(drawn, 2);
    stop();
    assert.equal(listeners.size, 0, 'stopping unsubscribes');
    skills = [];
    publish();
    assert.equal(drawn, 2);
});
test('a skill enabled is seen by the Skills pane through the adapter it subscribes to', async () => {
    const adapter = new ScriptedAdapter({ animate: false, storage: storage() });
    let drawn = 0;
    const stop = watchSkills(adapter, () => adapter.getSnapshot().skills, () => { drawn += 1; });
    const review = named(adapter.getSnapshot(), 'weekly-review');
    assert.equal((await adapter.execute({ type: 'skill.enable', name: 'weekly-review', digest: review.digest })).ok, true);
    assert.ok(drawn >= 1, 'enabling a skill publishes a change the pane draws');
    stop();
    adapter.dispose();
});
