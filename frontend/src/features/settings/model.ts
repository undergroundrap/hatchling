// The Model section of Settings: the model Hatchling uses, how it fits this
// machine, and the models its server has to choose from. A choice is used from
// the next message on, with no file to edit and no restart. The server stays
// the operator's: only the model's name is chosen here.
import type { AppContext } from '../../app/context.js';
import { h, valueOf } from '../../components/dom.js';
import { button, field, selectField } from '../../components/ui.js';
import type { ModelPort, ModelView } from '../../session/model.js';
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
        return 'The model Hatchling was installed with.';
    return model.configured === null ? 'You chose it.' : `You chose it; Hatchling was installed with ${model.configured}.`;
}
/** The choice: the server's models when it lists them, else a model's id to type. */
function chooser(ctx: AppContext, port: ModelPort, model: ModelView, refresh: () => void): HTMLElement {
    const current = inUse(model) ?? '';
    const options = (model.available ?? []).map(id => ({ value: id, label: id }));
    const input = model.available !== null && options.length > 0
        ? selectField('Model', 'model', options.some(option => option.value === current) ? options : [{ value: current, label: `${current} (not on the server)` }, ...options], current)
        : field('Model', 'model', current, { placeholder: 'The model’s id on its server' });
    const form = h('form', { class: 'stack model-choice', 'aria-label': 'Choose the model' }, input, h('div', { class: 'feed-prompt-actions' }, h('button', { class: 'button primary', type: 'submit' }, 'Use this model')));
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
        group.append(h('div', { class: 'split-row' }, h('span', { class: 'row-title' }, inUse(model) ?? 'No model is set'), h('span', { class: 'muted' }, model.endpoint ?? '')), h('p', {}, sourceLine(model)), h('p', {}, fitLine(model)), chooser(ctx, port, model, refresh), h('p', { class: 'muted text-small' }, model.available === null
            ? 'Its server doesn’t list its models here: type the id it gives the model.'
            : 'These are the models on its server. To add one, fetch it there (for Ollama, ollama pull), then refresh.'));
        const actions = h('div', { class: 'feed-prompt-actions' }, button('Refresh', () => { void port.refresh().then(refresh); }));
        if (model.chosen !== null && model.configured !== null)
            actions.append(button(`Back to ${model.configured}`, () => {
                void port.choose(null).then((result) => {
                    ctx.toast(result.message, !result.ok);
                    refresh();
                });
            }, { variant: 'link' }));
        group.append(actions);
    }
    return h('section', { class: 'general-section' }, h('h3', {}, 'Model'), group);
}
