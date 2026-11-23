/** Validate persisted commands at the untrusted local-storage boundary. Never cast parsed JSON. */
import type { ArtifactFormat, Command, GoalCategory, Scenario, Theme } from './models.js';
export function isRecord(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
}
function str(record: Record<string, unknown>, key: string): string {
    const value = record[key];
    if (typeof value !== 'string' || value.length > 3000000)
        throw new Error('Invalid text field.');
    return value;
}
function num(record: Record<string, unknown>, key: string): number {
    const value = record[key];
    if (typeof value !== 'number' || !Number.isFinite(value))
        throw new Error('Invalid numeric field.');
    return value;
}
function bool(record: Record<string, unknown>, key: string): boolean {
    const value = record[key];
    if (typeof value !== 'boolean')
        throw new Error('Invalid toggle field.');
    return value;
}
export function isAccent(value: unknown): value is import('./models.js').Accent { return ['white', 'avatar', 'blue', 'azure', 'violet', 'pink', 'orange', 'green', 'stone'].some((item) => item === value); }
export function isTheme(value: unknown): value is Theme { return value === 'system' || value === 'dark' || value === 'light'; }
export function isScenario(value: unknown): value is Scenario { return ['browser-watch', 'bundle-files', 'research', 'purchase', 'price-change', 'unknown-payment', 'captcha', 'persistent-captcha', 'fix-ci', 'channels', 'secure', 'choices', 'mixed-stream', 'interrupted', 'stalled'].some((item) => item === value); }
export function isFormat(value: unknown): value is ArtifactFormat { return ['archive', 'markdown', 'pdf', 'word', 'spreadsheet', 'csv', 'slides', 'text', 'code', 'html', 'app', 'image', 'video', 'audio'].some((item) => item === value); }
export function isGoalCategory(value: unknown): value is GoalCategory { return ['Health & fitness', 'Relationships', 'Money', 'Career', 'Interests', 'Productivity', 'Something else'].some((item) => item === value); }
export function parseCommand(value: unknown): Command | null {
    if (!isRecord(value))
        return null;
    try {
        const type = value['type'];
        switch (type) {
            case 'settings.interface-size': {
                const size = value['value'];
                return size === 'compact' || size === 'comfortable' || size === 'large' ? { type, value: size } : null;
            }
            case 'browser.advance': return { type, id: str(value, 'id'), sequence: num(value, 'sequence') };
            case 'browser.input': return { type, id: str(value, 'id'), text: str(value, 'text') };
            case 'identity.avatar': return { type, value: str(value, 'value'), ...(value['accent'] !== undefined ? { accent: str(value, 'accent') } : {}) };
            case 'support.download': return { type, title: str(value, 'title'), body: str(value, 'body') };
            case 'permission.network': return { type, id: str(value, 'id'), enabled: bool(value, 'enabled') };
            case 'skill.enable': return { type, name: str(value, 'name'), digest: str(value, 'digest') };
            case 'skill.disable': return { type, name: str(value, 'name') };
            case 'permission.website': {
                const policy = value['policy'];
                return policy === 'allow' || policy === 'ask' || policy === 'block' ? { type, host: str(value, 'host'), policy } : null;
            }
            case 'chat.read': return { type, id: str(value, 'id') };
            case 'settings.chat-panel': return { type, value: bool(value, 'value') };
            case 'settings.accent': {
                const accent = value['value'];
                return isAccent(accent) ? { type, value: accent } : null;
            }
            case 'chat.send': return { type, chatId: str(value, 'chatId'), text: str(value, 'text') };
            case 'chat.attach': return { type, chatId: str(value, 'chatId'), artifactId: str(value, 'artifactId'), ...(value['avatarAccent'] !== undefined ? { avatarAccent: str(value, 'avatarAccent') } : {}) };
            case 'artifact.import': {
                const format = value['format'];
                return isFormat(format) ? { type, title: str(value, 'title'), format, content: str(value, 'content'), dataUrl: str(value, 'dataUrl') } : null;
            }
            case 'chat.create': return { type, title: str(value, 'title') };
            case 'chat.rename': return { type, id: str(value, 'id'), title: str(value, 'title') };
            case 'message.react': return { type, id: str(value, 'id'), reaction: str(value, 'reaction') };
            case 'message.choose': return { type, id: str(value, 'id'), option: str(value, 'option') };
            case 'chat.toggle-pin': return { type, id: str(value, 'id') };
            case 'chat.archive': return { type, id: str(value, 'id') };
            case 'chat.delete': return { type, id: str(value, 'id') };
            case 'message.secure-complete': return { type, id: str(value, 'id') };
            case 'message.code-complete': return { type, id: str(value, 'id') };
            case 'artifact.open': return { type, id: str(value, 'id') };
            case 'artifact.pin': return { type, id: str(value, 'id') };
            case 'artifact.trash': return { type, id: str(value, 'id') };
            case 'artifact.restore': return { type, id: str(value, 'id') };
            case 'artifact.download': return { type, id: str(value, 'id') };
            case 'goal.close': return { type, id: str(value, 'id') };
            case 'goal.reopen': return { type, id: str(value, 'id') };
            case 'idea.activate': return { type, id: str(value, 'id') };
            case 'idea.dismiss': return { type, id: str(value, 'id') };
            case 'idea.restore': return { type, id: str(value, 'id') };
            case 'feed.react': return { type, id: str(value, 'id') };
            case 'connector.begin': return { type, id: str(value, 'id') };
            case 'connector.finish': return { type, id: str(value, 'id') };
            case 'connector.disconnect': return { type, id: str(value, 'id') };
            case 'channel.begin': return { type, id: str(value, 'id') };
            case 'channel.finish': return { type, id: str(value, 'id') };
            case 'channel.check': return { type, id: str(value, 'id') };
            case 'channel.disconnect': return { type, id: str(value, 'id') };
            case 'channel.restart': return { type, id: str(value, 'id') };
            case 'purchase.submit': return { type, id: str(value, 'id') };
            case 'purchase.decline': return { type, id: str(value, 'id') };
            case 'wallet.connect': return { type, id: str(value, 'id') };
            case 'wallet.reauth': return { type, id: str(value, 'id') };
            case 'wallet.add-method': return { type, id: str(value, 'id') };
            case 'browser.takeover': return { type, id: str(value, 'id') };
            case 'browser.resume': return { type, id: str(value, 'id') };
            case 'browser.grant': return { type, id: str(value, 'id') };
            case 'browser.solve': return { type, id: str(value, 'id') };
            case 'browser.cancel': return { type, id: str(value, 'id') };
            case 'browser.start': return { type, id: str(value, 'id') };
            case 'device.remove': return { type, id: str(value, 'id') };
            case 'memory.delete': return { type, id: str(value, 'id') };
            case 'job.toggle': return { type, id: str(value, 'id') };
            case 'job.run': return { type, id: str(value, 'id') };
            case 'job.delete': return { type, id: str(value, 'id') };
            case 'notification.read': return { type, id: str(value, 'id') };
            case 'notification.answer': return { type, id: str(value, 'id') };
            case 'turn.interrupt':
            case 'turn.resume':
            case 'demo.reset':
            case 'feed.generate':
            case 'permission.reset':
            case 'data.export': return { type };
            case 'demo.advance': return { type, step: num(value, 'step') };
            case 'demo.scenario': {
                const scenario = value['scenario'];
                return isScenario(scenario) ? { type, scenario } : null;
            }
            case 'artifact.create': {
                const format = value['format'];
                return isFormat(format) ? { type, title: str(value, 'title'), format } : null;
            }
            case 'artifact.app-toggle': return { type, id: str(value, 'id'), index: num(value, 'index') };
            case 'artifact.save': return { type, id: str(value, 'id'), content: str(value, 'content') };
            case 'artifact.save-slide': return { type, id: str(value, 'id'), slide: num(value, 'slide'), layerId: str(value, 'layerId'), text: str(value, 'text'), x: num(value, 'x'), y: num(value, 'y'), width: num(value, 'width'), size: num(value, 'size') };
            case 'artifact.publish': return { type, id: str(value, 'id'), approvedVersion: num(value, 'approvedVersion') };
            case 'goal.create': {
                const category = value['category'];
                return isGoalCategory(category) ? { type, title: str(value, 'title'), description: str(value, 'description'), category, due: str(value, 'due') } : null;
            }
            case 'goal.update': return { type, id: str(value, 'id'), title: str(value, 'title'), description: str(value, 'description'), due: str(value, 'due') };
            case 'idea.edit': return { type, id: str(value, 'id'), summary: str(value, 'summary') };
            case 'feed.prompt': return { type, text: str(value, 'text') };
            case 'purchase.approve': return { type, id: str(value, 'id'), revision: num(value, 'revision'), total: num(value, 'total') };
            case 'purchase.method': return { type, id: str(value, 'id'), methodId: str(value, 'methodId') };
            case 'purchase.requote': return { type, id: str(value, 'id'), total: num(value, 'total') };
            case 'permission.set': {
                const mode = value['mode'];
                return mode === 'ask-for-some' || mode === 'always-ask' ? { type, id: str(value, 'id'), mode } : null;
            }
            case 'device.pair': return { type, name: str(value, 'name') };
            case 'device.command': return { type, id: str(value, 'id'), capability: str(value, 'capability') };
            case 'memory.save': return { type, id: str(value, 'id'), title: str(value, 'title'), body: str(value, 'body'), ...(value['expectedBody'] !== undefined ? { expectedBody: str(value, 'expectedBody') } : {}) };
            case 'job.update': {
                const kind = value['kind'];
                return kind === 'time' || kind === 'hook' ? { type, id: str(value, 'id'), title: str(value, 'title'), kind, schedule: str(value, 'schedule'), recurring: bool(value, 'recurring'), source: str(value, 'source'), event: str(value, 'event') } : null;
            }
            case 'job.create': {
                const kind = value['kind'];
                return kind === 'time' || kind === 'hook' ? { type, title: str(value, 'title'), kind, schedule: str(value, 'schedule'), recurring: bool(value, 'recurring'), source: str(value, 'source'), event: str(value, 'event') } : null;
            }
            case 'settings.theme': {
                const theme = value['value'];
                return isTheme(theme) ? { type, value: theme } : null;
            }
            case 'identity.update': return { type, name: str(value, 'name'), vibe: str(value, 'vibe') };
            case 'settings.name':
            case 'settings.language':
            case 'settings.allowlist': return { type, value: str(value, 'value') };
            case 'settings.telemetry':
            case 'settings.captcha': return { type, value: bool(value, 'value') };
            case 'settings.digest': return { type, time: str(value, 'time'), start: str(value, 'start'), end: str(value, 'end'), allowance: num(value, 'allowance') };
            case 'secure.add':
            case 'secure.remove': return { type, service: str(value, 'service') };
            default: return null;
        }
    }
    catch {
        return null;
    }
}
