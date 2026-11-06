// What a page says when Hatchling's host served it with no key: this address
// alone can't reach your agent, and a made-up workspace mustn't stand in for it.
import { h } from '../components/dom.js';
/** The notice for a host that says the vault is locked, or isn't. */
export function addressNotice(locked: boolean): HTMLElement {
    const steps = locked ? ['sudo hatchling-broker unlock', 'hatchling-host url'] : ['hatchling-host url'];
    return h('main', { class: 'address-notice' }, h('h1', {}, locked ? 'Hatchling is locked' : 'This address needs its key'), h('p', {}, 'In a terminal on this machine, run this, then open the address it gives:'), ...steps.map((step) => h('code', {}, step)));
}
