import { execFileSync } from 'node:child_process';
import { cpSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
rmSync('dist', { recursive: true, force: true });
mkdirSync('dist', { recursive: true });
execFileSync(process.execPath, ['node_modules/typescript/bin/tsc'], { stdio: 'inherit' });
cpSync('index.html', 'dist/index.html');
cpSync('machine.html', 'dist/machine.html');
cpSync('src/styles', 'dist/styles', { recursive: true });
cpSync('public', 'dist', { recursive: true });
const { animationLibrary } = await import('../dist/app/components/animations.js');
mkdirSync('dist/animations', { recursive: true });
for (const [name, animation] of Object.entries(animationLibrary))
    writeFileSync(`dist/animations/${name}.json`, JSON.stringify(animation, null, 2));
execFileSync(process.execPath, ['scripts/standalone.mjs'], { stdio: 'inherit' });
console.log('Built dist/ — static files only. No server-side application.');
