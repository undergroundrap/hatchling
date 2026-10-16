// The Machine view's entry: started from the address `hatchling-host` printed,
// with the operator token in its fragment. Without one, it says where to get it.
import { operatorFromPlace } from './adapters/operator/launch.js';
import { render, withoutToken } from './machine/view.js';
const root = document.getElementById('app');
if (!root)
    throw new Error('The Machine view\'s mounting element is missing.');
const client = operatorFromPlace(location, address => { history.replaceState(null, '', address); }, machine => { render(root, machine); });
if (client === undefined)
    root.replaceChildren(withoutToken());
else {
    render(root, client.machine);
    window.addEventListener('pagehide', () => { client.stop(); }, { once: true });
}
