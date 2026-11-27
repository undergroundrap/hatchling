// The activity feed from the activity-monitor event contract (v0.1: the
// subagent lifecycle, and the local API's calls), mapped onto the existing
// activity views .
//
// One thread per parent run; the run is the main lane and each subagent gets
// its own lane. A subagent at work is a pending step, with no verdict; it
// becomes a row only when it ends, with the status the contract gives its
// ending. A kill is `interrupted`, never `failed`. Wording is the user's:
// no ids, hashes or raw arguments, and the child's own words (its progress
// notes, the parent's reason, a failure's detail) are quoted as untrusted text.
//
// The local API's calls are a thread of their own, one lane per
// caller and one row per call. A call records what kind of request it was and
// whether the API served it, never what it carried.
//
// Coding-harness runs are a thread of their own too, one lane per
// harness and one row per run: how it ended and how many files it changed.
// The pinned program and the brief are recorded by hash, never shown.
//
// Files the owner put in the agent's inbox are a thread of their own (WO-041),
// one row per file: the inbox's name for it, its size and how it ended. Never
// what it held, nor the owner's name for it.
//
// Each agent run's trail is a thread of its own: the tools the broker served
// it, each counted by how its uses ended, and its model calls. The broker
// recorded them as it answered, so they say what the run really did, whatever
// its answer says.
import type { ActionIcon, ActivityLane, ActivityRow, ActivityThread, RunningActivityStep, Verdict } from '../../protocol/models.js';
import type { ActivityEvent, ApiRequest, ChatMessage, ChildFailure, ChildVerdict, FileInEnded, FileOutEnded, HarnessOutcome, ToolTally, ToolUse } from './protocol/generated.js';
import { isLibraryName } from '../../session/inbox.js';
import { between, clockTime, count, quoted } from './text.js';
interface Ending {
    readonly status: Verdict;
    readonly icon: ActionIcon;
    readonly at: string;
    readonly title: string;
    readonly subtitle: string;
    readonly report: string;
}
/** What a spawn event says: when, and what the subagent was given. */
interface Spawn {
    readonly at: string;
    readonly budget: number;
    readonly grants: number;
}
interface Child {
    readonly id: string;
    readonly ordinal: number;
    spawn: Spawn | null;
    note: string | null;
    stepsSpent: number;
    ending: Ending | null;
}
interface Run {
    readonly kind: 'run';
    readonly id: string;
    readonly startedAt: string;
    lastAt: string;
    readonly children: Map<string, Child>;
}
/** One local API call. */
interface Call {
    readonly id: string;
    readonly at: string;
    readonly request: ApiRequest;
    readonly served: boolean;
    readonly grants: number;
}
/** The calls of one caller, in arrival order. */
interface Caller {
    readonly name: string;
    readonly calls: Call[];
}
/** The local API's calls, one thread per run id. */
interface Calls {
    readonly kind: 'calls';
    readonly id: string;
    readonly startedAt: string;
    lastAt: string;
    readonly callers: Map<string, Caller>;
}
/** One coding-harness run. */
interface HarnessRun {
    readonly id: string;
    readonly at: string;
    readonly outcome: HarnessOutcome;
    readonly files: number;
}
/** The runs of one harness, in arrival order. */
interface Harness {
    readonly name: string;
    readonly runs: HarnessRun[];
}
/** The coding-harness runs, one thread per run id. */
interface Harnessed {
    readonly kind: 'harness';
    readonly id: string;
    readonly startedAt: string;
    lastAt: string;
    readonly harnesses: Map<string, Harness>;
}
/** One file sent into the agent's inbox, once it ended. */
interface Crossing {
    readonly id: string;
    readonly at: string;
    readonly name: string | null;
    readonly bytes: number;
    readonly ended: FileInEnded;
}
/** One file the agent began giving the owner, once it ended. */
interface Given {
    readonly id: string;
    readonly at: string;
    readonly name: string | null;
    readonly bytes: number;
    readonly ended: FileOutEnded;
    /** A folder's count of files, once kept; none for a file. */
    readonly files: number | null;
}
/** The files the agent began giving the owner, one thread per run. */
interface Handed {
    readonly kind: 'handed';
    readonly id: string;
    readonly startedAt: string;
    lastAt: string;
    readonly given: Given[];
}
/** The files sent into the agent's inbox, one thread per run id. */
interface Crossings {
    readonly kind: 'files';
    readonly id: string;
    readonly startedAt: string;
    lastAt: string;
    readonly crossings: Crossing[];
}
/** One run's trail, as the broker served it. */
interface Trail {
    readonly kind: 'trail';
    readonly id: string;
    readonly startedAt: string;
    readonly lastAt: string;
    readonly chat: boolean;
    readonly tools: readonly ToolTally[];
    readonly modelCalls: number;
    readonly modelCallsRefused: number;
    readonly peakInputTokens: number;
    readonly outputTokens: number;
}
/** What each tool does, in the user's words. */
const TOOLS: Readonly<Record<ToolUse, string>> = {
    goals_read: 'Read your goals',
    goals_write: 'Changed your goals',
    memory_recall: 'Recalled your memory',
    memory_propose: 'Asked to remember something',
    memory_write: 'Wrote to memory',
    schedule_read: 'Listed scheduled work',
    schedule_write: 'Changed scheduled work',
    schedule_propose: 'Asked to schedule something',
    hook: 'Set up a hook',
    idea_suggest: 'Suggested an idea',
    web_search: 'Searched the web',
    web_page: 'Read a web page',
    browser: 'Used the browser',
    message_read: 'Read messages',
    message_send: 'Sent a message',
    subagent: 'Worked with a subagent',
    skill: 'Used a skill',
    vault: 'Used the vault',
    artifact: 'Worked on a build',
    space: 'Used a shared space',
    network: 'Made a network request',
    room: 'Used its computer',
};
/** What a run is doing as the broker serves it: each tool, said while it is in use, in the three words a status has. */
const DOING: Readonly<Record<ToolUse, string>> = {
    goals_read: 'Reading your goals',
    goals_write: 'Changing your goals',
    memory_recall: 'Recalling your memory',
    memory_propose: 'Asking to remember',
    memory_write: 'Writing to memory',
    schedule_read: 'Listing scheduled work',
    schedule_write: 'Changing scheduled work',
    schedule_propose: 'Asking to schedule',
    hook: 'Setting a hook',
    idea_suggest: 'Suggesting an idea',
    web_search: 'Searching the web',
    web_page: 'Reading a page',
    browser: 'Using the browser',
    message_read: 'Reading messages',
    message_send: 'Sending a message',
    subagent: 'Working with subagents',
    skill: 'Using a skill',
    vault: 'Using the vault',
    artifact: 'Making a build',
    space: 'Using a space',
    network: 'Making a request',
    room: 'Using its computer',
};
/** What a run is doing while it waits for its model's reply. */
const ASKING_MODEL = 'Asking the model';
/** What a run is doing before the broker has said: its first step hasn't started. */
const WORKING = 'Working on it';
const COMPACT = new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 1 });
/** How a tool's uses ended, in a few words: `2 done · 1 refused`. */
export function endings(tally: ToolTally): string {
    const parts = [[tally.done, 'done'], [tally.asked, 'asked you'], [tally.refused, 'refused'], [tally.failed, 'failed']] as const;
    return parts.filter(([value]) => value > 0).map(([value, what]) => `${value} ${what}`).join(' · ');
}
/** The line under an answer: the tools its run used, as Hatchling served them. */
export function trailLine(tools: readonly ToolTally[]): string {
    if (tools.length === 0)
        return 'Used no tools: this answer came from the model alone.';
    const said = tools.map(tally => {
        const what = TOOLS[tally.tool];
        return tally.refused + tally.failed === 0 ? what : `${what} (${endings(tally)})`;
    });
    return `Used: ${said.join(' · ')}`;
}
/** A tool's row: answered, refused or failed, as the broker served it. */
function toolRow(trail: Trail, laneId: string, tally: ToolTally): ActivityRow {
    const what = TOOLS[tally.tool];
    const served = tally.done + tally.asked;
    const status: Verdict = tally.refused + tally.failed === 0 ? 'success' : served === 0 ? 'failed' : 'partial';
    const said = endings(tally);
    return { id: `${trail.id}:${tally.tool}`, workId: `${trail.id}:${tally.tool}`, laneId, title: what, subtitle: said, icon: status === 'success' ? 'check' : 'warning', report: `${what}: ${said}. Hatchling counted each use as it answered it, never from what the agent said.`, startedAt: trail.startedAt, endedAt: trail.lastAt, durationMs: null, outputFailed: status === 'failed', status, phase: 'action' };
}
/** The model's row: its calls, the largest prompt, and any it was refused. */
function modelRow(trail: Trail, laneId: string): ActivityRow {
    const calls = count(trail.modelCalls, 'model call', 'model calls');
    const refused = trail.modelCallsRefused === 0 ? '' : ` · ${trail.modelCallsRefused} refused at your usage limit`;
    const subtitle = trail.modelCalls === 0 ? `No model call answered${refused}` : `${calls} · largest prompt ${COMPACT.format(trail.peakInputTokens)} tokens${refused}`;
    const room = trail.modelCalls === 0 ? '' : ` Its largest prompt was ${trail.peakInputTokens} tokens, which is how full the model's context got; it wrote ${trail.outputTokens} tokens in all.`;
    const limit = trail.modelCallsRefused === 0 ? '' : ` Hatchling refused ${count(trail.modelCallsRefused, 'call', 'calls')} past a usage limit you set.`;
    const status: Verdict = trail.modelCallsRefused === 0 ? 'success' : 'partial';
    return { id: `${trail.id}:model`, workId: `${trail.id}:model`, laneId, title: 'Called the model', subtitle, icon: status === 'success' ? 'check' : 'warning', report: `The run made ${calls}, as the model server reported them.${room}${limit}`, startedAt: trail.startedAt, endedAt: trail.lastAt, durationMs: null, outputFailed: false, status, phase: 'action' };
}
function projectTrail(trail: Trail): ActivityThread {
    const laneId = `${trail.id}:agent`;
    const used = trail.tools.length;
    const summary = used === 0 ? 'Used no tools: everything in its answer came from the model alone.' : `Used ${count(used, 'tool', 'tools')}, as Hatchling served them: ${trail.tools.map(tally => TOOLS[tally.tool].toLowerCase()).join(', ')}.`;
    return {
        id: trail.id,
        ...(trail.chat ? { chatId: 'main' } : {}),
        name: trail.chat ? 'What the chat run did' : 'What background work did',
        subtitle: `${count(used, 'tool', 'tools')} · ${count(trail.modelCalls, 'model call', 'model calls')}`,
        statusTitle: 'Trail recorded',
        state: 'completed',
        verdict: null,
        summary,
        finishMessage: summary,
        time: clockTime(trail.lastAt),
        startedAt: trail.startedAt,
        endedAt: trail.lastAt,
        evidenceArtifactId: null,
        lanes: [{ id: laneId, name: 'Main agent', parentId: null, rows: [...trail.tools.map(tally => toolRow(trail, laneId, tally)), modelRow(trail, laneId)] }],
        pendingSteps: [],
    };
}
/** One thing the owner asked in the chat, and the agent's answer once it came. */
interface Task {
    readonly kind: 'task';
    readonly id: string;
    readonly startedAt: string;
    lastAt: string;
    readonly ask: string;
    answer: string | null;
    /** The step its run last started, as the broker said: a tool, or its model. */
    step: ToolUse | 'model' | null;
}
/** The most words a task's name and its summary hold: the activity views' own bounds. */
const TASK_NAME_WORDS = 8;
const TASK_SUMMARY_WORDS = 10;
/** `text`'s first line, to `most` words, with an ellipsis when it was cut. */
function firstWords(text: string, most: number): string {
    const words = text.trim().split('\n', 1).join('').split(/\s+/).filter(Boolean);
    return words.length > most ? `${words.slice(0, most).join(' ')}…` : words.join(' ');
}
/** The app's own calls to the local API: the owner's clicks, recorded, but not the agent's work. */
function ownApp(caller: string): boolean {
    return caller === 'ui' || caller.startsWith('ui,');
}
function projectTask(task: Task): ActivityThread {
    const done = task.answer !== null;
    const said = task.answer === null ? '' : firstWords(task.answer, TASK_SUMMARY_WORDS);
    // A short answer is still said in a few words of Hatchling's own.
    const summary = task.answer === null ? 'The agent is working on it.' : said.split(' ').length < 4 ? `The agent answered: ${said}` : said;
    const laneId = `${task.id}:agent`;
    const rows: ActivityRow[] = task.answer === null ? [] : [{ id: `${task.id}:answer`, workId: task.id, laneId, title: 'Answered', subtitle: summary, icon: 'check', report: `The agent answered:\n\n${quoted(task.answer)}`, startedAt: task.startedAt, endedAt: task.lastAt, durationMs: between(task.startedAt, task.lastAt), outputFailed: false, status: 'success', phase: 'action' }];
    return {
        id: task.id,
        chatId: 'main',
        name: firstWords(task.ask, TASK_NAME_WORDS),
        subtitle: summary,
        statusTitle: done ? 'Answered' : task.step === null ? WORKING : task.step === 'model' ? ASKING_MODEL : DOING[task.step],
        state: done ? 'completed' : 'running',
        verdict: null,
        summary,
        finishMessage: summary,
        time: clockTime(task.lastAt),
        startedAt: task.startedAt,
        endedAt: done ? task.lastAt : null,
        evidenceArtifactId: null,
        lanes: [{ id: laneId, name: 'Main agent', parentId: null, rows }],
        pendingSteps: [],
    };
}
/** The harnesses Hatchling knows, by their skill names. */
const HARNESS_NAMES: Readonly<Partial<Record<string, string>>> = { 'claude-code': 'Claude Code', 'codex-cli': 'Codex CLI' };
/** How each harness run ended, in the user's words. */
const HARNESS_OUTCOMES: Readonly<Record<HarnessOutcome, {
    readonly status: Verdict;
    readonly icon: ActionIcon;
    readonly title: string;
    readonly said: string;
}>> = {
    done: { status: 'success', icon: 'check', title: 'Finished its task', said: 'finished its task' },
    needs_clarification: { status: 'partial', icon: 'warning', title: 'Asked a question', said: 'stopped to ask a question, which the next brief answers' },
    failed: { status: 'failed', icon: 'warning', title: 'Did not finish', said: 'did not finish' },
    timed_out: { status: 'failed', icon: 'clock', title: 'Ran out of time', said: 'ran out of time and was stopped' },
    refused: { status: 'failed', icon: 'warning', title: 'Refused before it started', said: 'was refused by Hatchling before it started' },
};
/** How each file sent into the agent's inbox ended, in the user's words. */
const GIVEN_ENDINGS: Readonly<Record<FileOutEnded, {
    readonly status: Verdict;
    readonly icon: ActionIcon;
    readonly title: string;
    readonly said: string;
}>> = {
    offered: { status: 'success', icon: 'file', title: 'Gave you a file', said: 'Hatchling keeps it for you in its Library.' },
    withheld: { status: 'failed', icon: 'warning', title: 'A file was withheld', said: 'It held a secret Hatchling keeps, and wasn’t given. Giving files was turned off for that conversation.' },
    unchecked: { status: 'interrupted', icon: 'warning', title: 'A file couldn’t be checked', said: 'It couldn’t be checked for the secrets Hatchling keeps, so it wasn’t given, and nothing was turned off.' },
    refused: { status: 'failed', icon: 'warning', title: 'A file wasn’t given', said: 'Hatchling didn’t give it, and told the agent why.' },
};
/** A folder the agent gave the owner, kept. */
const GIVEN_FOLDER = { status: 'success', icon: 'file', title: 'Gave you a folder', said: 'Hatchling keeps it for you in its Library.' } as const;
const FILE_ENDINGS: Readonly<Record<FileInEnded, {
    readonly status: Verdict;
    readonly icon: ActionIcon;
    readonly title: string;
    readonly said: string;
}>> = {
    placed: { status: 'success', icon: 'file', title: 'Put a file in the agent’s inbox', said: 'The agent can read it there, and can change none of it.' },
    refused: { status: 'failed', icon: 'warning', title: 'A file wasn’t put in the agent’s inbox', said: 'The inbox didn’t take it, and nothing was put. Hatchling said why when it was sent.' },
    unknown: { status: 'interrupted', icon: 'warning', title: 'Not known whether a file was put', said: 'Hatchling sent it, but didn’t hear back. The inbox’s list says whether it is there.' },
    abandoned: { status: 'interrupted', icon: 'warning', title: 'A file was let go before it was whole', said: 'It stopped part way: a part came out of order, its connection closed, or it waited too long. Nothing was put.' },
};
/** What each kind of API request did, in the user's words. */
const REQUESTS: Readonly<Record<ApiRequest, string>> = {
    hello: 'Connected',
    subscribe: 'Followed the activity',
    send_message: 'Sent a message',
    new_conversation: 'Started a new conversation',
    stop_run: 'Stopped the agent',
    keep_going: 'Asked the agent to keep going',
    decide: 'Answered a request',
    manage_grants: 'Managed the standing grants',
    manage_schedule: 'Changed a schedule',
    invoke_skill: 'Ran a skill',
    open_artifact: 'Opened a file',
    share_artifact: 'Shared a file',
    attach_artifact: 'Attached a file',
    save_file: 'Saved a file',
    take_control: 'Took over the browser',
    return_control: 'Handed back the browser',
    stop_browser: 'Stopped the browser',
    browser_input: 'Acted on the browser page',
    keep_signed_in: 'Kept a sign-in for the agent',
    manage_profiles: 'Managed the sites kept signed in',
    manage_sign_ins: 'Managed the sign-ins Hatchling fills',
    read_usage: 'Read the model usage',
    set_usage_limits: 'Set the usage limits',
    read_model: 'Read the model',
    read_setup: 'Read what’s set up',
    set_model: 'Chose the model',
    switch_model_server: 'Switched the model server',
    read_feed: 'Read the Feed page',
    manage_suggestions: 'Managed the Ideas page',
    manage_review: 'Managed the Review page',
    manage_goals: 'Managed your goals',
    manage_memory: 'Managed your memory',
    manage_skills: 'Managed your skills',
    list_decisions: 'Read the approvals history',
    list_covered: 'Read what your standing grants allowed',
    give_code: 'Gave a one-time code for the browser',
    decline_code: 'Declined to give a one-time code',
    begin_file_in: 'Began sending a file to the agent',
    file_in_part: 'Sent part of a file to the agent',
    end_file_in: 'Finished sending a file to the agent',
    list_inbox: 'Listed the agent’s inbox',
    remove_inbox_file: 'Removed a file from the agent’s inbox',
    list_library: 'Listed the Library',
    begin_file_out: 'Began downloading from the Library',
    file_out_part: 'Downloaded part of a download from the Library',
    remove_library_file: 'Removed an item from the Library',
    list_folder: 'Read a folder’s list in the Library',
};
const FAILURES: Readonly<Record<ChildFailure, string>> = {
    timeout: 'Ran out of time',
    cell_failed: 'Its sandbox stopped working',
    budget_exhausted: 'Used up its step budget',
};
const FINISHES: Readonly<Record<ChildVerdict, Pick<Ending, 'status' | 'icon' | 'title'> & {
    readonly what: string;
}>> = {
    ok: { status: 'success', icon: 'check', title: 'Finished its task', what: 'Completed its brief' },
    partial: { status: 'partial', icon: 'warning', title: 'Finished part of its task', what: 'Completed part of its brief' },
};
function steps(value: number): string {
    return count(value, 'step', 'steps');
}
/** The events that end a subagent. */
type Terminal = Extract<ActivityEvent, {
    readonly event: 'subagent_finished' | 'subagent_killed' | 'subagent_failed';
}>;
/** How one subagent ended, in the user's words. */
function ending(event: Terminal, ordinal: number): Ending {
    const name = `Subagent ${ordinal}`;
    switch (event.event) {
        case 'subagent_finished': {
            const finish = FINISHES[event.verdict];
            return { status: finish.status, icon: finish.icon, at: event.ts, title: finish.title, subtitle: `${finish.what} in ${steps(event.steps_spent)}`, report: `${name} returned: ${finish.what.toLowerCase()}, in ${steps(event.steps_spent)}. The broker keeps a hash of its result next to the hash of its brief.` };
        }
        case 'subagent_killed': {
            const kept = typeof event.partial_result_hash === 'string' ? 'What it had produced before it stopped was kept.' : 'It had produced nothing when it stopped.';
            return { status: 'interrupted', icon: 'run', at: event.ts, title: 'Stopped by the main agent', subtitle: `Stopped after ${steps(event.steps_spent)}`, report: `The main agent stopped ${name} after ${steps(event.steps_spent)}. Being stopped is not a failure of the subagent. ${kept}\n\nThe main agent's reason:\n\n${quoted(event.reason)}` };
        }
        case 'subagent_failed': {
            const what = FAILURES[event.failure];
            const detail = event.detail.trim() === '' ? '' : `\n\nWhat it reported:\n\n${quoted(event.detail)}`;
            return { status: 'failed', icon: 'warning', at: event.ts, title: what, subtitle: `Failed after ${steps(event.steps_spent)}`, report: `${name} failed on its own: ${what.toLowerCase()}. The main agent carries on, with this failure as the subagent's result.${detail}` };
        }
    }
}
function dispatchRow(run: Run, child: Child, spawn: Spawn): ActivityRow {
    const budget = `a budget of ${steps(spawn.budget)}`;
    const grants = count(spawn.grants, 'narrowed capability', 'narrowed capabilities');
    return { id: `${child.id}:dispatch`, workId: child.id, laneId: run.id, title: `Started subagent ${child.ordinal}`, subtitle: `Gave it ${budget} and ${grants}`, icon: 'run', report: `The main agent started subagent ${child.ordinal} with ${budget} and ${grants}. The steps are spent when they are given. The broker keeps a hash of the subagent's exact brief.`, startedAt: spawn.at, endedAt: spawn.at, durationMs: null, outputFailed: false, status: 'success', phase: 'dispatch' };
}
function finishRow(child: Child, end: Ending): ActivityRow {
    return { id: `${child.id}:finish`, workId: child.id, laneId: child.id, title: end.title, subtitle: end.subtitle, icon: end.icon, report: end.report, startedAt: child.spawn?.at ?? null, endedAt: end.at, durationMs: between(child.spawn?.at ?? null, end.at), outputFailed: end.status === 'failed', status: end.status, phase: 'finish' };
}
function workingStep(child: Child): RunningActivityStep {
    const spent = child.spawn === null ? `${steps(child.stepsSpent)} used` : `${child.stepsSpent} of ${steps(child.spawn.budget)} used`;
    const report = child.note === null ? 'No progress has been reported yet.' : `The latest progress it reported:\n\n${quoted(child.note)}`;
    return { id: `${child.id}:working`, workId: child.id, laneId: child.id, title: 'Working on its task', subtitle: spent, report, startedAt: child.spawn?.at ?? null };
}
function tally(children: readonly Child[]): string {
    const ended = (status: Verdict): number => children.filter(child => child.ending?.status === status).length;
    const parts = [[ended('success'), 'finished'], [ended('partial'), 'finished part of their task'], [ended('interrupted'), 'were stopped'], [ended('failed'), 'failed']] as const;
    const said = parts.filter(([value]) => value > 0).map(([value, what]) => `${value} ${what}`);
    return `Of ${count(children.length, 'subagent', 'subagents')}: ${said.join(', ')}. The run's own outcome is not part of the activity record yet.`;
}
function project(run: Run): ActivityThread {
    const children = [...run.children.values()];
    const working = children.filter(child => child.ending === null);
    const lanes: ActivityLane[] = [
        { id: run.id, name: 'Main agent', parentId: null, rows: children.flatMap(child => child.spawn === null ? [] : [dispatchRow(run, child, child.spawn)]) },
        ...children.map(child => ({ id: child.id, name: `Subagent ${child.ordinal}`, parentId: run.id, rows: child.ending === null ? [] : [finishRow(child, child.ending)] })),
    ];
    const running = working.length > 0;
    const summary = running ? `${count(working.length, 'subagent is', 'subagents are')} still working.` : tally(children);
    return {
        id: run.id,
        name: 'Delegated work',
        subtitle: `${count(children.length, 'subagent', 'subagents')} started · ${running ? `${working.length} still working` : 'all have ended'}`,
        statusTitle: running ? 'Subagents working' : 'Subagents ended',
        state: running ? 'running' : 'completed',
        verdict: null,
        summary,
        finishMessage: summary,
        time: clockTime(run.lastAt),
        startedAt: run.startedAt,
        endedAt: null,
        evidenceArtifactId: null,
        lanes,
        pendingSteps: working.map(workingStep),
    };
}
function callRow(laneId: string, caller: string, call: Call): ActivityRow {
    const what = REQUESTS[call.request];
    const held = count(call.grants, 'capability', 'capabilities');
    const outcome = call.served ? `The API served it, with the caller's ${held}.` : `The API refused it before any part of Hatchling acted on it. The caller held ${held}.`;
    return { id: call.id, workId: call.id, laneId, title: what, subtitle: call.served ? `Served, with ${held}` : `Refused, with ${held}`, icon: call.served ? 'check' : 'warning', report: `${caller}: ${what.toLowerCase()}. ${outcome}`, startedAt: call.at, endedAt: call.at, durationMs: null, outputFailed: !call.served, status: call.served ? 'success' : 'failed', phase: 'action' };
}
function projectCalls(calls: Calls): ActivityThread {
    const callers = [...calls.callers.values()];
    const all = callers.flatMap(caller => caller.calls);
    const refused = all.filter(call => !call.served).length;
    const lanes: ActivityLane[] = callers.map((caller, index) => {
        const laneId = `${calls.id}:${index}`;
        return { id: laneId, name: caller.name, parentId: null, rows: caller.calls.map(call => callRow(laneId, caller.name, call)) };
    });
    const summary = `${count(all.length, 'call', 'calls')} from ${count(callers.length, 'caller', 'callers')}: ${all.length - refused} served, ${refused} refused.`;
    return {
        id: calls.id,
        name: 'Local API calls',
        subtitle: `${count(all.length, 'call', 'calls')} · ${all.length - refused} served, ${refused} refused`,
        statusTitle: 'API calls recorded',
        state: 'completed',
        verdict: null,
        summary,
        finishMessage: summary,
        time: clockTime(calls.lastAt),
        startedAt: calls.startedAt,
        endedAt: null,
        evidenceArtifactId: null,
        lanes,
        pendingSteps: [],
    };
}
function harnessRow(laneId: string, label: string, run: HarnessRun): ActivityRow {
    const end = HARNESS_OUTCOMES[run.outcome];
    const files = count(run.files, 'file', 'files');
    return { id: run.id, workId: run.id, laneId, title: end.title, subtitle: `Changed ${files}`, icon: end.icon, report: `${label} ${end.said}, changing ${files}. Hatchling pinned the exact program that ran, and keeps a hash of the brief it was given. Any changes are a patch for review, not applied.`, startedAt: run.at, endedAt: run.at, durationMs: null, outputFailed: end.status === 'failed', status: end.status, phase: 'action' };
}
function projectHarness(harnessed: Harnessed): ActivityThread {
    const harnesses = [...harnessed.harnesses.values()];
    const runs = harnesses.flatMap(harness => harness.runs);
    const done = runs.filter(run => run.outcome === 'done').length;
    const lanes: ActivityLane[] = harnesses.map((harness, index) => {
        const laneId = `${harnessed.id}:${index}`;
        const label = HARNESS_NAMES[harness.name] ?? harness.name;
        return { id: laneId, name: label, parentId: null, rows: harness.runs.map(run => harnessRow(laneId, label, run)) };
    });
    const summary = `${count(runs.length, 'run', 'runs')} of ${count(harnesses.length, 'harness', 'harnesses')}: ${done} finished, ${runs.length - done} did not.`;
    return {
        id: harnessed.id,
        name: 'Coding harness runs',
        subtitle: `${count(runs.length, 'run', 'runs')} · ${done} finished, ${runs.length - done} did not`,
        statusTitle: 'Harness runs recorded',
        state: 'completed',
        verdict: null,
        summary,
        finishMessage: summary,
        time: clockTime(harnessed.lastAt),
        startedAt: harnessed.startedAt,
        endedAt: null,
        evidenceArtifactId: null,
        lanes,
        pendingSteps: [],
    };
}
/** A file's size, in the user's words. */
function size(bytes: number): string {
    if (bytes < 1024)
        return count(bytes, 'byte', 'bytes');
    if (bytes < 1048576)
        return `${Number((bytes / 1024).toFixed(1))} KB`;
    return `${Number((bytes / 1048576).toFixed(1))} MB`;
}
function crossingRow(laneId: string, crossing: Crossing): ActivityRow {
    const end = FILE_ENDINGS[crossing.ended];
    const named = crossing.name === null ? `A file of ${size(crossing.bytes)}` : `${crossing.name}, ${size(crossing.bytes)}`;
    const where = crossing.name === null ? '' : ` The agent reads it at /workspace/inbox/${crossing.name}.`;
    return { id: crossing.id, workId: crossing.id, laneId, title: end.title, subtitle: named, icon: end.icon, report: `${named}. ${end.said}${where}`, startedAt: crossing.at, endedAt: crossing.at, durationMs: null, outputFailed: end.status === 'failed', status: end.status, phase: 'action' };
}
function givenRow(laneId: string, given: Given): ActivityRow {
    // A folder kept says so, with its count of files.
    const files = given.ended === 'offered' ? given.files : null;
    const end = files === null ? GIVEN_ENDINGS[given.ended] : GIVEN_FOLDER;
    const held = files === null ? size(given.bytes) : `${count(files, 'file', 'files')}, ${size(given.bytes)}`;
    // A name is shown only when it is one the broker gives.
    const named = given.name === null || !isLibraryName(given.name) ? `${files === null ? 'A file' : 'A folder'} of ${held}` : `${given.name}, ${held}`;
    return { id: given.id, workId: given.id, laneId, title: end.title, subtitle: named, icon: end.icon, report: `${named}. ${end.said}`, startedAt: given.at, endedAt: given.at, durationMs: null, outputFailed: end.status === 'failed', status: end.status, phase: 'action' };
}
function projectHanded(handed: Handed): ActivityThread {
    const all = handed.given;
    const offered = all.filter(given => given.ended === 'offered').length;
    const laneId = `${handed.id}:given`;
    const summary = `${count(all.length, 'file', 'files')} from the agent: ${offered} given to you, ${all.length - offered} not.`;
    return {
        id: laneId,
        name: 'Files from the agent',
        subtitle: `${count(all.length, 'file', 'files')} · ${offered} given to you`,
        statusTitle: 'Files recorded',
        state: 'completed',
        verdict: null,
        summary,
        finishMessage: summary,
        time: clockTime(handed.lastAt),
        startedAt: handed.startedAt,
        endedAt: null,
        evidenceArtifactId: null,
        lanes: [{ id: laneId, name: 'To you, from the agent’s computer', parentId: null, rows: all.map(given => givenRow(laneId, given)) }],
        pendingSteps: [],
    };
}
function projectCrossings(crossings: Crossings): ActivityThread {
    const all = crossings.crossings;
    const placed = all.filter(crossing => crossing.ended === 'placed').length;
    const laneId = `${crossings.id}:files`;
    const summary = `${count(all.length, 'file', 'files')} sent to the agent: ${placed} put in its inbox, ${all.length - placed} not.`;
    return {
        id: `${crossings.id}:files`,
        name: 'Files for the agent',
        subtitle: `${count(all.length, 'file', 'files')} · ${placed} put in its inbox`,
        statusTitle: 'Files recorded',
        state: 'completed',
        verdict: null,
        summary,
        finishMessage: summary,
        time: clockTime(crossings.lastAt),
        startedAt: crossings.startedAt,
        endedAt: null,
        evidenceArtifactId: null,
        lanes: [{ id: laneId, name: 'Into the agent’s inbox', parentId: null, rows: all.map(crossing => crossingRow(laneId, crossing)) }],
        pendingSteps: [],
    };
}
/** One thread, whichever kind it is. */
function projectAny(thread: Run | Calls | Harnessed | Crossings | Handed | Task | Trail): ActivityThread {
    switch (thread.kind) {
        case 'task':
            return projectTask(thread);
        case 'trail':
            return projectTrail(thread);
        case 'run':
            return project(thread);
        case 'calls':
            return projectCalls(thread);
        case 'harness':
            return projectHarness(thread);
        case 'files':
            return projectCrossings(thread);
        case 'handed':
            return projectHanded(thread);
    }
}
/** The activity threads, built from activity events in sequence order. */
export class ActivityBook {
    readonly #runs = new Map<string, Run>();
    readonly #calls = new Map<string, Calls>();
    readonly #crossings = new Map<string, Crossings>();
    readonly #handed = new Map<string, Handed>();
    readonly #harnessed = new Map<string, Harnessed>();
    readonly #tasks = new Map<string, Task>();
    readonly #trails = new Map<string, Trail>();
    /** The tasks no answer has come for, oldest first: the next answer is the first one's. */
    readonly #waiting: Task[] = [];
    /** The tasks another's run took into it: its answer is theirs, and they are no threads of their own. */
    readonly #joined = new Set<Task>();
    /** Every thread, in the order each first appeared. */
    readonly #order: (Run | Calls | Harnessed | Crossings | Handed | Task | Trail)[] = [];
    /**
     * Apply one event. A subagent's first ending is its ending: a later,
     * contradictory one is ignored. Events for a subagent whose spawn was not
     * retained still land in its lane, without the facts the spawn carried.
     */
    apply(event: ActivityEvent): void {
        if (event.event === 'run_summary') {
            // A run's summary is written once, when its session ends: one seen
            // again in a replay changes nothing.
            if (!this.#trails.has(event.event_id))
                this.#begin<Trail>(this.#trails, { kind: 'trail', id: event.event_id, startedAt: event.ts, lastAt: event.ts, chat: event.chat, tools: event.tools, modelCalls: event.model_calls, modelCallsRefused: event.model_calls_refused, peakInputTokens: event.peak_input_tokens, outputTokens: event.output_tokens });
            return;
        }
        if (event.event === 'harness_run') {
            const harnessed = this.#harnessed.get(event.run_id) ?? this.#begin<Harnessed>(this.#harnessed, { kind: 'harness', id: event.run_id, startedAt: event.ts, lastAt: event.ts, harnesses: new Map<string, Harness>() });
            harnessed.lastAt = event.ts;
            const harness = harnessed.harnesses.get(event.harness) ?? { name: event.harness, runs: [] };
            harnessed.harnesses.set(event.harness, harness);
            harness.runs.push({ id: event.event_id, at: event.ts, outcome: event.outcome, files: event.files_changed });
            return;
        }
        if (event.event === 'file_in') {
            const crossings = this.#crossings.get(event.run_id) ?? this.#begin<Crossings>(this.#crossings, { kind: 'files', id: event.run_id, startedAt: event.ts, lastAt: event.ts, crossings: [] });
            if (crossings.crossings.some(crossing => crossing.id === event.event_id))
                return;
            crossings.lastAt = event.ts;
            crossings.crossings.push({ id: event.event_id, at: event.ts, name: event.name ?? null, bytes: event.bytes, ended: event.ended });
            return;
        }
        if (event.event === 'file_out') {
            const handed = this.#handed.get(event.run_id) ?? this.#begin<Handed>(this.#handed, { kind: 'handed', id: event.run_id, startedAt: event.ts, lastAt: event.ts, given: [] });
            if (handed.given.some(given => given.id === event.event_id))
                return;
            handed.lastAt = event.ts;
            handed.given.push({ id: event.event_id, at: event.ts, name: event.name ?? null, bytes: event.bytes, ended: event.ended, files: event.files ?? null });
            return;
        }
        if (event.event === 'api_call') {
            if (ownApp(event.caller))
                return;
            const calls = this.#calls.get(event.run_id) ?? this.#begin<Calls>(this.#calls, { kind: 'calls', id: event.run_id, startedAt: event.ts, lastAt: event.ts, callers: new Map<string, Caller>() });
            calls.lastAt = event.ts;
            const caller = calls.callers.get(event.caller) ?? { name: event.caller, calls: [] };
            calls.callers.set(event.caller, caller);
            caller.calls.push({ id: event.event_id, at: event.ts, request: event.request, served: event.outcome === 'served', grants: event.capabilities.length });
            return;
        }
        const run = this.#runs.get(event.run_id) ?? this.#begin<Run>(this.#runs, { kind: 'run', id: event.run_id, startedAt: event.ts, lastAt: event.ts, children: new Map<string, Child>() });
        run.lastAt = event.ts;
        const child = run.children.get(event.child_id) ?? { id: event.child_id, ordinal: run.children.size + 1, spawn: null, note: null, stepsSpent: 0, ending: null };
        run.children.set(child.id, child);
        if (event.event === 'subagent_spawned') {
            child.spawn = { at: event.ts, budget: event.budget_steps, grants: event.capabilities.length };
            return;
        }
        if (child.ending !== null)
            return;
        child.stepsSpent = event.steps_spent;
        if (event.event === 'subagent_progress') {
            child.note = event.note;
            return;
        }
        child.ending = ending(event, child.ordinal);
    }
    /**
     * Apply one chat message on the main chat: the owner's starts a task, and
     * the agent's next reply answers the oldest one waiting, as the broker
     * answers them in turn. A reply with no task waiting (a scheduled run's,
     * a notice) isn't a task's answer. A message seen again in a replay
     * changes nothing.
     */
    chat(message: ChatMessage, ts: string): void {
        if (message.chat_id !== 'local' || this.#tasks.has(message.message_id))
            return;
        if (message.author === 'user') {
            this.#waiting.push(this.#begin<Task>(this.#tasks, { kind: 'task', id: message.message_id, startedAt: ts, lastAt: ts, ask: message.text, answer: null, step: null }));
            return;
        }
        const asked = this.#waiting.shift();
        if (asked === undefined)
            return;
        asked.answer = message.text;
        asked.lastAt = ts;
    }
    /**
     * The run answering the oldest waiting task took the owner's next `count`
     * messages into it: its answer is for them too, so they are no tasks of
     * their own. With none waiting behind it, nothing changes.
     */
    joined(count: number, ts: string): void {
        const [asked] = this.#waiting;
        const taken = this.#waiting.splice(1, count);
        if (asked === undefined || taken.length === 0)
            return;
        for (const task of taken)
            this.#joined.add(task);
        asked.lastAt = ts;
    }
    /**
     * The step the run answering the waiting task just started: a tool, or
     * none while it waits for its model. With no task waiting it is
     * nobody's, and changes nothing.
     */
    step(tool: ToolUse | null, ts: string): void {
        const [asked] = this.#waiting;
        if (asked === undefined)
            return;
        asked.step = tool ?? 'model';
        asked.lastAt = ts;
    }
    /** Records a new thread under its id, in arrival order. */
    #begin<Thread extends Run | Calls | Harnessed | Crossings | Handed | Task | Trail>(threads: Map<string, Thread>, thread: Thread): Thread {
        threads.set(thread.id, thread);
        this.#order.push(thread);
        return thread;
    }
    /** Fresh thread objects, newest first. */
    threads(): ActivityThread[] {
        return this.#order.filter((thread) => thread.kind !== 'task' || !this.#joined.has(thread)).reverse().map(projectAny);
    }
}
