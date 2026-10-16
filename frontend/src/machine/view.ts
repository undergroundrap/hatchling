// The Machine view: what runs on the broker's machine, for the people who run
// it. Read-only, and metadata only: the broker, the cells running, the API and
// UI connections open, and the recent egress decisions. Every value is placed
// as text; a destination is a cell's own words.
import type { Machine } from '../adapters/operator/client.js';
import { h, replacePreservingFocus } from '../components/dom.js';
import { clock, kind, limits, socket, uptime, verdict } from './format.js';
/** What the status line says about the connection. */
export function statusLine(machine: Machine): string {
    switch (machine.connection) {
        case 'connecting':
            return 'Connecting to Hatchling…';
        case 'live':
            return 'Live, refreshed every 2 seconds.';
        case 'ended':
            return `Disconnected. ${machine.detail}`;
    }
}
/** A section with its heading, its count, and a table or an empty state. */
function section(id: string, title: string, count: number, empty: string, head: string[], rows: HTMLElement[][]): HTMLElement {
    const body = rows.length === 0
        ? h('p', { className: 'machine-empty' }, empty)
        : h('div', { className: 'machine-table-wrap', 'data-scroll': id }, h('table', { className: 'machine-table', 'aria-labelledby': `${id}-title` }, h('thead', {}, h('tr', {}, ...head.map(label => h('th', { scope: 'col' }, label)))), h('tbody', {}, ...rows.map(cells => h('tr', {}, ...cells)))));
    return h('section', { className: 'machine-section', 'aria-labelledby': `${id}-title` }, h('h2', { id: `${id}-title` }, title, ' ', h('span', { className: 'machine-count' }, String(count))), body);
}
const cell = (text: string, className?: string): HTMLElement => h('td', className === undefined ? {} : { className }, text);
/** The view of `machine`. */
export function machineView(machine: Machine): HTMLElement {
    const broker = machine.broker;
    const facts = broker === null
        ? h('p', { className: 'machine-empty' }, 'Waiting for the broker’s answer.')
        : h('dl', { className: 'machine-facts' }, h('dt', {}, 'Version'), h('dd', {}, broker.version), h('dt', {}, 'Started'), h('dd', {}, clock(broker.startedAt)), h('dt', {}, 'Running for'), h('dd', {}, uptime(broker.uptimeSecs)));
    const cells = machine.cells.map(running => [cell(running.cell_id, 'machine-id'), cell(kind(running.kind)), cell(clock(running.started_at)), cell(limits(running.limits).join(', ') || 'The broker’s own bounds')]);
    const sessions = machine.sessions.map(open => [cell(open.connection, 'machine-id'), cell(socket(open.socket)), cell(clock(open.opened_at))]);
    const egress = [...machine.egress].reverse().map(decision => {
        const said = verdict(decision.verdict);
        return [cell(clock(decision.at)), cell(decision.cell_id, 'machine-id'), cell(decision.destination, 'machine-id'), cell(said.label, `machine-verdict machine-${said.tone}`)];
    });
    return h('main', { className: 'machine', id: 'main' }, h('header', { className: 'machine-header' }, h('h1', {}, 'Machine'), h('p', { className: `machine-status machine-${machine.connection}`, role: 'status' }, statusLine(machine))), h('section', { className: 'machine-section', 'aria-labelledby': 'machine-broker-title' }, h('h2', { id: 'machine-broker-title' }, 'Broker'), facts), section('machine-cells', 'Cells running', machine.cells.length, 'No cells are running.', ['Cell', 'Kind', 'Started', 'Limits'], cells), section('machine-sessions', 'Connections', machine.sessions.length, 'No API or app connections are open.', ['Connection', 'Socket', 'Opened'], sessions), section('machine-egress', 'Egress decisions', machine.egress.length, 'No egress decisions yet.', ['Time', 'Cell', 'Destination', 'Decision'], egress));
}
/** The page shown when it was opened without an operator token. */
export function withoutToken(): HTMLElement {
    return h('main', { className: 'machine', id: 'main' }, h('header', { className: 'machine-header' }, h('h1', {}, 'Machine')), h('p', { className: 'machine-empty' }, 'Open the Machine view from the address hatchling-host printed: it carries the operator token this page needs.'));
}
/** Shows `machine` in `root`, keeping focus and scroll. */
export function render(root: HTMLElement, machine: Machine): void {
    replacePreservingFocus(root, machineView(machine));
}
