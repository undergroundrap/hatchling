// What keeps and clears your data, for Settings to show. These are commands
// on the machine, not buttons, on purpose: they're root's alone, so no agent
// and no stolen app address can reach them.
export interface DataCommand {
    readonly title: string;
    readonly note: string;
    readonly command: string;
}
/** Where the model runs. Both can be set up, and switched between in the app. */
export const MODEL_COMMANDS: readonly DataCommand[] = [
    {
        title: 'A hosted model',
        note: 'Fast to start, with no model on this machine. It asks for your key at its own prompt. Your words go to the host.',
        command: 'sudo hatchling-broker use-hosted openrouter',
    },
    {
        title: 'A model on this machine',
        note: 'Private, and works offline. Needs a model running here, such as one from Ollama.',
        command: 'sudo hatchling-broker use-local --model <name>',
    },
];
/** Who can open the vault. */
export const KEY_COMMANDS: readonly DataCommand[] = [
    {
        title: 'Protect it with a security key',
        note: 'Guided, and safe to run again. Without a key, the vault\u2019s key sits in a file here, and it opens by itself after a restart.',
        command: 'sudo hatchling-broker vault-keyring setup',
    },
    {
        title: 'Lost a key, or had one stolen',
        note: 'Removes it, and offers a replacement.',
        command: 'sudo hatchling-broker vault-keyring lost-key',
    },
    {
        title: 'Lock it now',
        note: 'Takes the vault\u2019s key out of memory until you unlock it. It needs a security key.',
        command: 'sudo hatchling-broker lock',
    },
    {
        title: 'Check everything',
        note: 'Says what stops Hatchling working here, and the fix.',
        command: 'sudo hatchling-broker doctor',
    },
];
/** Copying your data out, and bringing it back. */
export const KEEP_COMMANDS: readonly DataCommand[] = [
    {
        title: 'Back up',
        note: 'One sealed file, which opens with what opens your vault.',
        command: 'sudo hatchling-broker backup',
    },
    {
        title: 'Restore',
        note: 'Puts a backup back, in place of what’s here now.',
        command: 'sudo hatchling-broker restore <file>',
    },
];
/** Starting over: each takes a backup first, and asks before it clears. */
export const RESET_COMMANDS: readonly DataCommand[] = [
    {
        title: 'New conversation',
        note: 'Clears the conversation. Everything else stays.',
        command: 'sudo hatchling-broker reset conversation',
    },
    {
        title: 'Reset the agent',
        note: 'Clears its conversation, memory, goals, schedules and work. Your API keys, sign-ins and security keys stay.',
        command: 'sudo hatchling-broker reset agent',
    },
    {
        title: 'Erase everything',
        note: 'Also erases your API keys and sign-ins. Your security keys aren’t changed.',
        command: 'sudo hatchling-broker reset everything',
    },
];
