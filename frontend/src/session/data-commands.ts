// What keeps and clears your data, for Settings to show. These are commands
// on the machine, not buttons, on purpose: they're root's alone, so no agent
// and no stolen app address can reach them.
export interface DataCommand {
    readonly title: string;
    readonly note: string;
    readonly command: string;
}
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
