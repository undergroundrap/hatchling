// How the Machine view words what the broker answered.
import test from 'node:test';
import assert from 'node:assert/strict';
import { clock, cores, kind, limits, size, socket, uptime, verdict } from '../../dist/app/machine/format.js';
test('uptime is coarse: minutes, hours, then days', () => {
    assert.equal(uptime(0), 'under a minute');
    assert.equal(uptime(59), 'under a minute');
    assert.equal(uptime(60), '1 min');
    assert.equal(uptime(3599), '59 min');
    assert.equal(uptime(3600 + 3 * 60), '1 h 03 min');
    assert.equal(uptime(86399), '23 h 59 min');
    assert.equal(uptime(86400 * 2 + 3600 * 4), '2 d 4 h');
});
test('sizes take the largest unit they fill', () => {
    assert.equal(size(0), '0 bytes');
    assert.equal(size(1023), '1023 bytes');
    assert.equal(size(1024), '1 KiB');
    assert.equal(size(1536), '1.5 KiB');
    assert.equal(size(1073741824), '1 GiB');
    assert.equal(size(1024 ** 5 * 3), '3072 TiB');
});
test('CPU is in cores', () => {
    assert.equal(cores(1000), '1 core');
    assert.equal(cores(500), '0.5 cores');
    assert.equal(cores(2250), '2.25 cores');
});
test('limits are worded, and unset ones left out', () => {
    assert.deepEqual(limits({ processes: 4096, address_space: 1024, file_size: 2048, open_files: 8192, memory: 1073741824, cpu_millis: 500 }), ['4096 processes', '1 GiB memory', '0.5 cores', '8192 open files', '2 KiB per file', '1 KiB address space']);
    assert.deepEqual(limits({ processes: null, address_space: null, file_size: null, open_files: null, memory: null, cpu_millis: null }), []);
    assert.deepEqual(limits({}), []);
});
test('kinds, sockets and verdicts have their words', () => {
    assert.deepEqual(['agent', 'run', 'subagent', 'skill', 'harness', 'browser'].map(kind), ['Agent', 'Run', 'Subagent', 'Skill', 'Coding CLI', 'Browser']);
    assert.deepEqual(['api', 'ui'].map(socket), ['Local API', 'App']);
    assert.deepEqual(verdict('allowed'), { label: 'Allowed', tone: 'success' });
    assert.deepEqual(verdict('failed'), { label: 'Failed', tone: 'attention' });
    for (const refused of ['no_grant', 'not_allowed', 'bad_surrogate', 'policy'])
        assert.equal(verdict(refused).tone, 'danger', refused);
});
test('a timestamp shows its time of day in UTC, or itself when it isn\'t one', () => {
    assert.equal(clock('2026-10-16T09:05:07.123456Z'), '09:05:07 UTC');
    assert.equal(clock('2026-10-16T09:05:07+00:00'), '09:05:07 UTC');
    assert.equal(clock('yesterday'), 'yesterday');
});
