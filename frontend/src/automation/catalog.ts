import { commandSchemas } from './command-schemas.js';
import { actionPolicy } from './policy.js';
import type { ActionDescription, ActionSchema, InputField } from './contracts.js';
function view(name: string, title: string, properties: Record<string, InputField> = {}, required: readonly string[] = []): ActionDescription {
    return { name, title, policy: 'view', inputSchema: { type: 'object', properties: { type: { type: 'string', enum: [name] }, ...properties }, required: ['type', ...required], additionalProperties: false } };
}
const text: InputField = { type: 'string' };
export const routes = ['chat', 'search', 'feed', 'ideas', 'goals', 'library', 'browser', 'memory', 'schedule', 'notifications', 'published'] as const;
export const settingsSections = ['general', 'connectors', 'wallet', 'secure', 'permissions', 'skills', 'channels', 'devices', 'data', 'help', 'legal'] as const;
export const viewActions: readonly ActionDescription[] = [
    view('ui.navigate', 'Navigate to a workspace area', { route: { type: 'string', enum: routes }, detail: text }, ['route']),
    view('ui.settings', 'Open Settings', { section: { type: 'string', enum: settingsSections } }),
    view('ui.context', 'Select a context tab', { tab: { type: 'string', enum: ['activity', 'approvals', 'schedule', 'identity'] }, open: { type: 'boolean' } }, ['tab', 'open']),
    view('ui.side-chats', 'Expand or collapse the side-chat list', { open: { type: 'boolean' } }, ['open']),
    view('ui.prefill', 'Append a draft without sending', { text }, ['text']),
    view('ui.artifact', 'Open a file or page artifact', { id: text }, ['id']),
    view('ui.document', 'Open a memory or identity document', { id: text }, ['id']),
    view('ui.activity', 'Open an activity evidence viewer', { id: text }, ['id']),
    view('ui.browser', 'Watch an existing browser task without controlling it', { id: text }, ['id']),
    view('ui.close-workspace', 'Close the artifact workspace; do not stop a task'),
    view('ui.search', 'Open Search'),
];
export function actionCatalog(): readonly ActionDescription[] {
    return [...viewActions, ...Object.entries(commandSchemas).map(([name, inputSchema]) => ({ name, title: name.replace(/[.-]/gu, ' '), policy: policyFor(name), inputSchema }))];
}
export function policyFor(name: string): 'review' | 'human-only' | 'internal' {
    const entry = Object.entries(actionPolicy).find(([key]) => key === name);
    return entry?.[1] ?? 'internal';
}
/** Reject extra fields as well as malformed values. Never silently discard untrusted input. */
export function validateArguments(schema: ActionSchema, value: Record<string, unknown>): boolean {
    if (Object.keys(value).some(key => !(key in schema.properties)) || schema.required.some(key => value[key] === undefined))
        return false;
    return Object.entries(value).every(([key, field]) => {
        const definition = schema.properties[key];
        if (!definition || typeof field !== definition.type)
            return false;
        if (typeof field === 'string' && field.length > 100000)
            return false;
        if (typeof field === 'number' && !Number.isFinite(field))
            return false;
        return !definition.enum || definition.enum.some(option => option === field);
    });
}
