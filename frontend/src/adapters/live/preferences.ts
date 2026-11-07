// How the app looks: this browser's to remember, as the broker has no part in
// it. What comes back from storage is checked, as anything can write there.
import type { Accent, InterfaceSize, Settings, Theme } from '../../protocol/models.js';
export type Preferences = Pick<Settings, 'theme' | 'accent' | 'interfaceSize' | 'keepChatPanelVisible'>;
const KEY = 'hatchling-preferences';
const THEMES: readonly Theme[] = ['dark', 'light', 'system'];
const SIZES: readonly InterfaceSize[] = ['compact', 'comfortable', 'large'];
const ACCENTS: readonly Accent[] = ['white', 'avatar', 'blue', 'azure', 'violet', 'pink', 'orange', 'green', 'stone'];
function isOneOf<T>(choices: readonly T[], value: unknown): value is T {
    return choices.some(choice => choice === value);
}
function isRecord(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null;
}
/** What this browser remembers, leaving out anything that isn't a real choice. */
export function rememberedPreferences(): Partial<Preferences> {
    try {
        const kept: unknown = JSON.parse(window.localStorage.getItem(KEY) ?? '{}');
        if (!isRecord(kept))
            return {};
        const { theme, accent, interfaceSize, keepChatPanelVisible } = kept;
        return {
            ...(isOneOf(THEMES, theme) ? { theme } : {}),
            ...(isOneOf(SIZES, interfaceSize) ? { interfaceSize } : {}),
            ...(isOneOf(ACCENTS, accent) ? { accent } : {}),
            ...(typeof keepChatPanelVisible === 'boolean' ? { keepChatPanelVisible } : {}),
        };
    }
    catch {
        return {};
    }
}
/** Keeps `preferences` for later visits, if this browser keeps anything. */
export function keepPreferences(preferences: Preferences): boolean {
    try {
        window.localStorage.setItem(KEY, JSON.stringify(preferences));
        return true;
    }
    catch {
        return false;
    }
}
