// The agent's computer: its room disk, an encrypted disk image, in the app's
// words. Only what the broker said is said: its sizes, why it's off, and what
// its encryption is worth on this install's tier. Nothing here claims the
// agent uses it yet.
import type { RoomDiskOff, RoomDiskView, VaultTier } from './setup.js';
/** Why it doesn't shrink: said wherever its size is. */
export const NO_SHRINK = 'Deleting files inside it doesn’t make it smaller on your disk. Giving space back would tell your disk which parts are in use, and hiding that is part of what the encryption is for.';
/** On a Windows install, where Linux runs in a disk of its own. */
export const ON_WINDOWS = 'Windows doesn’t get that space back on its own, either.';
const OFF: Readonly<Record<RoomDiskOff, string>> = {
    not_here: 'It’s off on this install: it needs Hatchling running as a service, as root.',
    not_opened: 'Your agent’s computer is off because it failed to start. hatchling-broker doctor, run as root, says why.',
    not_read: 'Hatchling couldn’t read how it is just now.',
};
/** A close left it unlocked; another mount may still be using it. */
export const STILL_UNLOCKED = 'Its storage is still unlocked on this machine: something kept it from being closed. hatchling-broker doctor, run as root, says more.';
/** The start failed, but cleanup also left the disk mounted. */
export const STILL_MOUNTED = 'Its storage is still unlocked and mounted on this machine. hatchling-broker doctor, run as root, says more.';
/** What its encryption is worth, by how the vault opens: what opens it is the vault's. */
const LOCKED: Readonly<Record<VaultTier, string>> = {
    hardware_key: 'It’s encrypted. Your security key protects the vault that opens it. Old key copies or memory left on disk can still expose its contents.',
    totp: 'It’s encrypted, but what opens it is on this same disk until you protect your vault.',
    file: 'It’s encrypted, but what opens it is on this same disk until you protect your vault.',
};
/** `bytes`, as a person reads them: gigabytes to a tenth, or whole megabytes. */
export function diskSize(bytes: number): string {
    return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.round(bytes / 1e6)} MB`;
}
/** How big it is, and what it takes here, when that's known. */
function sizes(disk: RoomDiskView): string | null {
    if (disk.cap === null || disk.onDisk === null)
        return null;
    const used = disk.used === null ? '' : `, ${diskSize(disk.used)} of it in use,`;
    return `It holds up to ${diskSize(disk.cap)}${used} and takes ${diskSize(disk.onDisk)} on this machine.`;
}
/** What's said of `disk` on an install whose vault opens at `tier`, made by the Windows setup or not. */
export function roomDiskLines(disk: RoomDiskView, tier: VaultTier, windows: boolean): readonly string[] {
    if (disk.state === 'absent')
        return ['It hasn’t been made yet. Hatchling makes it when it starts.'];
    const warning = disk.state === 'mapped' ? [STILL_UNLOCKED] : disk.state === 'open' && disk.off !== null ? [STILL_MOUNTED] : [];
    const reason = disk.off !== null || disk.state === 'off' ? [OFF[disk.off ?? 'not_read']] : [];
    const lines = [...warning, ...reason];
    const said = sizes(disk);
    if (said === null)
        return lines;
    return [...lines, said, NO_SHRINK, ...(windows ? [ON_WINDOWS] : []), LOCKED[tier]];
}
