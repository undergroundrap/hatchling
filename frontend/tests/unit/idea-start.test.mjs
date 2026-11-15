// Starting an idea from the Ideas page: the chat its text is put in, and the
// conversation of its own it is given there.
import test from 'node:test';
import assert from 'node:assert/strict';
import { freshConversation, ideaChat, startIdea } from '../../dist/app/features/ideas/start.js';
const chat = (id, channelId = null, archived = false) => ({ id, channelId, archived });
test('an idea goes in the open chat, or the owner\'s own when that is a connected app\'s; never an archived one', () => {
    const chats = [chat('main'), chat('side'), chat('signal-1', 'signal'), chat('old', null, true)];
    assert.equal(ideaChat(chats, 'main'), 'main');
    assert.equal(ideaChat(chats, 'side'), 'side', 'the open chat, when it is the owner\'s');
    assert.equal(ideaChat(chats, 'signal-1'), 'main', 'never a connected app\'s chat');
    assert.equal(ideaChat(chats, 'gone'), 'main', 'a chat that isn\'t there');
    assert.equal(ideaChat(chats, 'old'), null, 'an archived chat is left alone');
    assert.equal(ideaChat([chat('side'), chat('signal-1', 'signal')], 'signal-1'), null, 'no chat of the owner\'s own');
    assert.equal(ideaChat([chat('main', null, true), chat('signal-1', 'signal')], 'signal-1'), null, 'the owner\'s own is archived');
    assert.equal(ideaChat([chat('main', 'signal')], 'main'), null, 'a connected app\'s chat by that name');
    assert.equal(ideaChat([], 'main'), null);
    // A chat that doesn't say whether it is archived isn't.
    assert.equal(ideaChat([{ id: 'main', channelId: null }], 'main'), 'main');
});
test('a started idea is given a conversation of its own, and the owner is told when it couldn\'t be', async () => {
    const asked = [];
    const port = answer => ({ start: (chatId) => { asked.push(chatId); return Promise.resolve(answer); } });
    assert.equal(await freshConversation(port({ ok: true, message: 'New conversation started.' }), 'main'), null);
    assert.equal(await freshConversation(port({ ok: false, message: 'Hatchling cannot start a new conversation right now, so the agent still has the last one.' }), 'side'), 'Hatchling cannot start a new conversation right now, so the agent still has the last one. Your idea is in the chat box of the conversation that is open.');
    assert.deepEqual(asked, ['main', 'side'], 'asked once each, of the chat named');
    // No way to start one here, or no chat to start it in: nothing is asked, and nothing is said.
    assert.equal(await freshConversation(undefined, 'main'), null);
    assert.equal(await freshConversation(port({ ok: false, message: 'never asked' }), null), null);
    assert.deepEqual(asked, ['main', 'side']);
});
test('starting an idea starts its conversation first, then puts its text in the chat box; nothing is sent', async () => {
    const app = (answer, chats = [chat('main'), chat('signal-1', 'signal')], chatId = 'signal-1') => {
        const did = [];
        return {
            did,
            conversations: answer === undefined ? undefined : { start: (id) => { did.push(['start', id]); return Promise.resolve(answer); } },
            state: { chats },
            ui: { chatId },
            toast: (message, error) => { did.push(['toast', message, error]); },
            prefillComposer: (text) => { did.push(['prefill', text]); },
        };
    };
    const started = app({ ok: true, message: 'New conversation started.' });
    await startIdea(started, 'A run plan: A week of short runs.');
    assert.deepEqual(started.did, [['start', 'main'], ['prefill', 'A run plan: A week of short runs.']], 'the conversation, then the text; no word when it worked');
    // It couldn't be started: the owner is told, and the text is put there all the same.
    const refused = app({ ok: false, message: 'Hatchling is not connected to its broker, so the agent still has the last conversation.' });
    await startIdea(refused, 'An idea');
    assert.deepEqual(refused.did, [['start', 'main'], ['toast', 'Hatchling is not connected to its broker, so the agent still has the last conversation. Your idea is in the chat box of the conversation that is open.', true], ['prefill', 'An idea']]);
    // A preview has no way to start one, and an archived chat is left alone: the text alone.
    const preview = app(undefined);
    await startIdea(preview, 'An idea');
    assert.deepEqual(preview.did, [['prefill', 'An idea']]);
    const archived = app({ ok: true, message: '' }, [chat('main', null, true)], 'main');
    await startIdea(archived, 'An idea');
    assert.deepEqual(archived.did, [['prefill', 'An idea']]);
});
