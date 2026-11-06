// The first-run checklist: what's set up on this machine, as the broker says,
// and what's left. Everything here is a step to take in a terminal the owner
// opened: a PIN, a key or a backup code never passes through this app, so an
// agent or a stolen app address can't reach one.
import { MODEL_COMMANDS, BACKUP_COMMAND, PROTECT_COMMAND } from './data-commands.js';
import type { DataCommand } from './data-commands.js';
import { whereShort } from './model.js';
import type { ModelView } from './model.js';
export type VaultTier = 'file' | 'totp' | 'hardware_key';
export interface SetupView {
    readonly tier: VaultTier;
    /** The security keys that open the vault. */
    readonly keys: number;
    /** The backup codes left. */
    readonly codes: number;
    readonly codesUnconfirmed: boolean;
    /** When the last backup was made, in Unix seconds. */
    readonly lastBackup: number | null;
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
    readonly id: 'model' | 'vault' | 'backup';
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
        commands: [PROTECT_COMMAND],
    };
}
function backupStep(setup: SetupView, now: number): SetupStep {
    const base = { id: 'backup', title: 'Make a backup', optional: false } as const;
    if (setup.lastBackup === null)
        return { ...base, status: 'next', said: 'One sealed file with everything Hatchling knows. It opens with what opens your vault.', commands: [BACKUP_COMMAND] };
    return { ...base, status: 'done', said: `Your last backup was made ${ago(setup.lastBackup, now)}.`, commands: NONE };
}
/** The steps, in order, with `noKey` when the owner chose to go without a key. */
export function setupSteps(model: ModelView, setup: SetupView, noKey: boolean, now: number): readonly SetupStep[] {
    return [modelStep(model), vaultStep(setup, noKey), backupStep(setup, now)];
}
/** Whether nothing's left to do. */
export function setupFinished(steps: readonly SetupStep[]): boolean {
    return steps.every(step => step.status !== 'next');
}
/** How `steps` stand, in a few words. */
export function setupProgress(steps: readonly SetupStep[]): string {
    return `${steps.filter(step => step.status !== 'next').length} of ${steps.length} done`;
}
/** What a step's status is called, for what can't see its mark. */
export const STEP_STATUS: Readonly<Record<StepStatus, string>> = { done: 'Done', next: 'To do', skipped: 'Skipped' };
