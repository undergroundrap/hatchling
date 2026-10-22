// The interactive surfaces for the live adapter: the agent's status,
// artifacts and files, and the live browser.
//
// Every command is checked again when it is sent, against what the broker last
// said, never against the screen it was pressed on: a version that is no longer
// listed is not shared, and a browser that is changing hands takes no command
// but Stop. The user's own input goes only to a browser they drive. The broker
// checks once more and has the last word. Nothing changes
// here until the broker answers; a lost answer is unknown, never a success.
import type { ActivityThread, Artifact, ArtifactFormat, BrowserTask, CommandResult, Message, TurnState } from '../../protocol/models.js';
import { ArtifactShelf, fileFormat, imageFromUrl } from './artifacts.js';
import { BrowserDesk } from './browser.js';
import type { LiveClient, Reply } from './client.js';
import { switching } from './control.js';
import type { ActionRefusal, AgentState, ArtifactRefusal, BrowserFailure, ControlRefusal, DeliveryRefusal, FileContent, OwnerInput, UiAction, UiActionResult, UiEventBody } from './protocol/generated.js';
/** The avatar and the status line show exactly what the broker reported. */
const TURNS: Readonly<Record<AgentState, TurnState>> = { idle: 'idle', working: 'working', awaiting_approval: 'awaiting', paused: 'interrupted', stalled: 'stalled' };
const ACTION_REFUSALS: Readonly<Record<ActionRefusal, string>> = {
    not_connected: 'Hatchling is not connected to its broker, so nothing happened.',
    bad_request: 'Hatchling could not read the request, so nothing happened.',
    unavailable: 'Hatchling cannot do this right now, so nothing happened.',
    not_granted: 'This connection is not allowed to do this, so nothing happened.',
};
const ARTIFACT_REFUSALS: Readonly<Record<ArtifactRefusal, string>> = {
    unknown_artifact: 'Hatchling no longer has this file.',
    unknown_revision: 'Hatchling no longer has this version of the file.',
};
const SHARE_REFUSALS: Readonly<Record<DeliveryRefusal, string>> = {
    'unknown-build': 'Hatchling no longer has this version, so nothing was shared.',
    incomplete: 'This version is not finished, so nothing was shared.',
    'hash-mismatch': 'This version changed after it was made, so nothing was shared.',
    blocked: 'The share review stopped this file, so nothing was shared.',
    'gate-error': 'The share review could not check this file, so nothing was shared.',
    'bad-target': 'Hatchling cannot share to that place, so nothing was shared.',
    'delivery-failed': 'The share review passed, but sending the file failed.',
};
const CONTROL_REFUSALS: Readonly<Record<ControlRefusal, string>> = {
    unknown_session: 'This browser session is no longer available.',
    finished: 'This browser session has ended.',
    switching: 'Control is changing hands. Wait until it settles.',
    not_allowed: 'Control cannot move that way right now.',
    stale_frame: 'The page changed since you last saw it. Look at it again before handing back.',
};
const INPUT_FAILURES: Readonly<Record<BrowserFailure, string>> = {
    no_page: 'No page is open yet.',
    unknown_element: 'That part of the page is gone. Look at the page again.',
    not_possible: 'The page could not take that. A click must be on the page, and typed text at most 4096 characters.',
    timeout: 'The page did not finish in time.',
    load_failed: 'The page could not be loaded.',
    browser_gone: 'The browser stopped working, so it was closed.',
    bad_command: 'The browser did not understand that, so nothing happened.',
};
const UNEXPECTED = 'Hatchling answered with something else, so this is not confirmed.';
function megabytes(bytes: number): string {
    return `${Number((bytes / 1048576).toFixed(1))} MB`;
}
const failed = (message: string): CommandResult => ({ ok: false, message });
export class Surfaces {
    readonly shelf = new ArtifactShelf();
    readonly desk = new BrowserDesk();
    readonly #client: LiveClient;
    readonly #changed: () => void;
    readonly #nextId: () => string;
    #agent: AgentState = 'idle';
    /** `changed` publishes a new snapshot; `nextId` names each request sent. */
    constructor(client: LiveClient, changed: () => void, nextId: () => string) {
        this.#client = client;
        this.#changed = changed;
        this.#nextId = nextId;
    }
    /** Apply one event. Returns a sentence, having changed nothing, when it breaks the protocol's rules. */
    observe(body: UiEventBody, at: string): string | undefined {
        switch (body.kind) {
            case 'agent_status':
                this.#agent = body.state;
                return undefined;
            case 'artifact_listed':
                this.shelf.list(body);
                return undefined;
            case 'browser_session':
                this.desk.session(body, at);
                return undefined;
            case 'browser_frame':
                this.desk.frame(body);
                return undefined;
            case 'browser_action':
                this.desk.action(body, at);
                return undefined;
            case 'browser_control':
                return this.desk.handoff(body.session_id, body.handoff, body.frame_sequence);
            case 'activity':
            case 'chat_message':
            case 'artifact':
            case 'schedule':
            case 'confirmation':
            case 'confirmation_decided':
            case 'attachment':
                return undefined;
        }
    }
    turn(): TurnState {
        return TURNS[this.#agent];
    }
    artifacts(): Artifact[] {
        return this.shelf.artifacts();
    }
    browserTasks(): BrowserTask[] {
        return this.desk.tasks();
    }
    threads(): ActivityThread[] {
        return this.desk.threads();
    }
    messages(): Message[] {
        return this.desk.messages();
    }
    /** Ask for control of a browser session the agent drives. */
    take(sessionId: string): Promise<CommandResult> {
        return this.#control(sessionId, 'agent', { action: 'take_control', session_id: sessionId }, 'Asked for control. It moves to you when hatchling confirms.');
    }
    /** Hand a browser session back, from the newest frame the user has seen. */
    giveBack(sessionId: string): Promise<CommandResult> {
        return this.#control(sessionId, 'user', { action: 'return_control', session_id: sessionId, frame_sequence: this.desk.lastFrame(sessionId) }, 'Handing control back. Hatchling looks at the page again before it acts.');
    }
    /** Send an action and read its answer; `read` returns `undefined` for an answer that does not belong to it. */
    async #ask(action: UiAction, read: (result: UiActionResult) => CommandResult | undefined): Promise<CommandResult> {
        const reply: Reply = await this.#client.request(this.#nextId(), action);
        if (reply.kind !== 'answered')
            return failed(reply.detail);
        if (reply.result.result === 'refused')
            return failed(ACTION_REFUSALS[reply.result.reason]);
        return read(reply.result) ?? failed(UNEXPECTED);
    }
    /** Fetch the newest revision of an artifact to show it. */
    async open(artifactId: string): Promise<CommandResult> {
        const revision = this.shelf.latest(artifactId);
        if (revision === undefined)
            return failed('This file is no longer available.');
        return this.#ask({ action: 'open_artifact', artifact_id: artifactId, revision }, result => {
            if (result.result === 'artifact_refused')
                return failed(ARTIFACT_REFUSALS[result.reason]);
            if (result.result !== 'artifact_opened' || result.artifact_id !== artifactId || result.revision !== revision)
                return undefined;
            this.shelf.keep(artifactId, revision, result.body);
            this.#changed();
            return { ok: true, message: 'Opened.' };
        });
    }
    /** Save the newest revision's content, fetching it first if need be. */
    async download(artifactId: string): Promise<CommandResult> {
        if (this.shelf.body(artifactId) === undefined) {
            const opened = await this.open(artifactId);
            if (!opened.ok)
                return opened;
        }
        const download = this.shelf.download(artifactId);
        return download === undefined ? failed('This file cannot be saved from the app yet.') : { ok: true, message: 'Downloaded.', download };
    }
    /** Share the revision the user reviewed, if it is still listed. */
    async share(artifactId: string, revision: number): Promise<CommandResult> {
        if (!this.shelf.hasRevision(artifactId, revision))
            return failed('This version is no longer listed. Review the current one before sharing.');
        return this.#ask({ action: 'share_artifact', artifact_id: artifactId, revision }, result => {
            if (result.result === 'share_refused')
                return failed(SHARE_REFUSALS[result.reason]);
            if (result.result === 'artifact_refused')
                return failed(ARTIFACT_REFUSALS[result.reason]);
            return result.result === 'artifact_shared' && result.revision === revision ? { ok: true, message: 'Shared.' } : undefined;
        });
    }
    /** Save a file from the user's device as a new artifact. */
    async save(name: string, format: ArtifactFormat, text: string, dataUrl: string): Promise<CommandResult> {
        const wire = fileFormat(format);
        if (wire === undefined)
            return failed('This kind of file cannot be added from the app yet. Text files and images can.');
        let content: FileContent = { type: 'text', text };
        if (wire === 'image') {
            const image = imageFromUrl(dataUrl);
            if (image === undefined)
                return failed('This image could not be read. Use a PNG, JPEG, WebP or GIF image.');
            content = { type: 'image', ...image };
        }
        return this.#ask({ action: 'save_file', chat_id: null, name, format: wire, content }, result => {
            if (result.result === 'artifact_saved')
                return { ok: true, message: 'Saved.', id: result.artifact_id };
            if (result.result !== 'file_refused')
                return undefined;
            const refusal = result.reason;
            switch (refusal.refusal) {
                case 'too_large':
                    return failed(`This file is larger than Hatchling accepts. The limit is ${megabytes(refusal.limit)}.`);
                case 'unsupported_type':
                    return failed('Hatchling does not accept this kind of file.');
                case 'full':
                    return failed('Hatchling’s file store is full. Remove a file, then try again.');
            }
        });
    }
    /** Ask for a handoff, from the state it starts from. Control moves only when the broker reports it. */
    #control(sessionId: string, from: 'agent' | 'user', action: UiAction, accepted: string): Promise<CommandResult> {
        const unready = this.#unready(sessionId, from);
        if (unready !== undefined)
            return Promise.resolve(unready);
        return this.#ask(action, result => this.#controlAnswer(sessionId, result, accepted));
    }
    /** Why a command for a browser driven by `from` cannot be sent now, if it cannot. */
    #unready(sessionId: string, from: 'agent' | 'user'): CommandResult | undefined {
        const control = this.desk.control(sessionId);
        if (control === undefined)
            return failed(CONTROL_REFUSALS.unknown_session);
        if (!this.desk.live(sessionId))
            return failed(CONTROL_REFUSALS.finished);
        if (switching(control))
            return failed(CONTROL_REFUSALS.switching);
        if (control !== from)
            return failed(from === 'agent' ? 'You already have control.' : 'Hatchling already has control.');
        return undefined;
    }
    /** The user's own input to a browser they drive. A frame of the page after it follows. */
    input(sessionId: string, input: OwnerInput): Promise<CommandResult> {
        const unready = this.#unready(sessionId, 'user');
        if (unready !== undefined)
            return Promise.resolve(unready);
        if (input.input === 'click' && !(Number.isInteger(input.x) && Number.isInteger(input.y) && input.x >= 0 && input.y >= 0))
            return Promise.resolve(failed(INPUT_FAILURES.not_possible));
        return this.#ask({ action: 'browser_input', session_id: sessionId, input }, result => {
            if (result.result === 'input_done' && result.session_id === sessionId)
                return { ok: true, message: 'Done. The page after it follows.' };
            if (result.result === 'input_failed' && result.session_id === sessionId)
                return failed(INPUT_FAILURES[result.failure]);
            return this.#refusal(sessionId, result);
        });
    }
    #controlAnswer(sessionId: string, result: UiActionResult, accepted: string): CommandResult | undefined {
        return result.result === 'control_accepted' && result.session_id === sessionId ? { ok: true, message: accepted } : this.#refusal(sessionId, result);
    }
    #refusal(sessionId: string, result: UiActionResult): CommandResult | undefined {
        return result.result === 'control_refused' && result.session_id === sessionId ? failed(CONTROL_REFUSALS[result.reason]) : undefined;
    }
    /** Stop is the one browser command allowed whoever drives, even while control changes hands. */
    stop(sessionId: string): Promise<CommandResult> {
        if (this.desk.control(sessionId) === undefined)
            return Promise.resolve(failed(CONTROL_REFUSALS.unknown_session));
        if (!this.desk.live(sessionId))
            return Promise.resolve(failed(CONTROL_REFUSALS.finished));
        return this.#ask({ action: 'stop_browser', session_id: sessionId }, result => {
            if (result.result === 'browser_stopped' && result.session_id === sessionId)
                return { ok: true, message: 'Stopped. No more actions will run.' };
            return this.#refusal(sessionId, result);
        });
    }
}
