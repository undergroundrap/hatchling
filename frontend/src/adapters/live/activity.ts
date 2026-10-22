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
import type { ActionIcon, ActivityLane, ActivityRow, ActivityThread, RunningActivityStep, Verdict } from '../../protocol/models.js';
import type { ActivityEvent, ApiRequest, ChildFailure, ChildVerdict, HarnessOutcome } from './protocol/generated.js';
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
/** What each kind of API request did, in the user's words. */
const REQUESTS: Readonly<Record<ApiRequest, string>> = {
    hello: 'Connected',
    subscribe: 'Followed the activity',
    send_message: 'Sent a message',
    decide: 'Answered a request',
    manage_sites: 'Managed the sites the agent is always allowed to read',
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
/** One thread, whichever kind it is. */
function projectAny(thread: Run | Calls | Harnessed): ActivityThread {
    switch (thread.kind) {
        case 'run':
            return project(thread);
        case 'calls':
            return projectCalls(thread);
        case 'harness':
            return projectHarness(thread);
    }
}
/** The activity threads, built from activity events in sequence order. */
export class ActivityBook {
    readonly #runs = new Map<string, Run>();
    readonly #calls = new Map<string, Calls>();
    readonly #harnessed = new Map<string, Harnessed>();
    /** Every thread, in the order each first appeared. */
    readonly #order: (Run | Calls | Harnessed)[] = [];
    /**
     * Apply one event. A subagent's first ending is its ending: a later,
     * contradictory one is ignored. Events for a subagent whose spawn was not
     * retained still land in its lane, without the facts the spawn carried.
     */
    apply(event: ActivityEvent): void {
        if (event.event === 'harness_run') {
            const harnessed = this.#harnessed.get(event.run_id) ?? this.#begin<Harnessed>(this.#harnessed, { kind: 'harness', id: event.run_id, startedAt: event.ts, lastAt: event.ts, harnesses: new Map<string, Harness>() });
            harnessed.lastAt = event.ts;
            const harness = harnessed.harnesses.get(event.harness) ?? { name: event.harness, runs: [] };
            harnessed.harnesses.set(event.harness, harness);
            harness.runs.push({ id: event.event_id, at: event.ts, outcome: event.outcome, files: event.files_changed });
            return;
        }
        if (event.event === 'api_call') {
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
    /** Records a new thread under its id, in arrival order. */
    #begin<Thread extends Run | Calls | Harnessed>(threads: Map<string, Thread>, thread: Thread): Thread {
        threads.set(thread.id, thread);
        this.#order.push(thread);
        return thread;
    }
    /** Fresh thread objects, newest first. */
    threads(): ActivityThread[] {
        return [...this.#order].reverse().map(projectAny);
    }
}
