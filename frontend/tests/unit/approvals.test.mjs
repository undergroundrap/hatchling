import test from 'node:test';
import assert from 'node:assert/strict';
import { ScriptedAdapter } from '../../dist/app/adapters/scripted-adapter.js';
import { ScriptedTaskApprovals } from '../../dist/app/adapters/task-approvals.js';
import { ApprovalReview } from '../../dist/app/features/approvals/review.js';
import { agentStatus } from '../../dist/app/features/context/agent-status.js';
function setup(scenario = 'network') {
    const adapter = new ScriptedAdapter({ animate: false });
    adapter.taskApprovals.preview(scenario, 'main');
    const request = adapter.taskApprovals.getSnapshot()[0];
    return { adapter, request, port: adapter.taskApprovals };
}
function attempt(request, decision = 'once', id = 'attempt-1') { return { id, requestId: request.id, revision: request.revision, decision }; }
test('task approvals are optional and do not seed an unrequested pending task', () => {
    const adapter = new ScriptedAdapter({ animate: false });
    assert.deepEqual(adapter.taskApprovals.getSnapshot(), []);
    const review = new ApprovalReview(undefined, () => { });
    assert.deepEqual(review.pending(), []);
    review.dispose();
    adapter.dispose();
});
test('approval request and activity evidence share exact request and chat ownership', () => {
    const { adapter, request } = setup();
    const snapshot = adapter.getSnapshot();
    const thread = snapshot.threads.find(t => t.id === request.threadId);
    assert.equal(thread.chatId, 'main');
    assert.equal(thread.pendingSteps[0].id, request.id);
    assert.equal(thread.verdict, null);
    assert.equal(agentStatus(snapshot, undefined, [request]).label, 'Waiting for approval');
    adapter.dispose();
});
test('allow once changes only that request and records no actual external work', async () => {
    const { adapter, port, request } = setup('multiple');
    assert.equal((await port.respond(attempt(request))).state, 'accepted');
    assert.equal(port.getSnapshot()[0].status, 'allowed-once');
    assert.equal(port.getSnapshot()[1].status, 'pending');
    assert.match(port.getSnapshot()[0].report, /no software was downloaded/);
    assert.equal(adapter.getSnapshot().threads.find(t => t.id === request.threadId).pendingSteps.length, 1);
    adapter.dispose();
});
test('task-scoped permission never creates an always-allow website grant', async () => {
    const { adapter, port, request } = setup();
    const before = adapter.getSnapshot().websitePermissions;
    await port.respond(attempt(request, 'task'));
    assert.equal(port.getSnapshot()[0].status, 'allowed-task');
    assert.deepEqual(adapter.getSnapshot().websitePermissions, before);
    adapter.dispose();
});
test('a scope that was not requested is rejected', async () => {
    const { adapter, port } = setup('multiple');
    const request = port.getSnapshot()[1];
    assert.equal((await port.respond(attempt(request, 'task'))).state, 'rejected');
    assert.equal(port.getSnapshot()[1].status, 'pending');
    adapter.dispose();
});
test('deny records the negative decision without marking tool execution successful', async () => {
    const { adapter, port, request } = setup();
    await port.respond(attempt(request, 'deny'));
    assert.equal(port.getSnapshot()[0].status, 'denied');
    const thread = adapter.getSnapshot().threads.find(t => t.id === request.threadId);
    assert.equal(thread.verdict, null);
    assert.match(thread.summary, /No requested action ran/);
    assert.match(thread.lanes[0].rows[0].title, /^Permission record:/);
    adapter.dispose();
});
test('expired and replaced requests cannot be approved by stale controls', async () => {
    const { adapter, port, request } = setup('expired');
    assert.equal((await port.respond(attempt(request))).state, 'rejected');
    port.preview('network', 'main');
    const current = port.getSnapshot()[0];
    port.preview('network', 'main');
    assert.equal((await port.respond(attempt(current, 'once', 'new'))).state, 'rejected');
    assert.equal(port.getSnapshot().find(r => r.id === current.id).status, 'cancelled');
    adapter.dispose();
});
test('stale revision and mismatched reuse of an attempt ID cannot grant permission', async () => {
    const { adapter, port, request } = setup();
    assert.equal((await port.respond({ ...attempt(request), revision: 0 })).state, 'rejected');
    assert.equal((await port.respond(attempt(request))).state, 'rejected');
    assert.equal(port.getSnapshot()[0].status, 'pending');
    adapter.dispose();
});
test('duplicate attempts return their original result, with one decision record', async () => {
    const { adapter, port, request } = setup();
    const command = attempt(request);
    const first = await port.respond(command);
    assert.deepEqual(await port.respond(command), first);
    assert.equal(adapter.getSnapshot().threads.find(t => t.id === request.threadId).lanes[0].rows.length, 1);
    adapter.dispose();
});
test('definite save failure remains retryable and never implies a grant', async () => {
    const { adapter, port, request } = setup('failed');
    const review = new ApprovalReview(port, () => { });
    await review.respond(request, 'once');
    assert.equal(review.state(request.id).phase, 'error');
    assert.equal(port.getSnapshot()[0].status, 'pending');
    await review.respond(request, 'once');
    assert.equal(port.getSnapshot()[0].status, 'allowed-once');
    assert.equal(review.state(request.id), undefined);
    review.dispose();
    adapter.dispose();
});
test('unknown permission response can only reconcile, not resend or change its decision', async () => {
    const { adapter, port, request } = setup('unknown');
    const review = new ApprovalReview(port, () => { });
    await review.respond(request, 'once');
    assert.equal(review.state(request.id).phase, 'unknown');
    await review.respond(request, 'deny');
    assert.equal(review.state(request.id).phase, 'unknown');
    assert.equal(port.getSnapshot()[0].status, 'pending');
    await review.check(request.id);
    assert.equal(port.getSnapshot()[0].status, 'allowed-once');
    assert.equal(review.state(request.id), undefined);
    review.dispose();
    adapter.dispose();
});
test('an answer that reaches a closed request says nothing was decided, until dismissed', async () => {
    const { adapter, port, request } = setup();
    let status = 'pending';
    const closing = {
        getSnapshot: () => port.getSnapshot().map(item => ({ ...item, status })),
        subscribe: () => () => { },
        // The broker's answer: the request had expired, and is closed.
        respond: () => { status = 'expired'; return Promise.resolve({ state: 'rejected', detail: 'This request expired before the decision arrived, so nothing was decided.' }); },
        refresh: () => Promise.resolve(),
    };
    let changes = 0;
    const review = new ApprovalReview(closing, () => { changes++; });
    await review.respond(request, 'deny');
    assert.equal(review.pending().length, 0, 'the request left the queue');
    assert.equal(review.state(request.id), undefined);
    assert.deepEqual(review.notices(), [{ id: request.id, chatId: request.chatId, title: request.description, detail: 'This request expired before the decision arrived, so nothing was decided.' }]);
    assert.deepEqual(review.notices(request.chatId).map(item => item.id), [request.id]);
    assert.deepEqual(review.notices('another-chat'), []);
    const before = changes;
    review.dismiss(request.id);
    assert.deepEqual(review.notices(), []);
    assert.equal(changes, before + 1, 'dismissing redraws');
    review.dismiss(request.id);
    assert.equal(changes, before + 1, 'nothing left to dismiss');
    review.dispose();
    adapter.dispose();
    // An answer that was taken, or whose fate is unknown, leaves no such notice.
    for (const [state, closes] of [['accepted', 'allowed-once'], ['unknown', 'expired']]) {
        const again = setup();
        let now = 'pending';
        const fake = { getSnapshot: () => again.port.getSnapshot().map(item => ({ ...item, status: now })), subscribe: () => () => { }, respond: () => { now = closes; return Promise.resolve({ state, detail: 'x' }); }, refresh: () => Promise.resolve() };
        const quiet = new ApprovalReview(fake, () => { });
        await quiet.respond(again.request, 'once');
        assert.deepEqual(quiet.notices(), []);
        quiet.dispose();
        again.adapter.dispose();
    }
});
test('the scripted answer that arrives too late closes its request and grants nothing', async () => {
    const { adapter, port, request } = setup('lapsed');
    const review = new ApprovalReview(port, () => { });
    await review.respond(request, 'once');
    assert.equal(port.getSnapshot()[0].status, 'expired');
    assert.match(port.getSnapshot()[0].report, /No permission was granted/);
    assert.equal(review.pending().length, 0);
    assert.match(review.notices()[0].detail, /expired before the decision arrived/);
    review.dispose();
    adapter.dispose();
});
test('double clicks dispatch one permission decision while waiting', async () => {
    const { adapter, port, request } = setup();
    let calls = 0;
    let resolve;
    const wait = new Promise(r => { resolve = r; });
    const fake = { getSnapshot: () => port.getSnapshot(), subscribe: () => () => { }, respond: () => { calls++; return wait; }, refresh: () => Promise.resolve() };
    const review = new ApprovalReview(fake, () => { });
    const first = review.respond(request, 'once');
    await review.respond(request, 'deny');
    await Promise.resolve();
    assert.equal(calls, 1);
    resolve({ state: 'unknown', detail: 'Lost receipt' });
    await first;
    assert.equal(review.state(request.id).phase, 'unknown');
    review.dispose();
    adapter.dispose();
});
test('thrown responses and hung approvals become unknown, never retryable rejection', async () => {
    for (const respond of [() => { throw new Error('Lost response'); }, () => new Promise(() => { })]) {
        const { adapter, port, request } = setup();
        const fake = { getSnapshot: () => port.getSnapshot(), subscribe: () => () => { }, respond, refresh: () => Promise.resolve() };
        const review = new ApprovalReview(fake, () => { }, 8);
        await review.respond(request, 'once');
        assert.equal(review.state(request.id).phase, 'unknown');
        review.dispose();
        adapter.dispose();
    }
});
test('acceptance without authoritative snapshot waits for confirmation', async () => {
    const { adapter, port, request } = setup();
    const fake = { getSnapshot: () => port.getSnapshot(), subscribe: () => () => { }, respond: () => Promise.resolve({ state: 'accepted', detail: '' }), refresh: () => Promise.resolve() };
    const review = new ApprovalReview(fake, () => { });
    await review.respond(request, 'once');
    assert.equal(review.state(request.id).phase, 'unknown');
    await review.check(request.id);
    assert.equal(review.state(request.id).phase, 'unknown');
    review.dispose();
    adapter.dispose();
});
test('request snapshots cannot be mutated by presentation', () => {
    const { adapter, port } = setup();
    const first = port.getSnapshot();
    first[0].status = 'allowed-once';
    first[0].scopes.push('always');
    assert.equal(port.getSnapshot()[0].status, 'pending');
    assert.deepEqual(port.getSnapshot()[0].scopes, ['once', 'task']);
    adapter.dispose();
});
test('chat deletion or archival invalidates stale approval controls', async () => {
    let available = true;
    const port = new ScriptedTaskApprovals(() => { }, () => available);
    port.preview('network', 'main');
    const request = port.getSnapshot()[0];
    available = false;
    assert.equal((await port.respond(attempt(request))).state, 'rejected');
    port.dispose();
});
test('reset clears requests, decisions and task activity with the original deterministic revision', async () => {
    const { adapter } = setup();
    await adapter.execute({ type: 'demo.reset' });
    assert.deepEqual(adapter.taskApprovals.getSnapshot(), []);
    assert.equal(adapter.getSnapshot().revision, 0);
    assert.equal(adapter.getSnapshot().threads.some(t => t.id.startsWith('approval-work-')), false);
    adapter.dispose();
});
test('adapter does not accept a conflicting new decision while a previous result is uncertain', async () => {
    const { adapter, port, request } = setup('unknown');
    await port.respond(attempt(request));
    assert.equal((await port.respond(attempt(request, 'deny', 'conflicting'))).state, 'unknown');
    await port.refresh(request.id);
    assert.equal(port.getSnapshot()[0].status, 'allowed-once');
    adapter.dispose();
});
