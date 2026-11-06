import assert from 'node:assert/strict';
import test from 'node:test';
import { KEEP_COMMANDS, KEY_COMMANDS, MODEL_COMMANDS, RESET_COMMANDS } from '../../dist/app/session/data-commands.js';
test('every data command is root\'s, and each reset level is offered', () => {
    const all = [...KEEP_COMMANDS, ...RESET_COMMANDS, ...MODEL_COMMANDS, ...KEY_COMMANDS];
    assert.ok(all.every((item) => item.command.startsWith('sudo hatchling-broker ')));
    assert.deepEqual(RESET_COMMANDS.map((item) => item.command.split(' ').at(-1)), ['conversation', 'agent', 'everything']);
    assert.ok(all.every((item) => item.title.length > 0 && item.note.length > 0));
});
test('what each reset keeps is said', () => {
    const [, agent, everything] = RESET_COMMANDS;
    assert.match(agent.note, /security keys stay/u);
    assert.match(everything.note, /security keys aren.t changed/u);
});
