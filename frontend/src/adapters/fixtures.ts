import { applySettingsFixtures } from './settings-fixtures.js';
import { applyActivityFixtures } from './activity-fixtures.js';
import type { ActivityRow, AppState, Artifact, Idea, IdeaDomain, IconName, Message } from '../protocol/models.js';
import { applySurfaceFixtures } from './surface-fixtures.js';
import { applyWorkspaceFixtures } from './workspace-fixtures.js';
import { normalizeRow } from '../protocol/rules.js';
export const DEMO_TIME = '2026-09-23T09:41:00';
export function row(id: string, title: string, subtitle: string, icon: ActivityRow['icon'] = 'check', laneId = 'main', phase: ActivityRow['phase'] = 'action', outputFailed = false): ActivityRow {
    return normalizeRow({ id, workId: id, laneId, title, subtitle, icon, phase, outputFailed, status: 'success', report: `# ${title}\n\n${subtitle}.\n\nThis is a deterministic demonstration record, not a live action.\n\nEvidence\n• Fixture source: the bundled sample workspace.\n• Scope: only the stated task; no external accounts accessed.\n• Outcome: ${outputFailed ? 'The attempted step did not produce a result.' : 'The stated result was verified against the scripted state.'}\n\nThe live adapter may attach commands, diffs, page snapshots and diagnostic details here. Those details stay out of chat and activity titles.` });
}
export function textMessage(id: string, text: string, role: 'agent' | 'user' = 'agent', chatId = 'main'): Message {
    return { id, chatId, time: '9:41 am', source: null, reaction: null, kind: 'text', role, text };
}
export function artifact(id: string, title: string, format: Artifact['format'], content: string, description: string): Artifact {
    return { id, title, kind: format === 'html' ? 'page' : format === 'app' ? 'app' : ['image', 'video', 'audio'].includes(format) ? 'media' : 'document', format, description, content, asset: null, created: DEMO_TIME, opened: DEMO_TIME, pinned: false, trashUntil: null, version: 1, publishedVersion: null, slides: [] };
}
const brief = '# A slower weekend by the coast\n\nA small, practical plan for two days away. This is a fictional example, not your travel itinerary.\n\n## Saturday · settle in\n\nLeave the morning open. Take the coast road, stop at the old harbor, and have lunch at the market. Check in after 3 pm.\n\n## Sunday · take the long way\n\nStart with a coffee and a walk along the headland. Visit the small bookshop before heading home.\n\n## Keep it simple\n\n- Pack a light layer and comfortable shoes.\n- Keep one afternoon unplanned.\n- Check opening hours before leaving.\n\n## Budget\n\nStay: $240 · Food: $100 · Travel: $60\n\nNothing is booked. All amounts are sample estimates.\n';
const backlog = '# Hatchling improvement backlog\n\nPrioritized, research-only · sample document\n\n## Verdict first\n\nKeep the frontend boundary small and testable. Every visible result should point to the evidence that produced it.\n\n## Agreed sequencing\n\n1. Define the protocol contract and deterministic fixtures.\n2. Recreate the navigation, chat and context panel.\n3. Test confirmation rails before adding live capabilities.\n\n## Definition of done\n\nStrict types pass. The activity history is inspectable. Destructive actions ask first. No demo screen claims a real purchase or connection.\n';
const domains: {
    domain: IdeaDomain;
    icon: IconName;
    title: string;
    summary: string;
    signal: string;
    item: string;
}[] = [
    { domain: 'Career', icon: 'briefcase', title: 'I can turn this week’s work into a clean handoff', summary: 'A concise brief with decisions, open questions, and the next useful step — so Monday starts with context, not archaeology.', signal: 'Sample fact: you keep a weekly work journal.', item: 'Weekly handoff brief' },
    { domain: 'Creation & media', icon: 'edit', title: 'I can give your next short video a stronger opening', summary: 'Use your own footage to make a clear, finished vertical edit, with captions and a review before publishing.', signal: 'Sample fact: you recorded a talking-head clip.', item: 'Opening and caption plan' },
    { domain: 'Productivity', icon: 'goal', title: 'I can pin down what “finished” actually means', summary: 'Turn a sprawling project into a one-page acceptance checklist with evidence beside each outcome.', signal: 'Sample fact: you have an active side project.', item: 'Acceptance checklist' },
    { domain: 'Devices', icon: 'device', title: 'I can make your devices easier to pick up and use', summary: 'A small audit of the capabilities each paired device offers, with an explicit target for every action.', signal: 'Sample fact: two devices are paired in this demo.', item: 'Device capability map' },
    { domain: 'Health & fitness', icon: 'moon', title: 'I can make room for a calmer evening', summary: 'A simple wind-down plan built around the time you actually have, with reminders only after you approve a schedule.', signal: 'Sample fact: you prefer low-pressure routines.', item: 'Evening rhythm plan' },
    { domain: 'Learning & hobbies', icon: 'book', title: 'I can build a reading trail around your curiosity', summary: 'A short, purposeful reading list and a place to capture the questions that are worth following.', signal: 'Sample fact: you enjoy learning through books.', item: 'Reading trail' },
    { domain: 'Relationships', icon: 'heart', title: 'I can help you keep a small promise', summary: 'Put together a thoughtful catch-up plan without sending a message or making a commitment on your behalf.', signal: 'Sample fact: you noted a friend you want to catch up with.', item: 'Catch-up note' },
    { domain: 'Family', icon: 'people', title: 'I can put the family weekend in one place', summary: 'A readable plan for shared activities, with room for rest and a clear list of things still to decide.', signal: 'Sample fact: your sample calendar includes a family afternoon.', item: 'Weekend plan' },
    { domain: 'Finance', icon: 'wallet', title: 'I can make recurring costs easier to review', summary: 'A neutral inventory of sample subscriptions and renewal dates. Nothing gets cancelled without your confirmation.', signal: 'Sample fact: you want a clearer subscription inventory.', item: 'Renewal inventory' },
    { domain: 'Shopping', icon: 'cart', title: 'I can compare the details before you buy', summary: 'A short comparison of variants, delivery, and the full price, followed by a review card before any checkout.', signal: 'Sample fact: you saved a travel-bag idea.', item: 'Purchase comparison' },
];
export function createFixtures(): AppState {
    const a1 = artifact('weekend', 'A slower weekend by the coast.md', 'markdown', brief, 'A two-day plan with room to wander.');
    const a2 = artifact('backlog', 'Hatchling improvement backlog.md', 'markdown', backlog, 'Prioritized, research-only.');
    const a3 = artifact('roadmap', 'Hatchling roadmap.pdf', 'pdf', '# Hatchling\nRoadmap ledger\n\nA reference-led frontend, built around explicit trust.\n\nRecorded baseline\nThe scripted adapter owns all product state.\nThe interface does not know how data is stored.\n\nDefinition of done\nA result is only complete when the evidence exists.\n\nNext milestones\n01 — Core workspace\n02 — Confirmation rails\n03 — Live adapter integration', 'A compact project ledger.');
    a3.asset = './media/sample-roadmap.pdf';
    const deck = artifact('deck', 'A weekend, well spent', 'slides', 'An editable three-slide sample deck.', 'A small trip in three slides.');
    deck.slides = [
        [{ id: 's1a', text: 'A weekend,\nwell spent.', x: 8, y: 18, width: 80, size: 48 }, { id: 's1b', text: 'LESS PLANNING. MORE ROOM TO WANDER.', x: 8, y: 76, width: 85, size: 14 }],
        [{ id: 's2a', text: 'Saturday, slowly.', x: 8, y: 20, width: 84, size: 38 }, { id: 's2b', text: 'Harbor walk · Market lunch · An open afternoon', x: 8, y: 60, width: 84, size: 20 }],
        [{ id: 's3a', text: 'Take the long way home.', x: 8, y: 20, width: 84, size: 38 }, { id: 's3b', text: 'One coffee. One good book. No rush.', x: 8, y: 65, width: 84, size: 20 }],
    ];
    const page = artifact('coast-page', 'A little guide to the coast', 'html', brief, 'A finished, read-only sample guide.');
    const app = artifact('packing-app', 'The little packing list', 'app', 'Notebook\nLight jacket\nWalking shoes\nCamera\nWater bottle', 'An interactive, local-only packing list. Never publishable.');
    const photo = artifact('portrait', 'Hatchling mascot', 'image', '', 'The original Hatchling mascot supplied for this app.');
    photo.asset = './media/hatchling-mascot.png';
    const video = artifact('video', 'Hatchling · a little hello', 'video', 'A four-second silent preview made from the supplied Hatchling mascot. The original artwork is retained; no substitute character or live footage is used.', 'The real Hatchling mascot in a short, silent motion preview.');
    video.asset = './media/sample-video.webm';
    const audio = artifact('audio', 'A small pause', 'audio', 'An original, four-second soft audio cue for this demonstration.', 'A short sample audio artifact.');
    audio.asset = './media/sample-audio.wav';
    const state: AppState = {
        websitePermissions: [], signedInSites: [], networkPermissions: [],
        version: 1, mode: 'scripted', revision: 0, turn: 'idle', clock: DEMO_TIME,
        chats: [
            { id: 'main', title: 'Main chat', channelId: null, pinned: true, archived: false, unread: false },
            ...['Plan a slower weekend', 'Check the project before shipping', 'A reading list for autumn', 'Make sense of these notes', 'Find a bag for short trips', 'An evening with a little less noise', 'The next small step', 'A better weekly handoff', 'Remember the important bits'].map((title, i) => ({ id: `chat-${i}`, title, channelId: null, pinned: i === 0, archived: false, unread: false })),
            { id: 'chat-whatsapp', title: 'WhatsApp', channelId: 'whatsapp', pinned: false, archived: false, unread: true },
            { id: 'chat-messenger', title: 'Messenger', channelId: 'messenger', pinned: false, archived: false, unread: false },
        ],
        messages: [
            textMessage('m1', 'I’ve kept the plan deliberately light: one good walk, somewhere unhurried for lunch, and an afternoon with nothing booked.\n\nThe practical bit is ready too — a small packing list and a budget you can actually scan.'),
            textMessage('m2', 'that sounds like exactly what I need. can you keep Sunday mostly open?', 'user'),
            textMessage('m3', 'Absolutely. Sunday is just coffee, a bookshop if you feel like it, and the long way home. Nothing timed, nothing you need to rush for.'),
            textMessage('m4', 'also, before we book anything, I’d like to see the full price and the cancellation terms. no surprises.', 'user'),
            textMessage('m5', 'Of course. I’ll bring you the exact total, the delivery or booking details, and the terms before anything is submitted.\n\nYou stay in control — a price change means a new review.'),
            { id: 'm6', chatId: 'main', time: '9:41 am', source: null, reaction: null, kind: 'attachment', role: 'agent', text: 'Here’s the plan so far, ready to keep or change.', artifactId: 'weekend' },
            textMessage('m7', 'looks good. what’s next?', 'user'),
            textMessage('m8', 'We can leave it there, or make one part a little more concrete.'),
            textMessage('w1', 'Your weekend plan is ready. Nothing has been booked.', 'agent', 'chat-whatsapp'),
            { ...textMessage('w2', 'Thanks — I’ll look tonight.', 'user', 'chat-whatsapp'), source: 'WhatsApp' },
            { ...textMessage('me1', 'I can bring a purchase review here for you to approve from Messenger. Replies stay in this conversation.', 'agent', 'chat-messenger'), source: 'Messenger' },
            ...['chat-0', 'chat-1', 'chat-2', 'chat-3', 'chat-4', 'chat-5', 'chat-6', 'chat-7', 'chat-8'].map((chatId, i) => textMessage(`side-${i}`, 'This is a dedicated side chat with its own saved transcript. What would you like to work on here?', 'agent', chatId)),
        ],
        threads: [
            { id: 't-weekend', name: 'Plan a slower weekend', subtitle: 'Saved a flexible two day itinerary', statusTitle: 'Saved plan', state: 'completed', verdict: 'success', finishMessage: 'The weekend plan is ready, with an open Sunday and a clear sample budget. Nothing has been booked.', time: '9:38 am', evidenceArtifactId: 'weekend', lanes: [{ id: 'main', name: 'Main agent', parentId: null, rows: [row('r1', 'Checked the shape of the weekend', 'Kept Sunday free of fixed appointments'), row('r2', 'Saved the weekend plan', 'Included a packing list and sample budget', 'file')] }] },
            { id: 't-handoff', name: 'Prepare a project handoff', subtitle: 'Recorded decisions and the next milestone', statusTitle: 'Completed brief', state: 'completed', verdict: 'success', finishMessage: 'The brief is ready for review. It separates completed work from open questions.', time: '9:24 am', evidenceArtifactId: 'backlog', lanes: [{ id: 'main', name: 'Main agent', parentId: null, rows: [row('r3', 'Reviewed the sample project notes', 'Separated decisions from open questions'), row('r4', 'Saved a compact handoff', 'Included the next verifiable milestone', 'file')] }] },
            { id: 't-memory', name: 'Keep the useful context', subtitle: 'Recorded the preference for unhurried plans', statusTitle: 'Updated context', state: 'completed', verdict: 'success', finishMessage: 'The sample preference is saved. No sensitive values were added.', time: '9:12 am', evidenceArtifactId: null, lanes: [{ id: 'main', name: 'Main agent', parentId: null, rows: [row('r5', 'Remembered a planning preference', 'Keep one afternoon open on short trips', 'memory')] }] },
            { id: 't-digest', name: 'Prepare the morning edition', subtitle: 'Delivered only the useful new updates', statusTitle: 'Delivered edition', state: 'completed', verdict: 'success', finishMessage: 'Your sample morning edition is ready. Routine checks stayed quiet.', time: '8:30 am', evidenceArtifactId: null, lanes: [{ id: 'main', name: 'Main agent', parentId: null, rows: [row('r6', 'Prepared a focused daily edition', 'Grouped two verified sample updates', 'clock')] }] },
        ],
        artifacts: [a2, a3, a1, deck, page, app, artifact('budget', 'Weekend budget.csv', 'csv', 'Item,Estimated cost,Notes\nStay,240,Two sample nights\nFood,100,Flexible\nTravel,60,Sample estimate\nTotal,400,Nothing booked', 'A simple, downloadable estimate.'), photo, video, audio], publications: [], systemEntries: [],
        goals: [
            { id: 'g1', kind: 'user', title: 'Make space for a slower weekend', description: 'Get outside and return with a little more energy. Keep the plan light.', category: 'Interests', status: 'open', pulse: 'The first itinerary is ready; Sunday is deliberately open.', pulseKind: 'milestone', attention: false, due: null, timeline: [{ date: 'Sep 23', text: 'A flexible sample itinerary was saved.' }, { date: 'Sep 22', text: 'Started with a preference for an unhurried plan.' }] },
            { id: 'g2', kind: 'user', title: 'Ship the next useful version', description: 'Keep the scope small and make every result easy to verify.', category: 'Career', status: 'open', pulse: 'Waiting for your choice of the next acceptance milestone.', pulseKind: 'waiting', attention: true, due: null, timeline: [{ date: 'Sep 23', text: 'The handoff brief is ready for review.' }] },
            { id: 'g3', kind: 'tracking', title: 'Watch the travel bag price', description: 'Check the saved sample bag and interrupt only when the price changes.', category: 'Money', status: 'open', pulse: 'Monitoring the sample merchant for a price change.', pulseKind: 'monitoring', attention: false, due: null, timeline: [{ date: 'Sep 23', text: 'Last checked at $68.00; no purchase authorized.' }] },
            { id: 'g4', kind: 'tracking', title: 'Your weekly work recap', description: 'A quiet, recurring look at what changed.', category: 'Productivity', status: 'open', pulse: 'Last edition: Sep 18. Next edition: Sep 25 at 4 pm.', pulseKind: 'rhythm', attention: false, due: '2026-09-25', timeline: [{ date: 'Sep 18', text: 'The previous sample edition was delivered.' }] },
            { id: 'g5', kind: 'tracking', title: 'Keep the booking decision together', description: 'Hold the plan until a stay is selected.', category: 'Interests', status: 'open', pulse: 'Blocked by the choice of a place to stay.', pulseKind: 'blocked', attention: true, due: null, timeline: [{ date: 'Sep 23', text: 'No stay has been selected or booked.' }] },
            { id: 'g6', kind: 'tracking', title: 'Sample project review', description: 'A fictional dated commitment, used to demonstrate an evidence-backed countdown.', category: 'Career', status: 'open', pulse: 'Two days until the sample review on Sep 25.', pulseKind: 'countdown', attention: false, due: '2026-09-25', timeline: [{ date: 'Sep 23', text: 'Sample calendar reference: project review, Sep 25, 2026. The demo clock is Sep 23.' }] },
            { id: 'g7', kind: 'tracking', title: 'Prepare the next morning edition', description: 'An explicitly scheduled sample digest.', category: 'Productivity', status: 'open', pulse: 'Scheduled for Sep 24 at 8:30 am in the sample calendar.', pulseKind: 'scheduled', attention: false, due: '2026-09-24', timeline: [{ date: 'Sep 23', text: 'The sample digest schedule was confirmed for 8:30 am.' }] },
        ],
        ideas: domains.map((item, i): Idea => ({ id: `idea-${i}`, title: item.title, summary: item.summary, domain: item.domain, icon: item.icon, rationale: 'This is a small, bounded piece of work with a concrete result to keep. It does not authorize messages, purchases or publishing.', signal: item.signal, items: [{ title: item.item, summary: 'A finished, editable Markdown document with the key decisions and next steps.' }], state: 'suggested', outcome: '', artifactId: null })),
        posts: [
            { id: 'f1', title: 'A good handoff is a gift to your future self', body: 'The most useful project note is rarely the longest. It says what changed, which decisions are settled, and what evidence would make the next step complete.\n\nLeave a small trail: **one decision, one open question, one next action**. That is enough to start again without reconstructing the whole week.', kicker: 'A smaller way to work', category: 'Work & craft', icon: 'edit', reason: 'Sample fact: your demo workspace includes a weekly work journal.', date: 'Tuesday evening', reacted: false },
            { id: 'f2', title: 'Leave a little space in the itinerary', body: 'A weekend can turn into another task list surprisingly quickly. Try giving each day one anchor — a walk, a market, a meal — and letting the rest stay open.\n\nThe plan is still useful. It just makes room for finding something you did not know to look for.', kicker: 'Room to wander', category: 'Everyday life', icon: 'leaf', reason: 'Sample fact: you prefer an unhurried trip.', date: 'Tuesday evening', reacted: false },
            { id: 'f3', title: 'The value of a clear stopping point', body: 'A small project feels lighter when “done” is something you can point to. A saved document. A checked result. A decision made with all the terms in front of you.\n\nWrite the stopping point before you start. It can keep an interesting idea from becoming an endless obligation.', kicker: 'Worth keeping', category: 'Notes & ideas', icon: 'goal', reason: 'Sample fact: your workspace has an acceptance checklist.', date: 'Monday morning', reacted: false },
        ],
        connectors: [
            { id: 'browser', name: 'Browser', icon: 'globe', state: 'connected', scope: 'Approved sites only · purchases always ask' },
            { id: 'github', name: 'GitHub', icon: 'code', state: 'connected', scope: 'Read repositories · changes need approval' },
            { id: 'gmail', name: 'Gmail', icon: 'chat', state: 'available', scope: 'Read and search · send only with approval' },
            { id: 'calendar', name: 'Google Calendar', icon: 'clock', state: 'available', scope: 'Read events · changes need approval' },
            { id: 'contacts', name: 'Google Contacts', icon: 'people', state: 'available', scope: 'Read selected contact details' },
            { id: 'instagram', name: 'Instagram', icon: 'image', state: 'available', scope: 'Read selected account data' },
            { id: 'messenger-companion', name: 'Messenger companion', icon: 'chat', state: 'available', scope: 'Read and search personal account · every write asks' },
        ],
        channels: [
            { id: 'whatsapp', name: 'WhatsApp', state: 'linked', polling: true, interval: '~1 second', greetingSent: true, chatId: 'chat-whatsapp', lastEvent: 'Listening for your messages' },
            { id: 'messenger', name: 'Messenger', state: 'linked', polling: false, interval: '~3 seconds', greetingSent: true, chatId: 'chat-messenger', lastEvent: 'Delivery paused — listening worker stopped' },
            { id: 'telegram', name: 'Telegram', state: 'unavailable', polling: false, interval: 'Not active', greetingSent: false, chatId: 'chat-telegram', lastEvent: 'Temporarily unavailable. Try again later.' },
        ],
        providers: [
            { id: 'wallet', name: 'Connected wallet', state: 'connected', methods: [{ id: 'visa', brand: 'Visa', last4: '4242', provider: 'Connected wallet', fee: 0 }, { id: 'mastercard', brand: 'Mastercard', last4: '8210', provider: 'Connected wallet', fee: 0 }] },
            { id: 'virtual', name: 'Virtual card', state: 'not_connected', methods: [] },
            { id: 'shop', name: 'Shop Pay', state: 'reauth_required', methods: [] },
        ],
        purchases: [],
        browserTasks: [
            { id: 'browser-idle', title: 'Check the sample weekend bag', site: 'Sample Supply', step: 'Comparing the available bag color options', statusTitle: 'Comparing options', state: 'assigned', controller: 'agent', attemptsRemaining: 3, constraints: ['At most one order', 'Always review the full price', 'Stop at any account or security change'], granted: [], credentialReference: null, challenge: 'none', continuedFrom: null, blocker: '', lastConfirmedState: 'Product detail page; nothing submitted.' },
            { id: 'browser-queued', title: 'Check sample opening hours', site: 'Harbor Bookshop', step: 'Waiting for the shared browser slot', statusTitle: 'Waiting for browser', state: 'queued', controller: 'agent', attemptsRemaining: 2, constraints: ['Read only', 'No account access'], granted: [], credentialReference: null, challenge: 'none', continuedFrom: null, blocker: '', lastConfirmedState: 'Not started.' },
        ],
        devices: [
            { id: 'desktop', name: 'This computer', type: 'computer', reachable: true, capabilities: ['Show notification', 'Open sample document'], lastSeen: 'Available now', origin: true },
            { id: 'phone', name: 'Sample iPhone', type: 'phone', reachable: false, capabilities: ['Show notification', 'Sync camera roll'], lastSeen: 'Last seen today, 8:42 am', origin: false },
        ],
        approvalHistory: [],
        permissions: [
            { id: 'connectors', name: 'Connectors', mode: 'ask-for-some', count: 2, description: 'Reading is allowed in connected scopes. Sending and changes ask first.' },
            { id: 'websites', name: 'Websites', mode: 'ask-for-some', count: 2, description: 'Only the allowlist. Sensitive or binding actions always ask.' },
            { id: 'artifacts', name: 'Artifacts', mode: 'ask-for-some', count: 0, description: 'Every publish and public-link update needs a fresh approval.' },
            { id: 'scheduled', name: 'Scheduled work', mode: 'always-ask', count: 3, description: 'A one-time yes never authorizes recurrence.' },
            { id: 'payments', name: 'Payments', mode: 'always-ask', count: 0, description: 'Exact quote only. An unknown outcome is never retried.' },
        ],
        memory: [
            { id: 'mem1', kind: 'profile', title: 'Make plans with room to breathe', body: 'Sample preference: keep one afternoon open on a short trip.', date: 'Sep 23, 2026', closeness: 0 },
            { id: 'mem2', kind: 'profile', title: 'Clear, direct communication', body: 'Sample preference: short, useful updates and no unnecessary interruptions.', date: 'Sep 22, 2026', closeness: 0 },
            { id: 'mem3', kind: 'people', title: 'Alex · friend', body: 'Fictional example person. Enjoys walking and independent bookshops. No contact details are stored.', date: 'Sep 22, 2026', closeness: 1 },
            { id: 'mem4', kind: 'people', title: 'Sam · project partner', body: 'Fictional example person. Prefers a short written handoff before the weekly review.', date: 'Sep 20, 2026', closeness: 2 },
            { id: 'mem5', kind: 'memory', title: 'A useful way to start again', body: 'Sample reflection: a short handoff with an explicit next step makes returning to a project easier.', date: 'Sep 23, 2026', closeness: 0 },
            { id: 'mem6', kind: 'preferences', title: 'Ask before committing', body: 'Show the full terms before purchases, publishing, messages, and account changes.', date: 'Sep 22, 2026', closeness: 0 },
            { id: 'mem7', kind: 'logs', title: 'Wednesday, September 23', body: '09:12 — Saved a sample planning preference.\n09:24 — Recorded a project handoff.\n09:38 — Saved the weekend itinerary.\n\nAppend-only source log. No passwords, codes, or payment values.', date: 'Sep 23, 2026', closeness: 0 },
        ],
        jobs: [
            { id: 'job1', title: 'Your morning edition', kind: 'time', schedule: 'Every day at 8:30 am', recurring: true, limit: 'One edition per day; wait for a quiet moment', source: '', event: '', goalId: null, health: 'ok', lastRun: 'Today, 8:30 am', nextRun: 'Tomorrow, 8:30 am' },
            { id: 'job2', title: 'The weekly work recap', kind: 'time', schedule: 'Fridays at 4 pm', recurring: true, limit: 'One edition per week', source: '', event: '', goalId: 'g4', health: 'ok', lastRun: 'Sep 18, 4 pm', nextRun: 'Sep 25, 4 pm' },
            { id: 'job3', title: 'A useful project change', kind: 'hook', schedule: 'When a selected project file changes', recurring: true, limit: 'Only meaningful changes; at most once per hour', source: 'Workspace files', event: 'Document updated', goalId: null, health: 'disabled', lastRun: 'Sep 22, 3:12 pm', nextRun: 'Disabled after the source became unavailable' },
        ],
        notifications: [
            { id: 'n1', title: 'Your morning edition', text: 'Your weekend plan and project handoff are ready. Two useful updates, with the details attached.', time: '8:30 am', read: false, urgent: false, evidenceId: 't-digest', offer: null, stage: 'delivered' },
            { id: 'n2', title: 'A smaller next step', text: 'Your acceptance checklist is ready to review. Shall I bring it into the main chat?', time: '9:24 am', read: false, urgent: false, evidenceId: 't-handoff', offer: 'Bring it into chat', stage: 'delivered' },
            { id: 'n3', title: 'A photo worth keeping', text: 'One sample camera-roll image could make a useful cover for your weekend guide.', time: '9:35 am', read: false, urgent: false, evidenceId: null, offer: 'Review the image', stage: 'candidate' },
        ],
        settings: { interfaceSize: 'comfortable', theme: 'dark', accent: 'white', avatarAccent: '#a78c57', keepChatPanelVisible: true, language: 'English', name: 'hatchling', vibe: 'warm, direct, a little playful; co-engineer energy', feedPrompt: 'Make me a feed about my interests. Keep the tone clear and direct. Ensure it is quick to skim. Try to avoid clickbait.', captchaStanding: false, telemetry: false, quietStart: '22:00', quietEnd: '08:00', digestTime: '08:30', dailyAllowance: 3, allowlist: ['Sample Supply', 'Harbor Bookshop'] },
        spendingLimit: 200, spentToday: 0, secureItems: ['Sample Supply · protected credential reference'],
    };
    const fixtures = applySettingsFixtures(applyActivityFixtures(applySurfaceFixtures(applyWorkspaceFixtures(state))));
    for (const [index, chat] of fixtures.chats.entries())
        chat.updatedAt = new Date(Date.parse(fixtures.clock) - (index === 1 ? 35 : index * 7) * 60000).toISOString();
    return fixtures;
}
