// Starting an idea from the Ideas page: the chat its text is put in, and the
// conversation of its own it is given there.
import test from 'node:test';
import assert from 'node:assert/strict';
import { freshConversation, ideaChat, ideaText, startIdea } from '../../dist/app/features/ideas/start.js';
import { ideaOf } from '../../dist/app/adapters/live/ideas.js';
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
    const port = answer => ({ start: (chatId, idea) => { asked.push([chatId, idea]); return Promise.resolve(answer); } });
    assert.equal(await freshConversation(port({ ok: true, message: 'New conversation started.' }), 'main', 'idea-1'), null);
    assert.equal(await freshConversation(port({ ok: false, message: 'Hatchling cannot start a new conversation right now, so the agent still has the last one.' }), 'side', 'idea-2'), 'Hatchling cannot start a new conversation right now, so the agent still has the last one. Your idea is in the chat box of the conversation that is open.');
    assert.deepEqual(asked, [['main', 'idea-1'], ['side', 'idea-2']], 'asked once each, of the chat named, for the idea named');
    // No way to start one here, or no chat to start it in: nothing is asked, and nothing is said.
    assert.equal(await freshConversation(undefined, 'main', 'idea-3'), null);
    assert.equal(await freshConversation(port({ ok: false, message: 'never asked' }), null, 'idea-3'), null);
    assert.equal(asked.length, 2);
});
test('an idea puts its title and what it would do in the chat box; never where a kept chat\'s run said it stopped', () => {
    const card = { suggestion_id: 'idea-k', title: 'Pick up where a chat stopped', summary: 'Make the tools.', why: 'You kept it.', area: 'productivity', suggested_at: '2026-11-16T08:30:00Z', state: 'suggested', source: { kind: 'chat' }, run: 'run-1', run_said: 'All three tools are done.' };
    const idea = ideaOf(card);
    assert.match(idea.signal, /All three tools are done/, 'the card shows it');
    assert.equal(ideaText(idea), 'Pick up where a chat stopped: Make the tools.', 'the chat box never gets it');
});
test('starting an idea starts its conversation first, then puts its text in the chat box; nothing is sent', async () => {
    const app = (answer, chats = [chat('main'), chat('signal-1', 'signal')], chatId = 'signal-1') => {
        const did = [];
        return {
            did,
            conversations: answer === undefined ? undefined : { start: (id, idea) => { did.push(['start', id, idea]); return Promise.resolve(answer); } },
            state: { chats },
            ui: { chatId },
            toast: (message, error) => { did.push(['toast', message, error]); },
            prefillComposer: (text) => { did.push(['prefill', text]); },
        };
    };
    const started = app({ ok: true, message: 'New conversation started.' });
    await startIdea(started, 'idea-7', 'A run plan: A week of short runs.');
    assert.deepEqual(started.did, [['start', 'main', 'idea-7'], ['prefill', 'A run plan: A week of short runs.']], 'the conversation, for this idea, then the text; no word when it worked');
    // It couldn't be started: the owner is told, and the text is put there all the same.
    const refused = app({ ok: false, message: 'Hatchling is not connected to its broker, so the agent still has the last conversation.' });
    await startIdea(refused, 'idea-8', 'An idea');
    assert.deepEqual(refused.did, [['start', 'main', 'idea-8'], ['toast', 'Hatchling is not connected to its broker, so the agent still has the last conversation. Your idea is in the chat box of the conversation that is open.', true], ['prefill', 'An idea']]);
    // A preview has no way to start one, and an archived chat is left alone: the text alone.
    const preview = app(undefined);
    await startIdea(preview, 'idea-9', 'An idea');
    assert.deepEqual(preview.did, [['prefill', 'An idea']]);
    const archived = app({ ok: true, message: '' }, [chat('main', null, true)], 'main');
    await startIdea(archived, 'idea-9', 'An idea');
    assert.deepEqual(archived.did, [['prefill', 'An idea']]);
});
