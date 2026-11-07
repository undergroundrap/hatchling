// The Model section of Settings: the model Hatchling uses, where it answers, how
// it fits this machine, and the models its server has to choose from. A choice
// is used from the next message on, with no file to edit and no restart. The
// servers are the operator's to set up: with two, a local one and a hosted one
// say, the owner switches between them here, after saying yes to where their
// words will go.
import type { AppContext } from '../../app/context.js';
import { h, valueOf } from '../../components/dom.js';
import { button, field, selectField } from '../../components/ui.js';
import { hostOf, onThisMachine, whereShort } from '../../session/model.js';
import type { ModelPort, ModelServerView, ModelView } from '../../session/model.js';
/** The model runs call: the owner's choice, else the installed one. */
export function inUse(model: ModelView): string | null {
    return model.chosen ?? model.configured;
}
/** `bytes` as gigabytes, to a tenth. */
function gigabytes(bytes: number): string {
    return `${(bytes / 1e9).toFixed(1)} GB`;
}
/** The model in use, its window and how it fits, in a sentence. */
export function fitLine(model: ModelView): string {
    const window = model.window === null ? 'Its window is known once the model has loaded.' : `A ${model.window.toLocaleString('en')}-token window.`;
    if (model.size === null || model.onCpu === null)
        return window;
    if (model.onCpu === 0)
        return `${window} All on the GPU (${gigabytes(model.size)}).`;
    return `${window} ${gigabytes(model.onCpu)} of its ${gigabytes(model.size)} runs on the CPU, more slowly: free the GPU’s memory, or give it a smaller window.`;
}
/** Where the model in use came from, in a sentence. */
export function sourceLine(model: ModelView): string {
    if (model.chosen === null)
        return model.switched ? 'The model set up on this server.' : 'The model Hatchling was installed with.';
    if (model.configured === null)
        return 'You chose it.';
    return model.switched ? `You chose it; this server was set up with ${model.configured}.` : `You chose it; Hatchling was installed with ${model.configured}.`;
}
/** Where the owner's words go with the model in use, in a sentence. */
export function whereLine(model: ModelView): string {
    if (model.endpoint === null)
        return 'No server is set for it.';
    return onThisMachine(model.endpoint) ? 'It answers on this machine: your words stay here.' : `It answers at ${hostOf(model.endpoint)}: your words go to its provider.`;
}
/** What switching to `other` asks the owner first: a title, and what changes. */
export function switchAsk(other: ModelServerView): {
    title: string;
    description: string;
} {
    const name = other.name ?? 'the other model';
    const kept = 'Your conversation, memory, goals and schedules stay in Hatchling, and you can switch back here any time.';
    if (onThisMachine(other.endpoint))
        return { title: `Switch to ${name}, on this machine?`, description: `From your next message, ${name} answers on this machine, and your words stay here. ${kept}` };
    const host = other.endpoint === null ? 'its server' : hostOf(other.endpoint);
    return { title: `Send your words to ${host}?`, description: `From your next message, what you say to Hatchling, and what it reads for you, goes to ${host} for ${name} to answer, under its provider’s terms. ${kept}` };
}
/** The other server the operator set up, and the switch to it. */
function otherServer(ctx: AppContext, port: ModelPort, other: ModelServerView, switched: boolean, refresh: () => void): HTMLElement {
    const name = other.name ?? 'the other model';
    const ask = switchAsk(other);
    return h('div', { class: 'stack model-switch' }, h('p', {}, `Also set up: ${name}, ${whereShort(other.endpoint)}. You don’t have to pick one for good: switch whenever the work calls for it.`), h('div', { class: 'feed-prompt-actions' }, button(`Switch to ${name}`, () => ctx.dialogs.confirm(ask.title, ask.description, () => {
        void port.switchServer(!switched).then((result) => {
            ctx.toast(result.message, !result.ok);
            refresh();
        });
    }, { label: 'Switch' }))));
}
/** The choice: the server's models when it lists them, else a model's id to type. */
function chooser(ctx: AppContext, port: ModelPort, model: ModelView, refresh: () => void): HTMLElement {
    const current = inUse(model) ?? '';
    const options = (model.available ?? []).map(id => ({ value: id, label: id }));
    const input = model.available !== null && options.length > 0
        ? selectField('Change the model', 'model', options.some(option => option.value === current) ? options : [{ value: current, label: `${current} (not on the server)` }, ...options], current)
        : field('Change the model', 'model', current, { placeholder: 'The model’s id on its server' });
    const form = h('form', { class: 'model-choice', 'aria-label': 'Choose the model' }, input, h('button', { class: 'button primary', type: 'submit' }, 'Use this model'));
    form.addEventListener('submit', (event) => {
        event.preventDefault();
        const name = valueOf(form, 'model').trim();
        if (!name) {
            ctx.toast('Choose a model, or type its id on the server.', true);
            return;
        }
        void port.choose(name === model.configured ? null : name).then((result) => {
            ctx.toast(result.message, !result.ok);
            refresh();
        });
    });
    return form;
}
/** The Model section, from what `ctx`'s model port last read; `refresh` draws Settings again. */
export function liveModel(ctx: AppContext, refresh: () => void): HTMLElement {
    const port = ctx.model;
    const state = port?.current();
    const group = h('div', { class: 'settings-group model-group' });
    if (!port || state === undefined || state.kind === 'unread')
        group.append(h('p', {}, 'Reading your model…'));
    else if (state.kind === 'failed')
        group.append(h('p', {}, `Hatchling couldn’t read the model: ${state.detail}`));
    else {
        const model = state.model;
        group.append(h('div', { class: 'model-head' }, h('span', { class: 'row-title' }, inUse(model) ?? 'No model is set'), ...(model.endpoint === null ? [] : [h('code', { class: 'model-endpoint' }, model.endpoint)])), h('p', { class: 'model-facts' }, `${sourceLine(model)} ${whereLine(model)} ${fitLine(model)}`), chooser(ctx, port, model, refresh));
        const hint = h('p', { class: 'model-hint' }, model.available === null
            ? 'Its server doesn’t list its models here: type the id it gives the model.'
            : 'These are the models on its server. To add one, fetch it there (for Ollama, ollama pull), then refresh.');
        const actions = h('div', { class: 'feed-prompt-actions' }, button('Refresh', () => { void port.refresh().then(refresh); }));
        if (model.chosen !== null && model.configured !== null)
            actions.append(button(`Back to ${model.configured}`, () => {
                void port.choose(null).then((result) => {
                    ctx.toast(result.message, !result.ok);
                    refresh();
                });
            }, { variant: 'link' }));
        group.append(h('div', { class: 'model-foot' }, hint, actions));
        if (model.other !== null)
            group.append(otherServer(ctx, port, model.other, model.switched, refresh));
    }
    return h('section', { class: 'general-section' }, h('h3', {}, 'Model'), group);
}
