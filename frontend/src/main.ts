import { installAgentControl } from './automation/bridge.js';
import { askTheHost } from './adapters/live/client.js';
import { liveFromPlace, tabTokens } from './adapters/live/launch.js';
import { createBrowserAdapter } from './adapters/scripted-adapter.js';
import { addressNotice } from './app/address-notice.js';
import { App } from './app/controller.js';
const root = document.getElementById('app');
const dialogs = document.getElementById('dialog-root');
const toasts = document.getElementById('toast-root');
if (!root || !dialogs || !toasts)
    throw new Error('The application mounting elements are missing.');
// Live when `hatchling-host` served the page with a UI token, or this tab kept
// one from an earlier load; scripted otherwise.
const live = liveFromPlace(location, address => { history.replaceState(null, '', address); }, tabTokens());
const adapter = live ?? createBrowserAdapter();
const app = new App(adapter, root, dialogs, toasts);
const disposeAgentControl = installAgentControl(app);
window.addEventListener('pagehide', () => { disposeAgentControl(); app.dispose(); }, { once: true });
// A real install opened with no key: say so, rather than show the demo.
if (live === undefined) {
    void askTheHost().then((state) => {
        if (state !== 'no-host')
            root.replaceChildren(addressNotice(state === 'locked'));
    });
}
