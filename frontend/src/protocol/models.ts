/** Product-domain contract. No DOM, storage, transport URLs or implementation dependencies. */
export type DeepReadonly<T> = T extends (...args: never[]) => unknown ? T : T extends readonly (infer U)[] ? readonly DeepReadonly<U>[] : T extends object ? {
    readonly [K in keyof T]: DeepReadonly<T[K]>;
} : T;
export type Id = string;
export type Verdict = 'success' | 'failed' | 'interrupted' | 'partial';
export type TurnState = 'idle' | 'working' | 'awaiting' | 'interrupted' | 'stalled';
export type Theme = 'system' | 'dark' | 'light';
export type Accent = 'white' | 'avatar' | 'blue' | 'azure' | 'violet' | 'pink' | 'orange' | 'green' | 'stone';
export type IconName = 'smile' | 'reply' | 'hand' | 'mail' | 'transfer' | 'eye' | 'bug' | 'chat' | 'search' | 'feed' | 'idea' | 'goal' | 'library' | 'browser' | 'memory' | 'clock' | 'bell' | 'settings' | 'panel' | 'plus' | 'close' | 'chevron' | 'arrow' | 'more' | 'check' | 'shield' | 'lock' | 'link' | 'wallet' | 'device' | 'file' | 'folder' | 'image' | 'video' | 'audio' | 'download' | 'edit' | 'trash' | 'pin' | 'share' | 'grid' | 'list' | 'mic' | 'phone' | 'stop' | 'heart' | 'refresh' | 'logout' | 'gift' | 'help' | 'sun' | 'moon' | 'globe' | 'run' | 'warning' | 'code' | 'spark' | 'briefcase' | 'leaf' | 'people' | 'book' | 'cart' | 'copy' | 'expand' | 'filter' | 'play' | 'pause' | 'volume' | 'paperclip' | 'keyboard' | 'fingerprint' | 'menu' | 'calendar' | 'connected' | 'currency' | 'palette' | 'laptop' | 'clipboard' | 'microscope' | 'compass' | 'shieldLock' | 'key';
export type ActionIcon = 'file' | 'globe' | 'run' | 'check' | 'warning' | 'memory' | 'clock';
export interface ActivityRow {
    id: Id;
    workId: Id;
    laneId: Id;
    title: string;
    subtitle: string;
    icon: ActionIcon;
    report: string;
    startedAt?: string | null;
    endedAt?: string | null;
    durationMs?: number | null;
    artifactIds?: Id[];
    outputFailed: boolean;
    status: Verdict;
    phase: 'action' | 'dispatch' | 'start' | 'finish';
}
export interface ActivityLane {
    id: Id;
    name: string;
    parentId: Id | null;
    rows: ActivityRow[];
}
/** Pending work has no verdict. It becomes an ActivityRow only when output arrives. */
export interface RunningActivityStep {
    id: Id;
    workId: Id;
    laneId: Id;
    title: string;
    subtitle: string;
    report: string;
    startedAt: string | null;
}
export interface ActivityThread {
    pendingSteps?: RunningActivityStep[];
    id: Id;
    chatId?: Id;
    startedAt?: string | null;
    endedAt?: string | null;
    summary?: string;
    evidenceArtifactIds?: Id[];
    name: string;
    subtitle: string;
    statusTitle: string;
    state: 'running' | 'completed' | 'interrupted' | 'failed';
    verdict: Verdict | null;
    finishMessage: string;
    time: string;
    lanes: ActivityLane[];
    evidenceArtifactId: Id | null;
}
export interface Chat {
    updatedAt?: string;
    id: Id;
    title: string;
    channelId: Id | null;
    pinned: boolean;
    archived: boolean;
    unread: boolean;
}
interface MessageBase {
    id: Id;
    chatId: Id;
    time: string;
    source: string | null;
    reaction: string | null;
}
export type Message = MessageBase & ({
    kind: 'browser';
    role: 'agent';
    taskId: Id;
} | {
    kind: 'text';
    role: 'user' | 'agent';
    text: string;
} | {
    kind: 'progress' | 'scheduled' | 'device' | 'error' | 'notice';
    role: 'agent';
    text: string;
    evidenceId: Id | null;
} | {
    kind: 'choice';
    role: 'agent';
    text: string;
    options: string[];
    selected: string | null;
} | {
    kind: 'purchase';
    role: 'agent';
    purchaseId: Id;
} | {
    kind: 'secure';
    role: 'agent';
    service: string;
    completed: boolean;
} | {
    kind: 'attachment';
    role: 'agent' | 'user';
    text: string;
    artifactId: Id;
} | {
    kind: 'code-request';
    role: 'agent';
    text: string;
    completed: boolean;
});
export type ArtifactKind = 'document' | 'page' | 'app' | 'media';
export type ArtifactFormat = 'archive' | 'markdown' | 'pdf' | 'word' | 'spreadsheet' | 'csv' | 'slides' | 'text' | 'code' | 'html' | 'app' | 'image' | 'video' | 'audio';
export interface SlideLayer {
    id: Id;
    text: string;
    x: number;
    y: number;
    width: number;
    size: number;
}
export interface Artifact {
    id: Id;
    title: string;
    kind: ArtifactKind;
    format: ArtifactFormat;
    description: string;
    content: string;
    asset: string | null;
    created: string;
    opened: string;
    pinned: boolean;
    trashUntil: string | null;
    version: number;
    publishedVersion: number | null;
    slides: SlideLayer[][];
}
export interface Publication {
    id: Id;
    artifactId: Id;
    title: string;
    content: string;
    version: number;
}
export type GoalCategory = 'Health & fitness' | 'Relationships' | 'Money' | 'Career' | 'Interests' | 'Productivity' | 'Something else';
export type PulseKind = 'waiting' | 'blocked' | 'monitoring' | 'scheduled' | 'countdown' | 'milestone' | 'rhythm';
export interface Goal {
    sampleOnly?: boolean;
    id: Id;
    kind: 'user' | 'tracking';
    title: string;
    description: string;
    category: GoalCategory;
    status: 'open' | 'closed';
    pulse: string;
    pulseKind: PulseKind;
    attention: boolean;
    due: string | null;
    timeline: {
        date: string;
        text: string;
    }[];
}
export type IdeaDomain = 'Career' | 'Devices' | 'Creation & media' | 'Family' | 'Finance' | 'Health & fitness' | 'Learning & hobbies' | 'Productivity' | 'Relationships' | 'Shopping';
export interface Idea {
    sampleOnly?: boolean;
    section?: string;
    id: Id;
    title: string;
    summary: string;
    domain: IdeaDomain;
    icon: IconName;
    rationale: string;
    signal: string;
    items: {
        title: string;
        summary: string;
    }[];
    state: 'suggested' | 'activated' | 'executing' | 'completed' | 'partial' | 'needs_user' | 'failed' | 'dismissed';
    outcome: string;
    artifactId: Id | null;
}
export interface FeedPost {
    reference?: boolean;
    media?: {
        src: string;
        alt: string;
        width: number;
        height: number;
    };
    highlights?: string[];
    id: Id;
    title: string;
    body: string;
    kicker: string;
    category: string;
    icon: IconName;
    reason: string;
    date: string;
    reacted: boolean;
}
export type ChannelState = 'linked' | 'unlinked' | 'link_pending' | 'checking' | 'unavailable';
export interface Channel {
    id: Id;
    name: string;
    state: ChannelState;
    polling: boolean;
    interval: string;
    greetingSent: boolean;
    chatId: Id;
    lastEvent: string;
}
export interface Connector {
    custom?: boolean;
    sampleOnly?: boolean;
    id: Id;
    name: string;
    icon: IconName;
    state: 'connected' | 'available' | 'pending' | 'reauth_required';
    scope: string;
}
export interface PaymentMethod {
    id: Id;
    brand: string;
    last4: string;
    provider: string;
    fee: number;
}
export interface WalletProvider {
    sampleOnly?: boolean;
    comingSoon?: boolean;
    id: Id;
    name: string;
    state: 'connected' | 'not_connected' | 'reauth_required';
    methods: PaymentMethod[];
}
export type PaymentState = 'proposed' | 'approved' | 'submitted' | 'confirmed' | 'denied' | 'merchant-declined' | 'unknown';
export interface Purchase {
    id: Id;
    taskId: Id;
    merchant: string;
    items: {
        name: string;
        variant: string;
        quantity: number;
        price: number;
    }[];
    address: string;
    contact: string;
    shipping: string;
    estimate: string;
    subtotal: number;
    tax: number;
    shippingCost: number;
    fees: number;
    total: number;
    methodId: Id;
    addOns: string;
    cancellation: string;
    state: PaymentState;
    reviewState: 'draft' | 'changed' | 'approved' | 'declined';
    revision: number;
    approvedTotal: number | null;
    approvedRevision: number | null;
    ordersSubmitted: number;
    outcome: 'confirmed' | 'unknown' | 'merchant-declined';
}
export interface BrowserFrame {
    sequence: number;
    page: 'home' | 'search' | 'challenge' | 'results';
    title: string;
    query: string;
    capturedAt: string;
    image: string | null;
}
export type BrowserTaskState = 'queued' | 'assigned' | 'running' | 'parked' | 'completed' | 'failed' | 'cancelled';
export interface BrowserTask {
    chatId?: Id;
    frame?: BrowserFrame;
    id: Id;
    title: string;
    site: string;
    step: string;
    statusTitle: string;
    state: BrowserTaskState;
    controller: 'agent' | 'user' | 'switching';
    attemptsRemaining: number;
    constraints: string[];
    granted: string[];
    credentialReference: string | null;
    challenge: 'none' | 'waiting' | 'persistent' | 'solved';
    continuedFrom: Id | null;
    blocker: string;
    lastConfirmedState: string;
}
export interface Device {
    id: Id;
    name: string;
    type: 'phone' | 'computer';
    reachable: boolean;
    capabilities: string[];
    lastSeen: string;
    origin: boolean;
}
export interface Permission {
    id: Id;
    name: string;
    mode: 'ask-for-some' | 'always-ask';
    count: number;
    description: string;
}
export interface MemoryEntry {
    id: Id;
    kind: 'profile' | 'people' | 'memory' | 'preferences' | 'logs';
    title: string;
    body: string;
    date: string;
    closeness: number;
}
export interface ScheduledJob {
    id: Id;
    title: string;
    kind: 'time' | 'hook';
    schedule: string;
    recurring: boolean;
    limit: string;
    source: string;
    event: string;
    goalId: Id | null;
    health: 'ok' | 'errored' | 'disabled';
    lastRun: string;
    nextRun: string;
}
export interface Notification {
    id: Id;
    title: string;
    text: string;
    time: string;
    read: boolean;
    urgent: boolean;
    evidenceId: Id | null;
    offer: string | null;
    stage: 'candidate' | 'delivered';
}
/** Immutable evidence shown in the context panel; never a new permission grant. */
export interface ApprovalRecord {
    id: Id;
    title: string;
    description: string;
    outcome: 'timed-out' | 'task' | 'always' | 'denied';
    time: string;
    report: string;
}
export type InterfaceSize = 'compact' | 'comfortable' | 'large';
export interface Settings {
    interfaceSize: InterfaceSize;
    avatar?: string;
    theme: Theme;
    accent: Accent;
    avatarAccent: string;
    keepChatPanelVisible: boolean;
    language: string;
    name: string;
    vibe: string;
    feedPrompt: string;
    captchaStanding: boolean;
    telemetry: boolean;
    quietStart: string;
    quietEnd: string;
    digestTime: string;
    dailyAllowance: number;
    allowlist: string[];
}
/** An adapter-supplied, read-only index. Never a path into the host filesystem. */
export interface SystemEntry {
    reference?: {
        type: string;
        modified: string;
        size: string;
    };
    id: Id;
    parentId: Id | null;
    name: string;
    kind: 'folder' | 'document' | 'note' | 'index';
    visibility: 'user' | 'internal';
    modified: string | null;
    bytes: number | null;
    artifactId: Id | null;
    memoryId: Id | null;
    description: string;
}
export type WebsitePolicy = 'allow' | 'ask' | 'block';
export interface WebsitePermission {
    host: string;
    policy: WebsitePolicy;
}
export interface NetworkPermission {
    id: string;
    name: string;
    description: string;
    icon: IconName;
    enabled: boolean;
}
/** A site the user signed in to and kept signed in for the agent: its browser opens signed in. */
export interface SignedInSite {
    id: Id;
    /** The sites it holds, the one signed in on first. */
    sites: string[];
    /** Whether a session is kept for it; signed out, its sites are kept but open signed out. */
    signedIn: boolean;
    lastUsed: string;
}
export interface AppState {
    websitePermissions: WebsitePermission[];
    signedInSites: SignedInSite[];
    networkPermissions: NetworkPermission[];
    version: 1;
    mode: 'scripted' | 'live';
    revision: number;
    turn: TurnState;
    clock: string;
    chats: Chat[];
    messages: Message[];
    threads: ActivityThread[];
    artifacts: Artifact[];
    publications: Publication[];
    systemEntries: SystemEntry[];
    goals: Goal[];
    ideas: Idea[];
    posts: FeedPost[];
    connectors: Connector[];
    channels: Channel[];
    providers: WalletProvider[];
    purchases: Purchase[];
    browserTasks: BrowserTask[];
    devices: Device[];
    permissions: Permission[];
    approvalHistory: ApprovalRecord[];
    memory: MemoryEntry[];
    jobs: ScheduledJob[];
    notifications: Notification[];
    settings: Settings;
    spendingLimit: number;
    spentToday: number;
    secureItems: string[];
}
export type Scenario = 'browser-watch' | 'bundle-files' | 'research' | 'purchase' | 'price-change' | 'unknown-payment' | 'captcha' | 'persistent-captcha' | 'fix-ci' | 'channels' | 'secure' | 'choices' | 'mixed-stream' | 'interrupted' | 'stalled';
/** The keys the user may press on a page they drive: the ones a form needs. */
export type BrowserKey = 'enter' | 'tab' | 'escape' | 'backspace' | 'arrow_up' | 'arrow_down' | 'arrow_left' | 'arrow_right';
export type Command = {
    type: 'settings.interface-size';
    value: InterfaceSize;
} | {
    type: 'browser.advance';
    id: Id;
    sequence: number;
} | {
    type: 'browser.input';
    id: Id;
    text: string;
} | {
    type: 'browser.click';
    id: Id;
    x: number;
    y: number;
} | {
    type: 'browser.key';
    id: Id;
    key: BrowserKey;
} | {
    type: 'browser.scroll';
    id: Id;
    down: boolean;
} | {
    type: 'browser.keep-signed-in';
    id: Id;
} | {
    type: 'profile.sign-out';
    id: Id;
} | {
    type: 'profile.delete';
    id: Id;
} | {
    type: 'identity.avatar';
    value: string;
    accent?: string;
} | {
    type: 'permission.website';
    host: string;
    policy: WebsitePolicy;
} | {
    type: 'permission.network';
    id: string;
    enabled: boolean;
} | {
    type: 'support.download';
    title: string;
    body: string;
} | {
    type: 'settings.accent';
    value: Accent;
} | {
    type: 'settings.chat-panel';
    value: boolean;
} | {
    type: 'chat.read';
    id: Id;
} | {
    type: 'identity.update';
    name: string;
    vibe: string;
} | {
    type: 'chat.send';
    chatId: Id;
    text: string;
} | {
    type: 'chat.create';
    title: string;
} | {
    type: 'chat.attach';
    chatId: Id;
    artifactId: Id;
    avatarAccent?: string;
} | {
    type: 'artifact.import';
    title: string;
    format: ArtifactFormat;
    content: string;
    dataUrl: string;
} | {
    type: 'chat.rename';
    id: Id;
    title: string;
} | {
    type: 'chat.toggle-pin' | 'chat.archive' | 'chat.delete';
    id: Id;
} | {
    type: 'message.react';
    id: Id;
    reaction: string;
} | {
    type: 'message.choose';
    id: Id;
    option: string;
} | {
    type: 'message.secure-complete' | 'message.code-complete';
    id: Id;
} | {
    type: 'turn.interrupt' | 'turn.resume';
} | {
    type: 'demo.scenario';
    scenario: Scenario;
} | {
    type: 'demo.advance';
    step: number;
} | {
    type: 'demo.reset';
} | {
    type: 'artifact.create';
    title: string;
    format: ArtifactFormat;
} | {
    type: 'artifact.open' | 'artifact.pin' | 'artifact.trash' | 'artifact.restore' | 'artifact.download';
    id: Id;
} | {
    type: 'artifact.app-toggle';
    id: Id;
    index: number;
} | {
    type: 'artifact.save';
    id: Id;
    content: string;
} | {
    type: 'artifact.save-slide';
    id: Id;
    slide: number;
    layerId: Id;
    text: string;
    x: number;
    y: number;
    width: number;
    size: number;
} | {
    type: 'artifact.publish';
    id: Id;
    approvedVersion: number;
} | {
    type: 'goal.create';
    title: string;
    description: string;
    category: GoalCategory;
    due: string;
} | {
    type: 'goal.update';
    id: Id;
    title: string;
    description: string;
    due: string;
} | {
    type: 'goal.close' | 'goal.reopen';
    id: Id;
} | {
    type: 'idea.activate' | 'idea.dismiss' | 'idea.restore';
    id: Id;
} | {
    type: 'idea.edit';
    id: Id;
    summary: string;
} | {
    type: 'feed.prompt';
    text: string;
} | {
    type: 'feed.generate';
} | {
    type: 'feed.react';
    id: Id;
} | {
    type: 'connector.begin' | 'connector.finish' | 'connector.disconnect';
    id: Id;
} | {
    type: 'channel.begin' | 'channel.finish' | 'channel.check' | 'channel.disconnect' | 'channel.restart';
    id: Id;
} | {
    type: 'purchase.approve';
    id: Id;
    revision: number;
    total: number;
} | {
    type: 'purchase.submit' | 'purchase.decline';
    id: Id;
} | {
    type: 'purchase.method';
    id: Id;
    methodId: Id;
} | {
    type: 'purchase.requote';
    id: Id;
    total: number;
} | {
    type: 'wallet.connect' | 'wallet.reauth' | 'wallet.add-method';
    id: Id;
} | {
    type: 'permission.set';
    id: Id;
    mode: 'ask-for-some' | 'always-ask';
} | {
    type: 'permission.reset';
} | {
    type: 'browser.takeover' | 'browser.resume' | 'browser.grant' | 'browser.solve' | 'browser.cancel' | 'browser.start';
    id: Id;
} | {
    type: 'device.pair';
    name: string;
} | {
    type: 'device.remove';
    id: Id;
} | {
    type: 'device.command';
    id: Id;
    capability: string;
} | {
    type: 'memory.save';
    expectedBody?: string;
    id: Id;
    title: string;
    body: string;
} | {
    type: 'memory.delete';
    id: Id;
} | {
    type: 'job.create';
    title: string;
    kind: 'time' | 'hook';
    schedule: string;
    recurring: boolean;
    source: string;
    event: string;
} | {
    type: 'job.update';
    id: Id;
    title: string;
    kind: 'time' | 'hook';
    schedule: string;
    recurring: boolean;
    source: string;
    event: string;
} | {
    type: 'job.toggle' | 'job.run' | 'job.delete';
    id: Id;
} | {
    type: 'notification.read' | 'notification.answer';
    id: Id;
} | {
    type: 'settings.theme';
    value: Theme;
} | {
    type: 'settings.name' | 'settings.language' | 'settings.allowlist';
    value: string;
} | {
    type: 'settings.telemetry' | 'settings.captcha';
    value: boolean;
} | {
    type: 'settings.digest';
    time: string;
    start: string;
    end: string;
    allowance: number;
} | {
    type: 'secure.add';
    service: string;
} | {
    type: 'secure.remove';
    service: string;
} | {
    type: 'data.export';
};
export interface DownloadPayload {
    filename: string;
    mime: string;
    text: string | null;
    asset: string | null;
}
export type CommandResult = {
    ok: true;
    message: string;
    id?: Id;
    download?: DownloadPayload;
} | {
    ok: false;
    message: string;
};
export interface ProtocolAdapter {
    readonly mode: 'scripted' | 'live';
    getSnapshot(): DeepReadonly<AppState>;
    subscribe(listener: () => void): () => void;
    execute(command: Command): Promise<CommandResult>;
    dispose(): void;
}
