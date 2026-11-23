import type { AppContext } from '../../app/context.js';
import { h } from '../../components/dom.js';
import { icon } from '../../components/icons.js';
import { button } from '../../components/ui.js';
import type { SkillState } from '../../protocol/models.js';
import type { SettingsNavigation } from './settings-navigation.js';
// The signer's words are shown with any hidden character revealed, as an approval's are.
import { revealedValue } from '../approvals/fields.js';
/** Where a skill stands, in the owner's words. */
const STATES: Readonly<Record<SkillState, string>> = {
    enabled: 'Enabled',
    waiting: 'Waiting for you',
    changed: 'Changed since you enabled it',
};
/** What a capability lets a skill's program do, for the ones Hatchling names. */
const CAPABILITIES: Readonly<Record<string, string>> = {
    'fs.read': 'Read the files a task gives it',
    'fs.write': 'Write files in its task',
    'net.egress': 'Reach the network, through Hatchling’s allowlist',
    'memory.read': 'Read what you approved for its memory',
    'goals.query': 'Read your goals',
    'text.read': 'Read the text it is given',
    'text.transform': 'Rewrite the text it is given',
    'artifact.build': 'Build a file for you to review',
};
/** A digest or fingerprint, short enough to compare by eye. */
export function short(hex: string): string {
    return hex.length > 16 ? `${hex.slice(0, 8)}…${hex.slice(-8)}` : hex;
}
/** A size in the units people read. */
export function size(bytes: number): string {
    if (bytes < 1024)
        return `${bytes} B`;
    if (bytes < 1024 * 1024)
        return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
/** What enabling `skill` lets it do, one line each. */
export function grants(skill: {
    readonly capabilities: readonly string[];
    readonly program: string | null;
}): string[] {
    if (skill.capabilities.length === 0)
        return [skill.program === null ? 'Nothing more: it is a playbook, with no program to run' : 'Nothing more: its program declares no capabilities'];
    return skill.capabilities.map(capability => CAPABILITIES[capability] ?? capability);
}
/** The signed skills Hatchling found: read here, then enabled or disabled by the owner alone. */
export function skillsTab(ctx: AppContext, nav: SettingsNavigation): HTMLElement {
    if (nav.detail === 'skill')
        return detail(ctx, nav);
    const group = h('div', { class: 'settings-group reference-list skills-list' });
    for (const skill of ctx.state.skills) {
        const row = h('button', { type: 'button', class: 'reference-setting-row skill-row', 'aria-label': `Review ${skill.name}`, 'data-skill': skill.name, 'data-state': skill.state }, h('span', { class: 'website-icon' }, icon(skill.state === 'changed' ? 'warning' : 'spark', 18)), h('span', { class: 'setting-row-copy' }, revealedValue(skill.name), h('span', { class: 'setting-description' }, `${STATES[skill.state]} · `, revealedValue(skill.version))), icon('chevron', 16));
        row.addEventListener('click', () => nav.open('skill', skill.name));
        group.append(row);
    }
    const empty = h('p', { class: 'settings-note' }, 'No skills yet. A skill is a signed folder the operator puts in Hatchling’s skills directory; it shows here for you to read and enable.');
    return h('div', { class: 'settings-stack' }, h('section', {}, h('h3', { class: 'settings-section-title' }, 'Your skills'), ctx.state.skills.length ? group : empty), h('p', { class: 'settings-note' }, 'A skill runs only after you enable it, and only as you saw it here: if it changes, it stops until you enable it again.'));
}
function detail(ctx: AppContext, nav: SettingsNavigation): HTMLElement {
    const skill = ctx.state.skills.find(entry => entry.name === nav.id);
    if (!skill)
        return h('p', {}, 'Hatchling no longer finds this skill.');
    const facts = h('dl', { class: 'skill-facts' }, h('dt', {}, 'Version'), h('dd', {}, revealedValue(skill.version)), h('dt', {}, 'Signed by'), h('dd', { title: skill.signer }, h('code', {}, short(skill.signer))), h('dt', {}, 'Digest'), h('dd', { title: skill.digest }, h('code', {}, short(skill.digest))), h('dt', {}, 'Status'), h('dd', {}, STATES[skill.state]));
    const can = h('ul', { class: 'skill-grants' }, ...grants(skill).map(line => h('li', {}, line)));
    const files = h('ul', { class: 'skill-files' }, ...skill.files.map(file => h('li', {}, h('code', {}, revealedValue(file.path)), ' · ', size(file.bytes), file.path === skill.instructions ? ' · its playbook' : file.path === skill.program ? ' · its program' : '')));
    const root = h('div', { class: 'settings-stack skill-detail' }, h('div', { class: 'settings-group' }, h('p', {}, revealedValue(skill.description)), facts), h('section', {}, h('h3', { class: 'settings-section-title' }, 'Enabling it lets it'), h('div', { class: 'settings-group' }, can)), h('section', {}, h('h3', { class: 'settings-section-title' }, 'What it holds'), h('div', { class: 'settings-group' }, files)));
    if (skill.state === 'changed')
        root.prepend(h('div', { class: 'notice' }, icon('warning', 18), 'This skill changed since you enabled it, so it doesn’t run. Read it again, and enable it as it is now if you still want it.'));
    if (skill.state === 'enabled')
        root.append(button('Disable', () => ctx.dialogs.confirm(`Disable ${skill.name}?`, 'The agent can no longer use it, in any task, from now on.', () => { void ctx.run({ type: 'skill.disable', name: skill.name }, true).then(nav.refresh); }, { label: 'Disable skill', danger: true }), { variant: 'danger' }));
    else
        root.append(button('Enable', () => ctx.dialogs.confirm(`Enable ${skill.name}?`, `Enabling it lets it: ${grants(skill).join('; ')}. You enable it as shown here (digest ${short(skill.digest)}): if it changes, it stops until you enable it again.`, () => { void ctx.run({ type: 'skill.enable', name: skill.name, digest: skill.digest }, true).then(nav.refresh); }, { label: 'Enable skill' }), { variant: 'primary' }));
    return root;
}
