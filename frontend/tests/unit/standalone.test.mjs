// The standalone page names its modules and inlines its assets by paths
// joined with `/`, whatever system built it (#529).
import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
const SCRIPT = resolve('scripts/standalone.mjs');
const walk = (dir, prefix = '') => readdirSync(dir, { withFileTypes: true }).flatMap(entry => entry.isDirectory() ? walk(join(dir, entry.name), `${prefix}${entry.name}/`) : [`${prefix}${entry.name}`]);
const moduleIds = html => [...html.matchAll(/("[^"\n]+"): function\(module, exports, require\) \{$/gm)].map(match => JSON.parse(match[1]));
/** A frontend in a temporary folder: `files` maps each path to its text. */
function tree(t, files) {
    const root = mkdtempSync(join(tmpdir(), 'standalone-'));
    t.after(() => rmSync(root, { recursive: true, force: true }));
    for (const [path, text] of Object.entries(files)) {
        mkdirSync(dirname(join(root, path)), { recursive: true });
        writeFileSync(join(root, path), text);
    }
    return root;
}
const PAGE = '<html><head><link rel="stylesheet" href="./styles/app.css"><link rel="icon" href="./icon.png"></head><body><script type="module" src="./app/main.js"></script></body></html>';
test('modules are named from dist/app with `/`, nested ones too, and assets are inlined', t => {
    const root = tree(t, {
        'index.html': PAGE,
        'src/styles/app.css': 'body { background: url("../media/x.svg"); }',
        'public/icon.png': 'png',
        'public/media/x.svg': '<svg/>',
        'dist/app/main.js': "import { two } from './parts/two.js'; two();",
        'dist/app/parts/two.js': "export const two = () => './media/x.svg';",
    });
    execFileSync(process.execPath, [SCRIPT], { cwd: root, stdio: 'pipe' });
    const html = readFileSync(join(root, 'dist/hatchling-standalone.html'), 'utf8');
    assert.deepEqual(moduleIds(html).sort(), ['main.js', 'parts/two.js']);
    assert.ok(!html.includes('./icon.png') && !html.includes('./media/x.svg'), 'every asset is inlined');
    assert.ok(html.includes(`data:image/png;base64,${Buffer.from('png').toString('base64')}`));
    assert.ok(html.includes(`data:image/svg+xml;base64,${Buffer.from('<svg/>').toString('base64')}`));
});
test('a build with no main.js stops, rather than make a page that never starts', t => {
    const root = tree(t, {
        'index.html': PAGE,
        'src/styles/app.css': 'body {}',
        'public/icon.png': 'png',
        'dist/app/other.js': 'export {};',
    });
    const run = spawnSync(process.execPath, [SCRIPT], { cwd: root, encoding: 'utf8' });
    assert.notEqual(run.status, 0);
    assert.match(run.stderr, /No module is named main\.js: the standalone page would never start\./);
});
test('the built standalone page names every compiled module as its loader asks for it', () => {
    const html = readFileSync('dist/hatchling-standalone.html', 'utf8');
    const ids = moduleIds(html);
    assert.ok(ids.includes('main.js'), 'the module the page starts with');
    assert.deepEqual(ids.filter(id => id.includes('\\') || id.startsWith('dist')), []);
    assert.deepEqual([...ids].sort(), walk('dist/app').filter(path => path.endsWith('.js')).sort());
    for (const asset of walk('public'))
        assert.ok(!html.includes(`./${asset}`), `./${asset} is inlined`);
});
