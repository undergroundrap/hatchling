// The frontend protocol handshake: version skew is a user-facing
// event with a sentence a person can act on, never a bare refused connection.
import { INCOMPATIBLE_BACKEND_NEWER, INCOMPATIBLE_FRONTEND_NEWER, UI_SCHEMA_VERSION } from './generated.js';
import type { UiRequest, UiResponse } from './generated.js';
/** Where the handshake leaves the connection. */
export type Handshake = {
    readonly status: 'open';
    readonly sessionProtocol: number;
} | {
    readonly status: 'incompatible';
    readonly frontend: number;
    readonly backend: number;
    readonly message: string;
} | {
    readonly status: 'refused';
    readonly message: string;
};
/**
 * The sentence for a version mismatch, from the newer side's template. The
 * templates come from the schema, so the backend says exactly the same words.
 */
export function incompatibilityMessage(frontend: number, backend: number): string {
    const template = frontend > backend ? INCOMPATIBLE_FRONTEND_NEWER : INCOMPATIBLE_BACKEND_NEWER;
    return template.replaceAll('{frontend}', String(frontend)).replaceAll('{backend}', String(backend));
}
/**
 * The first frame this frontend sends. `token` is the UI token `hatchling-host`
 * gave the page, which the broker asks of every UI connection.
 */
export function hello(client: string, schemaVersion: number = UI_SCHEMA_VERSION, token?: string): UiRequest {
    return token === undefined ? { type: 'hello', schema_version: schemaVersion, client } : { type: 'hello', schema_version: schemaVersion, client, token };
}
/**
 * Read the backend's answer to `hello`. A welcome at another version is a
 * mismatch too: an older broker that does not know to refuse still gets the
 * user an explanation, from this side's own copy of the templates.
 */
export function readHandshake(response: UiResponse, frontend: number = UI_SCHEMA_VERSION): Handshake {
    switch (response.type) {
        case 'welcome':
            if (response.schema_version === frontend)
                return { status: 'open', sessionProtocol: response.session_protocol };
            return { status: 'incompatible', frontend, backend: response.schema_version, message: incompatibilityMessage(frontend, response.schema_version) };
        case 'incompatible':
            return { status: 'incompatible', frontend: response.frontend, backend: response.backend, message: response.message };
        case 'event':
        case 'action_result':
            return { status: 'refused', message: `The backend sent "${response.type}" before the handshake, so it is not a Hatchling broker speaking frontend protocol v${frontend}.` };
    }
}
