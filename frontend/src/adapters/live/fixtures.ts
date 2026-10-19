// Typed fixtures for the live adapter's tests: a synthetic session,
// frame by frame, typed against the generated wire types, so a fixture that
// drifts from the protocol fails the typecheck. No real people, runs or data.
import { SESSION_PROTOCOL_VERSION, UI_SCHEMA_VERSION } from './protocol/generated.js';
import type { ActivityEvent, ActivityState, UiEventBody, UiResponse } from './protocol/generated.js';
/** The sample run that owns the activity thread. */
export const SAMPLE_RUN = 'run-sample-1';
/**
 * Untrusted text with every way it could try to become structure: markup,
 * script, an event handler, links dressed as buttons, an artifact link and a
 * script URL, beside the plain formatting the renderer is allowed to keep.
 * The closing script tag is assembled at run time: the standalone page inlines
 * every module in one script element, which a literal one would end early.
 */
export const HOSTILE_TEXT = [
    `<script>alert("x")<${'/'}script><img src=x onerror="alert(1)"><iframe src="https://example.invalid"></iframe>`,
    '[Approve](https://example.invalid/approve) [Open the file](artifact:sample-file) [Run](javascript:alert(1)) [[Allow](x)](https://example.invalid/nested)',
    '# Approval required',
    '| Allow | Deny |',
    '| --- | --- |',
    '```',
    '<button>Allow once</button>',
    '```',
    '**bold** and `code` stay as formatting.',
].join('\n');
/** An RFC 3339 time, `second` seconds into the sample session. */
export function sampleTime(second: number): string {
    return new Date(Date.UTC(2026, 9, 7, 9, 0, second)).toISOString();
}
/** One event frame. */
export function sampleEvent(seq: number, second: number, body: UiEventBody): UiResponse {
    return { type: 'event', seq, ts: sampleTime(second), body };
}
/** An activity event without the fields every event shares. */
export type SampleActivity = ActivityEvent extends infer Event ? Event extends unknown ? Omit<Event, 'event_id' | 'run_id' | 'ts'> : never : never;
/** One activity event body for the sample run. */
export function sampleActivity(second: number, state: ActivityState, event: SampleActivity): UiEventBody {
    return { kind: 'activity', state, event: { ...event, event_id: `event-${second}`, run_id: SAMPLE_RUN, ts: sampleTime(second) } };
}
const HASH = 'a'.repeat(64);
/** The broker's welcome at this build's versions. */
export const SAMPLE_WELCOME: UiResponse = { type: 'welcome', schema_version: UI_SCHEMA_VERSION, session_protocol: SESSION_PROTOCOL_VERSION };
/**
 * A whole session after the welcome: a short chat, four subagents that end
 * four ways (finished, stopped, failed on their own, finished in part), a
 * hostile agent reply, a confirmation and its newer revision, and two events
 * no view shows yet.
 */
export const SAMPLE_EVENTS: readonly UiResponse[] = [
    sampleEvent(1, 1, { kind: 'chat_message', chat_id: 'main', message_id: 'message-1', author: 'user', text: 'Please check the sample release notes.' }),
    sampleEvent(2, 2, { kind: 'chat_message', chat_id: 'main', message_id: 'message-2', author: 'agent', text: 'On it. I will ask four helpers.' }),
    sampleEvent(3, 3, sampleActivity(3, 'pending', { event: 'subagent_spawned', child_id: 'child-a', parent_run_id: SAMPLE_RUN, task_brief_hash: HASH, budget_steps: 40, capabilities: ['files.read', 'web.fetch'], depth: 1 })),
    sampleEvent(4, 4, sampleActivity(4, 'pending', { event: 'subagent_spawned', child_id: 'child-b', parent_run_id: SAMPLE_RUN, task_brief_hash: HASH, budget_steps: 1, capabilities: ['files.read'], depth: 1 })),
    sampleEvent(5, 5, sampleActivity(5, 'pending', { event: 'subagent_spawned', child_id: 'child-c', parent_run_id: SAMPLE_RUN, task_brief_hash: HASH, budget_steps: 10, capabilities: [], depth: 1 })),
    sampleEvent(6, 6, sampleActivity(6, 'pending', { event: 'subagent_spawned', child_id: 'child-d', parent_run_id: SAMPLE_RUN, task_brief_hash: HASH, budget_steps: 30, capabilities: ['files.read'], depth: 1 })),
    sampleEvent(7, 7, sampleActivity(7, 'pending', { event: 'subagent_progress', child_id: 'child-a', note: 'Read 3 of 5 sample files.', steps_spent: 12 })),
    sampleEvent(8, 9, sampleActivity(9, 'success', { event: 'subagent_finished', child_id: 'child-a', result_hash: HASH, steps_spent: 31, verdict: 'ok' })),
    sampleEvent(9, 10, sampleActivity(10, 'interrupted', { event: 'subagent_killed', child_id: 'child-b', reason: HOSTILE_TEXT, partial_result_hash: HASH, steps_spent: 1 })),
    sampleEvent(10, 11, sampleActivity(11, 'failed', { event: 'subagent_failed', child_id: 'child-c', failure: 'budget_exhausted', detail: 'Stopped at the step limit.', steps_spent: 10 })),
    sampleEvent(11, 12, sampleActivity(12, 'partial', { event: 'subagent_finished', child_id: 'child-d', result_hash: HASH, steps_spent: 30, verdict: 'partial' })),
    sampleEvent(12, 13, { kind: 'chat_message', chat_id: 'main', message_id: 'message-3', author: 'agent', text: HOSTILE_TEXT }),
    sampleEvent(13, 14, { kind: 'confirmation', confirmation_id: 'confirm-1', revision: 1, chat_id: 'main', summary: 'Post the sample summary to the sample team space?', expires_at: sampleTime(600), digest: HASH, fields: [{ label: 'Operation', value: 'Post a summary', source: 'broker' }, { label: 'Text', value: HOSTILE_TEXT, source: 'agent' }] }),
    sampleEvent(14, 15, { kind: 'confirmation', confirmation_id: 'confirm-1', revision: 2, chat_id: 'main', summary: 'Post the shorter sample summary to the sample team space?', expires_at: sampleTime(600), digest: HASH, fields: [{ label: 'Operation', value: 'Post a summary', source: 'broker' }, { label: 'Text', value: 'The shorter sample summary.', source: 'agent' }] }),
    sampleEvent(15, 16, { kind: 'artifact', state: 'building', build_id: 'build-1', build_kind: 'document' }),
    sampleEvent(16, 17, { kind: 'schedule', outcome: 'fired', schedule_id: 'schedule-1' }),
];
