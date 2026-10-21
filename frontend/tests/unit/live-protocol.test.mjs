import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { hello, incompatibilityMessage, readHandshake } from '../../dist/app/adapters/live/protocol/version.js';
import { EventLedger } from '../../dist/app/adapters/live/protocol/reconcile.js';
import { SESSION_PROTOCOL_VERSION, UI_SCHEMA_VERSION } from '../../dist/app/adapters/live/protocol/generated.js';
import { OUTPUT_PATH, SCHEMA_PATH, WIRE_PATH, WIRE_ROOTS, format, generate, typeOf, wire } from '../../scripts/generate-protocol.mjs';
import { WIRE_DEFINITIONS } from '../../dist/app/adapters/live/protocol/wire.js';
const schema = () => JSON.parse(readFileSync(new URL(`../../${SCHEMA_PATH}`, import.meta.url), 'utf8'));
const at = (seq, body) => ({ seq, ts: `2026-10-07T02:00:${String(seq).padStart(2, '0')}Z`, body });
const request = (id, revision) => at(0, { kind: 'confirmation', confirmation_id: id, revision, chat_id: 'main', summary: `do ${id}`, expires_at: null, digest: `digest-${id}-${revision}`, fields: [] }).body;
const offer = (seq, id, revision = 1) => at(seq, request(id, revision));
const decided = (seq, id, decision = 'approve', revision = 1) => at(seq, { kind: 'confirmation_decided', confirmation_id: id, revision, decision });
const chat = seq => at(seq, { kind: 'chat_message', chat_id: 'main', message_id: `m${seq}`, author: 'agent', text: '<script>x</script>' });
test('the mismatch sentence names the side to update, exactly as the backend words it', () => {
    // The same literals are pinned by hatchling-core's ui tests.
    assert.equal(incompatibilityMessage(3, 2), 'frontend v3 needs broker ≥ v3; broker is v2 — restart the broker to update');
    assert.equal(incompatibilityMessage(2, 3), 'broker v3 needs frontend ≥ v3; frontend is v2 — reload the frontend to update');
});
test('hello carries this build\'s protocol version', () => {
    assert.deepEqual(hello('web'), { type: 'hello', schema_version: UI_SCHEMA_VERSION, client: 'web' });
    assert.deepEqual(hello('test', 9), { type: 'hello', schema_version: 9, client: 'test' });
    assert.deepEqual(hello('web', 9, 'abc'), { type: 'hello', schema_version: 9, client: 'web', token: 'abc' });
    assert.equal(SESSION_PROTOCOL_VERSION, 15);
});
test('a matching welcome opens the connection', () => {
    assert.deepEqual(readHandshake({ type: 'welcome', schema_version: 4, session_protocol: 10 }, 4), { status: 'open', sessionProtocol: 10 });
    assert.deepEqual(readHandshake({ type: 'welcome', schema_version: UI_SCHEMA_VERSION, session_protocol: 10 }), { status: 'open', sessionProtocol: 10 });
});
test('version skew fails loudly with a human sentence, whichever side noticed', () => {
    // An older broker that welcomes anyway: the frontend explains it itself.
    assert.deepEqual(readHandshake({ type: 'welcome', schema_version: 2, session_protocol: 10 }, 3), {
        status: 'incompatible', frontend: 3, backend: 2,
        message: 'frontend v3 needs broker ≥ v3; broker is v2 — restart the broker to update',
    });
    assert.equal(readHandshake({ type: 'welcome', schema_version: 5, session_protocol: 10 }, 3).message, incompatibilityMessage(3, 5));
    // A broker that refused: its sentence is shown as given.
    assert.deepEqual(readHandshake({ type: 'incompatible', frontend: 3, backend: 2, message: 'from the broker' }, 3), {
        status: 'incompatible', frontend: 3, backend: 2, message: 'from the broker',
    });
});
test('any other first frame is a refusal, not a silent misrender', () => {
    const event = readHandshake({ type: 'event', ...chat(1) }, 1);
    assert.equal(event.status, 'refused');
    assert.match(event.message, /sent "event" before the handshake/);
    assert.match(event.message, /frontend protocol v1\./);
    assert.match(readHandshake({ type: 'action_result', request_id: 'r', result: { result: 'refused', reason: 'bad_request' } }, 2).message, /"action_result".*v2/);
});
test('events apply once, in sequence, and the mark is where to resume', () => {
    const ledger = new EventLedger();
    assert.equal(ledger.afterSeq(), null);
    assert.equal(ledger.apply(chat(1)), 'applied');
    assert.equal(ledger.apply(chat(1)), 'duplicate');
    assert.equal(ledger.apply(chat(4)), 'applied');
    assert.equal(ledger.apply(chat(3)), 'duplicate', 'below the mark');
    assert.equal(ledger.afterSeq(), 4);
    // A reconnect replays from the mark: overlap drops, the rest applies.
    assert.deepEqual([3, 4, 5, 6].map(seq => ledger.apply(chat(seq))), ['duplicate', 'duplicate', 'applied', 'applied']);
    assert.equal(ledger.afterSeq(), 6);
    const fromZero = new EventLedger();
    assert.equal(fromZero.apply(chat(0)), 'applied', 'sequence zero is a real event');
    assert.equal(fromZero.afterSeq(), 0);
    assert.equal(fromZero.apply(chat(0)), 'duplicate');
});
test('a decided confirmation is never presented again, however it is replayed', () => {
    const ledger = new EventLedger();
    ledger.apply(offer(1, 'k1'));
    ledger.apply(offer(2, 'k2'));
    assert.deepEqual(ledger.pending().map(item => item.confirmation_id), ['k1', 'k2']);
    ledger.apply(decided(3, 'k1', 'deny'));
    assert.deepEqual(ledger.pending().map(item => item.confirmation_id), ['k2']);
    assert.equal(ledger.decision('k1'), 'deny');
    assert.equal(ledger.decision('k2'), undefined);
    // The request re-sent later (a replay after reconnect, a restarted backend).
    assert.equal(ledger.apply(offer(4, 'k1', 2)), 'applied');
    assert.deepEqual(ledger.pending().map(item => item.confirmation_id), ['k2']);
    assert.equal(ledger.decide('k1', 'approve'), undefined, 'never decided twice');
    // A fresh client replaying the whole history ends in the same place.
    const fresh = new EventLedger();
    for (const event of [offer(1, 'k1'), offer(2, 'k2'), decided(3, 'k1', 'deny'), offer(4, 'k1', 2)])
        fresh.apply(event);
    assert.deepEqual(fresh.pending().map(item => item.confirmation_id), ['k2']);
});
test('a confirmation shows its newest revision, and a decision names it', () => {
    const ledger = new EventLedger();
    ledger.apply(offer(1, 'k1', 2));
    ledger.apply(offer(2, 'k2', 1));
    ledger.apply(offer(3, 'k1', 1));
    assert.equal(ledger.pending()[0].revision, 2, 'an older revision does not replace a newer one');
    ledger.apply(offer(4, 'k1', 3));
    assert.deepEqual(ledger.pending().map(item => [item.confirmation_id, item.revision]), [['k1', 3], ['k2', 1]], 'first-offered order holds');
    assert.deepEqual(ledger.decide('k1', 'approve'), { action: 'decide', confirmation_id: 'k1', revision: 3, digest: 'digest-k1-3', decision: 'approve' });
    assert.equal(ledger.decide('unknown', 'deny'), undefined);
    ledger.apply(offer(5, 'k1', 3));
    assert.equal(ledger.pending()[0].revision, 3, 'the same revision again changes nothing');
});
test('the checked-in wire types are exactly what the schema generates', () => {
    const checkedIn = readFileSync(new URL(`../../${OUTPUT_PATH}`, import.meta.url), 'utf8');
    assert.equal(generate(schema()), checkedIn);
});
test('generation is deterministic: same schema, same bytes, whatever the key order', () => {
    const shuffle = value => {
        if (Array.isArray(value))
            return value.map(shuffle);
        if (value === null || typeof value !== 'object')
            return value;
        return Object.fromEntries(Object.keys(value).reverse().map(key => [key, shuffle(value[key])]));
    };
    const first = generate(schema());
    assert.equal(generate(schema()), first);
    assert.equal(generate(shuffle(schema())), first);
    assert.equal(format(first), first, 'already in the formatter\'s shape');
});
test('every construct the schema uses maps to its TypeScript type', () => {
    assert.equal(typeOf({ type: 'string', const: 'vault' }, '#'), "'vault'");
    assert.equal(typeOf({ const: 3 }, '#'), '3');
    assert.equal(typeOf({ type: ['string', 'null'] }, '#'), 'string | null');
    assert.equal(typeOf({ type: 'integer', format: 'uint64', minimum: 0 }, '#'), 'number');
    assert.equal(typeOf({ type: 'boolean' }, '#'), 'boolean');
    assert.equal(typeOf({ $ref: '#/$defs/Op' }, '#'), 'Op');
    assert.equal(typeOf({ items: { type: 'string' } }, '#'), 'readonly (string)[]');
    assert.equal(typeOf({ prefixItems: [{ type: 'integer' }, { type: 'string' }] }, '#'), 'readonly [number, string]');
    assert.equal(typeOf({ oneOf: [{ const: 'a' }, { const: 'b' }, { const: 'a' }] }, '#'), "'a' | 'b'");
    assert.equal(typeOf({ anyOf: [{ $ref: '#/$defs/X' }, { type: 'null' }] }, '#'), 'X | null');
    assert.equal(typeOf(true, '#'), 'unknown');
    assert.equal(typeOf(false, '#'), 'never');
    assert.equal(typeOf({}, '#'), 'unknown');
    const object = typeOf({ type: 'object', properties: { b: { type: 'string' }, a: { type: 'integer', description: 'Doc */ here.' }, 'x-y': { type: 'boolean' } }, required: ['b'] }, '#');
    assert.match(object, /readonly a\?: number;[\s\S]*readonly b: string;[\s\S]*readonly 'x-y'\?: boolean;/, 'sorted, optional unless required, quoted when not an identifier');
    assert.match(object, /Doc \* \/ here\./, 'a comment cannot close early');
    assert.match(typeOf({ type: 'object', additionalProperties: { type: 'integer' } }, '#'), /readonly \[key: string\]: number;/);
    assert.equal(typeOf({ type: 'object' }, '#'), 'object', 'any object');
    assert.equal(typeOf({ type: 'object', properties: {} }, '#'), 'Readonly<Record<string, never>>', 'an empty one');
    assert.equal(typeOf({ type: 'array' }, '#'), 'readonly unknown[]');
    assert.match(typeOf({ type: 'object', properties: {}, additionalProperties: true }, '#'), /readonly \[key: string\]: unknown;/);
    assert.equal(typeOf({ type: 'object', properties: {}, additionalProperties: false }, '#'), 'Readonly<Record<string, never>>');
    // A flattened enum and an internally tagged newtype variant.
    assert.match(typeOf({ properties: { id: { type: 'string' } }, required: ['id'], oneOf: [{ const: 'a' }, { const: 'b' }] }, '#'), /readonly id: string;\n\} & \('a' \| 'b'\)$/);
    assert.match(typeOf({ $ref: '#/$defs/VaultOp', type: 'object', properties: { kind: { const: 'vault' } }, required: ['kind'] }, '#'), /^VaultOp & \{\n {4}readonly kind: 'vault';\n\}$/);
});
test('a construct the generator does not understand is refused, never guessed', () => {
    const refused = (schemaNode, pattern) => assert.throws(() => typeOf(schemaNode, '#/$defs/T'), pattern);
    refused({ type: 'string', pattern: '^a' }, /#\/\$defs\/T: unsupported schema keyword "pattern"/);
    refused({ allOf: [] }, /unsupported schema keyword "allOf"/);
    refused({ $ref: '#/$defs/X', items: {} }, /\$ref with items is not supported/);
    refused({ $ref: '#/$defs/X', type: 'string', properties: {} }, /\$ref with type, properties is not supported/);
    refused({ $ref: 'other.json#/X' }, /unsupported reference/);
    refused({ oneOf: [] }, /non-empty array/);
    refused({ oneOf: [{}], anyOf: [{}] }, /oneOf and anyOf together/);
    refused({ type: 'object', properties: {}, required: ['missing'] }, /required property "missing" is not declared/);
    refused({ type: 'date' }, /unsupported type "date"/);
    refused({ type: [] }, /type must not be empty/);
    refused({ prefixItems: {} }, /prefixItems must be an array/);
    refused({ const: {} }, /a const must be/);
    refused([], /a schema must be an object or a boolean/);
    assert.throws(() => generate(null), /must be an object/);
    assert.throws(() => generate({ $defs: {} }), /missing x-hatchling/);
    assert.throws(() => generate({ 'x-hatchling': {} }), /missing \$defs/);
    const meta = { ui_schema_version: 1, session_protocol: 10, incompatible: { frontend_newer: 'f', backend_newer: 'b' }, control: { transitions: [{ from: 'agent', handoff: 'takeover', to: 'to_user' }] } };
    assert.throws(() => generate({ 'x-hatchling': { ...meta, ui_schema_version: '1' }, $defs: {} }), /versions must be integers/);
    assert.throws(() => generate({ 'x-hatchling': { ...meta, incompatible: { frontend_newer: 'f' } }, $defs: {} }), /both mismatch templates/);
    for (const control of [undefined, {}, { transitions: {} }, { transitions: [{ from: 'agent', handoff: 'takeover' }] }, { transitions: [null] }])
        assert.throws(() => generate({ 'x-hatchling': { ...meta, control }, $defs: {} }), /the control transition table is required/);
    assert.match(generate({ 'x-hatchling': meta, $defs: {} }), /CONTROL_TRANSITIONS: readonly \{[^}]*\}\[\] = \[\{ from: 'agent', handoff: 'takeover', to: 'to_user' \}\];/);
    assert.throws(() => generate({ 'x-hatchling': meta, $defs: { 'not-a-name': {} } }), /must be an identifier/);
    assert.match(generate({ 'x-hatchling': meta, $defs: { Zed: { const: 1 }, Alpha: { type: 'string' } } }), /export type Alpha = string;\nexport type Zed = 1;\n$/);
});
test('the generated types cover the UI protocol and every session family', () => {
    const text = readFileSync(new URL(`../../${OUTPUT_PATH}`, import.meta.url), 'utf8');
    for (const name of ['UiRequest', 'UiResponse', 'UiEvent', 'UiEventBody', 'UiAction', 'UiActionResult', 'ActivityEvent', 'ActivityState', 'SessionRequest', 'SessionResponse', 'DeviceHello'])
        assert.match(text, new RegExp(`^export type ${name} = `, 'm'), name);
    for (const family of ['vault', 'egress', 'channel', 'memory', 'skill', 'goals', 'query', 'schedule', 'hook', 'artifact', 'space', 'subagent'])
        assert.match(text, new RegExp(`readonly kind: '${family}';`), family);
    assert.match(text, /export type ActivityState = 'pending' \| 'success' \| 'partial' \| 'failed' \| 'interrupted';/);
});
test('the checked-in wire table is exactly what the schema generates, deterministically ', () => {
    const checkedIn = readFileSync(new URL(`../../${WIRE_PATH}`, import.meta.url), 'utf8');
    assert.equal(wire(schema()), checkedIn);
    const reversed = value => Array.isArray(value) ? value.map(reversed) : value === null || typeof value !== 'object' ? value : Object.fromEntries(Object.keys(value).reverse().map(key => [key, reversed(value[key])]));
    assert.equal(wire(reversed(schema())), checkedIn);
});
test('the wire table holds what the UI frames reach, without prose, and nothing else', () => {
    const names = Object.keys(WIRE_DEFINITIONS);
    assert.deepEqual(WIRE_ROOTS, ['UiRequest', 'UiResponse']);
    for (const name of ['UiRequest', 'UiResponse', 'UiEvent', 'UiEventBody', 'UiAction', 'UiActionResult', 'ActivityEvent', 'ChatMessage', 'ConfirmationRequest', 'ScheduleOp', 'SkillResult'])
        assert.ok(names.includes(name), name);
    for (const name of ['VaultOp', 'SessionRequest', 'DeviceHello', 'AgentMessage'])
        assert.ok(!names.includes(name), `${name} is not a UI frame`);
    assert.deepEqual(names, [...names].sort());
    const text = JSON.stringify(WIRE_DEFINITIONS);
    // Schema keywords only, never prose (a property may be named "title").
    const keywords = new Set(['type', 'const', 'properties', 'required', '$ref', 'oneOf', 'anyOf', 'items', 'additionalProperties', 'minimum', 'maximum']);
    const walk = node => {
        if (typeof node !== 'object' || node === null)
            return;
        for (const [key, value] of Object.entries(node)) {
            assert.ok(keywords.has(key), `keyword ${key}`);
            if (key === 'properties')
                Object.values(value).forEach(walk);
            else if (key === 'oneOf' || key === 'anyOf')
                value.forEach(walk);
            else if (key === 'items' || key === 'additionalProperties')
                walk(value);
        }
    };
    Object.values(WIRE_DEFINITIONS).forEach(walk);
    // Every reference resolves inside the table.
    for (const [, target] of text.matchAll(/"\$ref":"#\/\$defs\/([A-Za-z_]+)"/g))
        assert.ok(names.includes(target), target);
});
test('a keyword the runtime check does not interpret is refused, never dropped', () => {
    const document = defs => ({ $defs: { UiRequest: { const: 'r' }, UiResponse: { const: 'x' }, ...defs } });
    const bounded = wire(document({ UiResponse: { type: 'integer', minimum: 0, format: 'uint8', description: 'A number.', title: 'T', default: 1 } }));
    assert.ok(bounded.includes('"UiResponse": {\n        "minimum": 0,\n        "type": "integer"\n    }'), bounded);
    assert.match(wire(document({ UiResponse: { type: 'object', properties: { a: { $ref: '#/$defs/A' } }, additionalProperties: { items: true } }, A: { oneOf: [false, { const: 1 }] } })), /"A": \{/);
    assert.throws(() => wire(document({ UiResponse: { allOf: [{ const: 1 }] } })), /#\/\$defs\/UiResponse: the runtime check does not interpret "allOf"/);
    assert.match(wire(document({ UiResponse: { anyOf: [{ $ref: '#/$defs/A' }, { type: 'null' }] }, A: { const: 1 } })), /"anyOf": \[/);
    assert.throws(() => wire(document({ UiResponse: { properties: { a: { type: 'string', pattern: '^a' } } } })), /UiResponse\/properties\/a: the runtime check does not interpret "pattern"/);
    assert.throws(() => wire(document({ UiResponse: { prefixItems: [] } })), /does not interpret "prefixItems"/);
    assert.throws(() => wire(document({ UiResponse: { $ref: '#/$defs/Gone' } })), /#\/\$defs\/Gone: a referenced definition is missing/);
    assert.throws(() => wire(document({ UiResponse: { $ref: 'other.json#/X' } })), /unsupported reference/);
    assert.throws(() => wire(document({ UiResponse: [] })), /a schema must be an object or a boolean/);
    assert.throws(() => wire({}), /missing \$defs/);
    assert.throws(() => wire(null), /missing \$defs/);
});
