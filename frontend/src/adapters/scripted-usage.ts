// The local preview's usage (WO-056): no model runs here, so nothing is
// counted, and Settings says so. The composer's context ring and panel are
// shown with a sample of what a connected chat's last answer reports, so the
// design can be reviewed before it's wired to Hatchling. Every chat in the
// preview shows the same sample.
import type { CommandResult } from '../protocol/models.js';
import type { ContextUse, UsageLimits, UsagePort, UsageState } from '../session/usage.js';
/** What each of the preview's chats shows: an answer from a local model, its prompt split by part. */
export const SAMPLE_CONTEXT: ContextUse = {
    tokens: 8320,
    window: 65536,
    onCpu: null,
    memoryLow: false,
    model: 'gemma4:12b',
    parts: { system: 3100, tools: 2400, memory: 720, conversation: 2100 },
};
const NOTHING_COUNTED = 'The local preview runs no model, so it counts no usage. Connect to Hatchling to see yours.';
export class ScriptedUsage implements UsagePort {
    current(): UsageState {
        return { kind: 'failed', detail: NOTHING_COUNTED };
    }
    refresh(): Promise<void> {
        return Promise.resolve();
    }
    setLimits(_limits: UsageLimits): Promise<CommandResult> {
        return Promise.resolve({ ok: false, message: NOTHING_COUNTED });
    }
    context(_chatId: string): ContextUse | null {
        return SAMPLE_CONTEXT;
    }
}
