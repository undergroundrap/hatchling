import type { AppState, NetworkPermission, Skill } from '../protocol/models.js';
/** Sample signed skills for the demo: made up here, signed by no real key. */
export function sampleSkills(): Skill[] {
    const hex = (seed: string): string => seed.repeat(64).slice(0, 64);
    return [
        { name: 'release-check', version: '1.2.0', description: 'Use before a release: runs the project checks and lists what still fails.', signer: hex('5e1'), digest: hex('c4a'), capabilities: ['fs.read'], instructions: 'SKILL.md', program: 'scripts/check.py', files: [{ path: 'SKILL.md', bytes: 2140 }, { path: 'scripts/check.py', bytes: 3812 }], state: 'changed' },
        { name: 'tidy-notes', version: '1.0.0', description: 'Use when asked to tidy, merge or retitle notes, keeping every line the owner wrote.', signer: hex('5e1'), digest: hex('a17'), capabilities: [], instructions: 'SKILL.md', program: null, files: [{ path: 'SKILL.md', bytes: 1630 }], state: 'enabled' },
        { name: 'weekly-review', version: '0.3.0', description: 'Use on Fridays, or when asked what changed this week: reads the week’s goals and notes and drafts a short review.', signer: hex('5e1'), digest: hex('9b2'), capabilities: ['memory.read', 'goals.query'], instructions: 'SKILL.md', program: 'review', files: [{ path: 'SKILL.md', bytes: 2875 }, { path: 'review', bytes: 412096 }], state: 'waiting' },
    ];
}
/** Synthetic settings examples, not claims of linked real accounts. */
export function defaultNetworkPermissions(): NetworkPermission[] {
    return [
        { id: 'ssh', name: 'Outbound SSH', description: 'Direct secure-shell connections to remote servers (port 22; SFTP uses this too)', icon: 'code', enabled: true },
        { id: 'smtp', name: 'Outgoing email (SMTP)', description: 'Direct mail-sending connections (ports 25, 465, 587); email connectors do not need this', icon: 'mail', enabled: true },
        { id: 'imap', name: 'Email mailbox access (IMAP, POP3)', description: 'Direct mailbox connections (ports 110, 143, 993, 995)', icon: 'mail', enabled: true },
        { id: 'database', name: 'Database connections', description: 'Direct connections to PostgreSQL, MySQL, MongoDB, Redis, and SQL Server', icon: 'file', enabled: true },
        { id: 'ftp', name: 'File transfer (FTP)', description: 'Classic file-transfer connections (ports 21, 990)', icon: 'transfer', enabled: true },
        { id: 'dns', name: 'External DNS lookups', description: 'Direct queries to outside DNS resolvers, plus mail and service record lookups (TXT, MX, SRV and similar); these run without individual approval prompts while enabled', icon: 'globe', enabled: true },
        { id: 'tcp', name: 'Other TCP connections', description: 'Any other direct TCP connection on ports without a dedicated setting', icon: 'globe', enabled: false },
        { id: 'udp', name: 'Other UDP traffic', description: 'Any other direct UDP traffic, like QUIC or custom protocols', icon: 'globe', enabled: false },
    ];
}
export function applySettingsFixtures(state: AppState): AppState {
    state.connectors = [
        { id: 'browser', name: 'Browser', icon: 'globe', state: 'connected', scope: 'Approved sites only. Purchases and sensitive changes always ask.' },
        { id: 'github', name: 'Github', icon: 'grid', state: 'connected', custom: true, scope: 'Read repositories. Each write requires approval.' },
        { id: 'instagram', name: 'Instagram', icon: 'image', state: 'connected', scope: 'Read selected account data. Sharing requires approval.' },
        { id: 'calendly', name: 'Calendly', icon: 'calendar', state: 'available', scope: 'Read availability. Booking and changes require approval.' },
        { id: 'facebook', name: 'Facebook', icon: 'people', state: 'available', scope: 'Read selected content. Every post requires approval.' },
        { id: 'plaid', name: 'Finances (Plaid)', icon: 'wallet', state: 'available', scope: 'Selected financial data, read only.' },
        { id: 'function', name: 'Function Health', icon: 'heart', state: 'available', scope: 'Read selected records with explicit approval.' },
        { id: 'gmail', name: 'Gmail', icon: 'mail', state: 'available', scope: 'Read and search. Sending always requires approval.' },
        { id: 'calendar', name: 'Google Calendar', icon: 'calendar', state: 'available', scope: 'Read events. Changes require approval.' },
        { id: 'contacts', name: 'Google Contacts', icon: 'people', state: 'available', scope: 'Read selected contact details.' },
        { id: 'messenger-companion', name: 'Messenger companion', icon: 'chat', state: 'available', scope: 'Read and search. Every write requires approval.' },
    ];
    for (const provider of state.providers)
        provider.sampleOnly = true;
    state.providers.unshift({ id: 'link', name: 'Link by Stripe', state: 'not_connected', methods: [] }, { id: 'shop-reference', name: 'Shop Pay', state: 'not_connected', methods: [], comingSoon: true });
    state.secureItems = [];
    state.websitePermissions = ['files.example', 'reports.example', 'checks.example', 'archive.example', 'api.example', 'projects.example'].map(host => ({ host, policy: 'allow' }));
    state.networkPermissions = defaultNetworkPermissions();
    for (const item of state.permissions) {
        if (item.id === 'connectors')
            item.count = 3;
        if (item.id === 'websites')
            item.count = 6;
    }
    return state;
}
