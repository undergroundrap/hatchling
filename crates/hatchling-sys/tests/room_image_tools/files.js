// Files, processes and a worker, as a script of an agent's might use them.
const fs = require('fs');
const cp = require('child_process');
const zlib = require('zlib');
const crypto = require('crypto');
const os = require('os');
const path = require('path');
const { Worker } = require('worker_threads');

(async () => {
  fs.mkdirSync('nd/a', { recursive: true });
  fs.writeFileSync('nd/a/x.txt', 'hi');
  fs.cpSync('nd', 'nd2', { recursive: true });
  fs.renameSync('nd2', 'nd3');
  fs.copyFileSync('numbers.txt', 'n-copy.txt');
  fs.symlinkSync('x.txt', 'nd/a/l');
  console.log(fs.readdirSync('.').length > 0, fs.statSync('nd').isDirectory(), fs.readFileSync('letters.txt', 'utf8').length);
  console.log(cp.execSync('echo a | tr a b').toString().trim(), cp.spawnSync('true').status);
  console.log(zlib.gzipSync('x').length, crypto.createHash('sha256').update('x').digest('hex').slice(0, 8), crypto.randomBytes(8).length);
  const d = fs.mkdtempSync(path.join(os.tmpdir(), 'n-'));
  await fs.promises.writeFile(path.join(d, 'a'), 'a');
  console.log(await fs.promises.readFile(path.join(d, 'a'), 'utf8'), os.cpus().length > 0, os.totalmem() > 0);
  fs.rmSync(d, { recursive: true });
  fs.rmSync('nd3', { recursive: true });
  await new Promise((resolve) => setTimeout(resolve, 20));
  await new Promise((resolve, reject) => {
    const worker = new Worker('require("worker_threads").parentPort.postMessage(1)', { eval: true });
    worker.on('message', resolve);
    worker.on('error', reject);
  });
  await new Promise((resolve, reject) => {
    const child = cp.spawn('sh', ['-c', 'echo child']);
    child.stdout.on('data', () => {});
    child.on('error', reject);
    child.on('close', (code) => (code === 0 ? resolve() : reject(new Error(`child exit ${code}`))));
  });
  console.log(JSON.stringify({ ok: true }));
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
