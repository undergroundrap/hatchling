// The first-run checklist: what's set up on this machine, as the broker says,
// and what's left. Everything here is a step to take in a terminal the owner
// opened: a PIN, a key or a backup code never passes through this app, so an
// agent or a stolen app address can't reach one.
import { AUTHENTICATOR_COMMAND, MODEL_COMMANDS, BACKUP_COMMAND, onWindows, PROTECT_COMMAND } from './data-commands.js';
import type { DataCommand } from './data-commands.js';
import { whereShort } from './model.js';
import type { ModelView } from './model.js';
export type VaultTier = 'file' | 'totp' | 'hardware_key';
/** Why the agent's computer is off. */
export type RoomDiskOff = 'not_here' | 'not_opened' | 'not_read';
/** The agent's computer, its room disk: how it is, and its sizes in bytes, as the broker said. */
export interface RoomDiskView {
    readonly state: 'absent' | 'closed' | 'mapped' | 'open' | 'off';
    readonly off: RoomDiskOff | null;
    /** The most it holds. */
    readonly cap: number | null;
    /** In use inside, while it's open. */
    readonly used: number | null;
    /** Free inside, while it's open. */
    readonly free: number | null;
    /** What it takes of this machine's disk. */
    readonly onDisk: number | null;
    /** Free on this machine's disk. */
    readonly hostFree: number | null;
}
export interface SetupView {
    readonly tier: VaultTier;
    /** The security keys that open the vault. */
    readonly keys: number;
    /** The backup codes left. */
    readonly codes: number;
    readonly codesUnconfirmed: boolean;
    /** When the last backup was made, in Unix seconds. */
    readonly lastBackup: number | null;
    /** Whether the Windows setup made this install. */
    readonly windowsSetup: boolean;
    /** The agent's computer: none from a broker from before there was one. */
    readonly roomDisk: RoomDiskView | null;
    /** Whether the agent's rooms are held by process limits alone, with no cgroup of their own. */
    readonly processLimitsOnly: boolean;
    /** Whether Windows keeps Hatchling running while the owner is logged in: none off Windows, or before it said. */
    readonly keepRunning: boolean | null;
    /** The latest stretch Hatchling wasn't running: none when there's been none. */
    readonly stopped: StoppedView | null;
}
/** A stretch Hatchling wasn't running, stopped or with the machine asleep, in Unix seconds. */
export interface StoppedView {
    readonly from: number;
    readonly to: number;
}
/** What's set up, as last read. */
export type SetupState = {
    readonly kind: 'unread';
} | {
    readonly kind: 'failed';
    readonly detail: string;
} | {
    readonly kind: 'read';
    readonly setup: SetupView;
};
/** The choices this browser remembers: only that a step was put off, nothing else. */
export type SetupChoice = 'closed' | 'no-key';
export interface SetupPort {
    current(): SetupState;
    refresh(): Promise<void>;
    remembers(choice: SetupChoice): boolean;
    remember(choice: SetupChoice): void;
}
export type StepStatus = 'done' | 'next' | 'skipped';
export interface SetupStep {
    readonly id: 'model' | 'vault' | 'backup' | 'running';
    readonly title: string;
    readonly status: StepStatus;
    readonly said: string;
    /** What to run for this step. */
    readonly commands: readonly DataCommand[];
    /** Whether the owner may go on without it. */
    readonly optional: boolean;
}
const NONE: readonly DataCommand[] = [];
const AFTER_KEY = 'A security key means nothing opens the vault without you. Hatchling then waits for you to unlock it after each restart.';
function plural(count: number, one: string, many: string): string {
    return `${count} ${count === 1 ? one : many}`;
}
/** How long ago `seconds` was, at `now`, both in Unix seconds. */
export function ago(seconds: number, now: number): string {
    const days = Math.floor((now - seconds) / 86400);
    if (days < 1)
        return 'today';
    return days === 1 ? 'yesterday' : `${days} days ago`;
}
function modelStep(model: ModelView): SetupStep {
    const name = model.chosen ?? model.configured;
    const base = { id: 'model', title: 'Choose your model', optional: false } as const;
    if (model.endpoint === null || name === null)
        return { ...base, status: 'next', said: 'Hatchling needs a model to think with. Set up one, or both: with both, you switch between them in Settings.', commands: MODEL_COMMANDS };
    return { ...base, status: 'done', said: `Hatchling uses ${name}, ${whereShort(model.endpoint)}.`, commands: NONE };
}
function keyedVault(setup: SetupView): Pick<SetupStep, 'status' | 'said' | 'commands'> {
    const keys = plural(setup.keys, 'security key opens', 'security keys open');
    if (setup.codesUnconfirmed)
        return { status: 'next', said: `${keys} your vault, but your backup codes were never confirmed saved. Run setup again for a fresh set.`, commands: [PROTECT_COMMAND] };
    if (setup.codes === 0)
        return { status: 'next', said: `${keys} your vault, and no backup codes are left. Run setup again for a fresh set.`, commands: [PROTECT_COMMAND] };
    const codes = plural(setup.codes, 'backup code', 'backup codes');
    if (setup.keys === 1)
        return { status: 'done', said: `One security key opens your vault, with ${codes} to fall back on. Add a spare, so losing it is no trouble: setup carries on from here.`, commands: [PROTECT_COMMAND] };
    return { status: 'done', said: `${keys} your vault, any one of them, with ${codes} to fall back on.`, commands: NONE };
}
function vaultStep(setup: SetupView, noKey: boolean): SetupStep {
    const base = { id: 'vault', title: 'Protect your vault', optional: true } as const;
    if (setup.tier === 'hardware_key')
        return { ...base, ...keyedVault(setup) };
    if (setup.tier === 'totp')
        return { ...base, status: 'done', said: 'A code from your authenticator app opens your vault.', commands: NONE };
    return {
        ...base,
        status: noKey ? 'skipped' : 'next',
        said: noKey ? `Skipped: your vault’s key sits in a file on this machine, and it opens by itself after a restart. ${AFTER_KEY}` : `Your API keys and sign-ins are sealed, but the key to them sits in a file on this machine. ${AFTER_KEY} Hatchling works without one.`,
        commands: [PROTECT_COMMAND, AUTHENTICATOR_COMMAND],
    };
}
function backupStep(setup: SetupView, now: number): SetupStep {
    const base = { id: 'backup', title: 'Make a backup', optional: false } as const;
    if (setup.lastBackup === null)
        return { ...base, status: 'next', said: 'One sealed file with everything Hatchling knows. It opens with what opens your vault.', commands: [BACKUP_COMMAND] };
    return { ...base, status: 'done', said: `Your last backup was made ${ago(setup.lastBackup, now)}.`, commands: NONE };
}
/** On a Windows install that said, whether Windows keeps Hatchling running: done, or off as the owner chose in Setup. */
function keepRunningStep(setup: SetupView): SetupStep | null {
    if (!setup.windowsSetup || typeof setup.keepRunning !== 'boolean')
        return null;
    const base = { id: 'running', title: 'Keep Hatchling running', optional: true, commands: NONE } as const;
    if (setup.keepRunning)
        return { ...base, status: 'done', said: 'Windows keeps Hatchling running while you’re logged in, so scheduled and background work wakes on time.' };
    return { ...base, status: 'skipped', said: 'Off: Hatchling stops soon after its last window closes, so scheduled and background work runs only while it’s open. To turn it on, run Hatchling Setup again and keep “Keep Hatchling running while I’m logged in” ticked.' };
}
/** How long after it ended a stop is still said: a week. */
export const STOP_SAID_FOR = 7 * 86400;
/** When `seconds` was, as this browser says a moment. */
export function whenSaid(seconds: number): string {
    return new Date(seconds * 1000).toLocaleString(undefined, { weekday: 'short', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
}
/** What the app says of the latest stretch Hatchling wasn't running, at `now`, while it ended within the last week. */
export function stoppedNote(stopped: StoppedView | null, now: number, when: (seconds: number) => string = whenSaid): string | null {
    if (stopped === null || now - stopped.to > STOP_SAID_FOR)
        return null;
    return `Hatchling wasn’t running from ${when(stopped.from)} to ${when(stopped.to)}, so anything scheduled for then didn’t run on time.`;
}
/** Where each step is on a Windows install: the Start menu, with no command to type. */
const START_MENU: Readonly<Partial<Record<SetupStep['id'], string>>> = {
    vault: 'Open the Start menu and type Protect your vault.',
    backup: 'Open the Start menu and type Make a backup.',
};
function onWindowsStep(step: SetupStep): SetupStep {
    const place = START_MENU[step.id];
    if (step.commands.length === 0)
        return step;
    return place === undefined ? { ...step, commands: step.commands.map(onWindows) } : { ...step, commands: NONE, said: `${step.said} ${place}` };
}
/** The steps, in order, with `noKey` when the owner chose to go without a key. */
export function setupSteps(model: ModelView, setup: SetupView, noKey: boolean, now: number): readonly SetupStep[] {
    const running = keepRunningStep(setup);
    const steps = [modelStep(model), vaultStep(setup, noKey), backupStep(setup, now), ...(running === null ? [] : [running])];
    return setup.windowsSetup ? steps.map(onWindowsStep) : steps;
}
/** Whether nothing's left to do. */
export function setupFinished(steps: readonly SetupStep[]): boolean {
    return steps.every(step => step.status !== 'next');
}
/** How `steps` stand, in a few words. */
export function setupProgress(steps: readonly SetupStep[]): string {
    return `${steps.filter(step => step.status !== 'next').length} of ${steps.length} done`;
}
/** What the chat says about `steps`, until the owner closes the checklist. */
export function setupReminder(steps: readonly SetupStep[]): {
    readonly title: string;
    readonly note: string;
} {
    const next = steps.find(step => step.status === 'next');
    if (next === undefined)
        return { title: 'You’re all set', note: 'Nothing’s left to set up. The checklist stays in Settings, under General.' };
    return { title: 'Finish setting up', note: `${setupProgress(steps)}. Next: ${next.title.toLowerCase()}.` };
}
/** What a step's status is called, for what can't see its mark. */
export const STEP_STATUS: Readonly<Record<StepStatus, string>> = { done: 'Done', next: 'To do', skipped: 'Skipped' };
